//! Rules on the relations between tasks.

use std::collections::BTreeSet;
use std::fmt;

use crate::command::DomainError;
use crate::context::ContextName;
use crate::id::TaskId;

/// Why a task cannot have a parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentProblem {
    /// No task has this ID.
    Unknown,
    /// The parent is in another context: a task and its parent share one context.
    OtherContext(ContextName),
    /// The task would be its own ancestor.
    Cycle,
}

impl fmt::Display for ParentProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("no task has this ID"),
            Self::OtherContext(context) => write!(
                f,
                "it is in context {context}: a task and its parent share one context"
            ),
            Self::Cycle => f.write_str("the task would be its own ancestor"),
        }
    }
}

/// What [`check_parent`] needs to know of a task: its context and its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentLink {
    /// Context of the task.
    pub context: ContextName,
    /// Its parent, if any; `None` too when its file cannot be read.
    pub parent: Option<TaskId>,
}

/// Checks that `parent` can be the parent of a task of `context`: `child` for an existing
/// task, `None` for a task being created.
///
/// The parent must be a known task of the same context, and must not be `child` nor have
/// `child` among its ancestors. `lookup` reads a task; a loop already among the ancestors (a
/// hand edit) ends the walk without an error, since it does not involve `child`.
///
/// # Errors
///
/// [`DomainError::InvalidParent`] with the [`ParentProblem`].
pub fn check_parent(
    child: Option<&TaskId>,
    context: &ContextName,
    parent: &TaskId,
    lookup: impl Fn(&TaskId) -> Option<ParentLink>,
) -> Result<(), DomainError> {
    let refuse = |problem| DomainError::InvalidParent {
        parent: parent.clone(),
        problem,
    };
    if child == Some(parent) {
        return Err(refuse(ParentProblem::Cycle));
    }
    let link = lookup(parent).ok_or_else(|| refuse(ParentProblem::Unknown))?;
    if &link.context != context {
        return Err(refuse(ParentProblem::OtherContext(link.context)));
    }
    let Some(child) = child else {
        return Ok(());
    };
    let mut seen = BTreeSet::from([parent.clone()]);
    let mut next = link.parent;
    while let Some(ancestor) = next {
        if &ancestor == child {
            return Err(refuse(ParentProblem::Cycle));
        }
        if !seen.insert(ancestor.clone()) {
            break;
        }
        next = lookup(&ancestor).and_then(|link| link.parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn id(text: &str) -> TaskId {
        text.parse().unwrap()
    }

    fn context(text: &str) -> ContextName {
        text.parse().unwrap()
    }

    /// Tasks `(id, context, parent)`.
    fn tasks(rows: &[(&str, &str, Option<&str>)]) -> impl Fn(&TaskId) -> Option<ParentLink> {
        let map: BTreeMap<TaskId, ParentLink> = rows
            .iter()
            .map(|(task, ctx, parent)| {
                (
                    id(task),
                    ParentLink {
                        context: context(ctx),
                        parent: parent.map(id),
                    },
                )
            })
            .collect();
        move |task| map.get(task).cloned()
    }

    fn problem(result: Result<(), DomainError>) -> ParentProblem {
        match result {
            Err(DomainError::InvalidParent { problem, .. }) => problem,
            other => panic!("not a parent refusal: {other:?}"),
        }
    }

    #[test]
    fn a_new_task_takes_a_parent_of_its_context() {
        let lookup = tasks(&[("aaaaaaaa", "work", None)]);
        assert_eq!(
            check_parent(None, &context("work"), &id("aaaaaaaa"), &lookup),
            Ok(())
        );
        assert_eq!(
            problem(check_parent(
                None,
                &context("home"),
                &id("aaaaaaaa"),
                &lookup
            )),
            ParentProblem::OtherContext(context("work"))
        );
        assert_eq!(
            problem(check_parent(
                None,
                &context("work"),
                &id("bbbbbbbb"),
                &lookup
            )),
            ParentProblem::Unknown
        );
    }

    #[test]
    fn a_task_is_never_its_own_ancestor() {
        // c -> b -> a
        let lookup = tasks(&[
            ("aaaaaaaa", "work", None),
            ("bbbbbbbb", "work", Some("aaaaaaaa")),
            ("cccccccc", "work", Some("bbbbbbbb")),
        ]);
        let work = context("work");
        assert_eq!(
            problem(check_parent(
                Some(&id("aaaaaaaa")),
                &work,
                &id("aaaaaaaa"),
                &lookup
            )),
            ParentProblem::Cycle
        );
        assert_eq!(
            problem(check_parent(
                Some(&id("aaaaaaaa")),
                &work,
                &id("cccccccc"),
                &lookup
            )),
            ParentProblem::Cycle
        );
        assert_eq!(
            check_parent(Some(&id("cccccccc")), &work, &id("aaaaaaaa"), &lookup),
            Ok(())
        );
    }

    #[test]
    fn a_loop_among_the_ancestors_ends_the_walk() {
        // b -> a -> b, written by hand; c takes b as parent.
        let lookup = tasks(&[
            ("aaaaaaaa", "work", Some("bbbbbbbb")),
            ("bbbbbbbb", "work", Some("aaaaaaaa")),
            ("cccccccc", "work", None),
        ]);
        assert_eq!(
            check_parent(
                Some(&id("cccccccc")),
                &context("work"),
                &id("bbbbbbbb"),
                &lookup
            ),
            Ok(())
        );
    }
}
