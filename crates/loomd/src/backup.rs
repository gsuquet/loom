//! Backup of the context stores (ADR-0013, decision 8): what is committed and how commits are
//! named, decided without I/O. `loomd::store_git` runs git.

use std::fmt::Write as _;

use loom_api::{ContextName, TaskId};
use loom_core::{Event, Status};

/// `.gitattributes` written when a store repository is created: `events.jsonl` only grows, so
/// two histories of it merge by keeping both sides' lines.
pub const GITATTRIBUTES: &str = "*.jsonl merge=union\n";

/// `.gitignore` written when a store repository is created: the temporary files of atomic
/// writes (`.task.md.tmp`) and of creations (`tasks/.<id>.tmp/`).
pub const GITIGNORE: &str = ".*.tmp\n";

/// Longest summary of a commit message, in characters, before `…`.
pub const SUMMARY_MAX_CHARS: usize = 60;

/// What a commit takes from the working tree of a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The directory of this task only.
    Task(TaskId),
    /// Every change of the store.
    All,
}

/// What to stage for a change of task `id`.
///
/// Its directory, or every change of the store when an earlier commit failed (its changes are
/// committed with this one) or when the store has no repository yet (its first commit holds
/// every file).
#[must_use]
pub fn scope(id: &TaskId, pending: bool, repository: bool) -> Scope {
    if pending || !repository {
        Scope::All
    } else {
        Scope::Task(id.clone())
    }
}

/// The message of the commit of `events`, recorded on task `id` by one change:
/// `<type>(<id>): <summary>`, the type and summary of the first event, `(+N events)` when there
/// are more. `None` when there is no event.
#[must_use]
pub fn commit_message(id: &TaskId, events: &[&Event]) -> Option<String> {
    let first = events.first()?;
    let (kind, summary) = describe(first);
    let mut summary = one_line(&summary);
    if events.len() > 1 {
        let _ = write!(summary, " (+{} events)", events.len() - 1);
    }
    Some(format!("{kind}({id}): {summary}"))
}

/// The message of a hand edit of task `id` that changed no field (notes, a new or invalid
/// file): `edited(<id>): changed by hand`.
#[must_use]
pub fn hand_edit_message(id: &TaskId) -> String {
    format!("edited({id}): changed by hand")
}

/// The message of the removal of task `id`'s directory: `removed(<id>): task directory removed`.
#[must_use]
pub fn removed_message(id: &TaskId) -> String {
    format!("removed({id}): task directory removed")
}

/// The message of a commit of every change of the store of `context` that no other commit
/// took: `sync(<context>): uncommitted changes`.
#[must_use]
pub fn catch_up_message(context: &ContextName) -> String {
    format!("sync({context}): uncommitted changes")
}

/// Name of the remote loomd sets in each store repository to `store.backup.remote`.
pub const REMOTE: &str = "backup";

/// What a sync does once the remote is fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// Same commits on both sides, or none on either.
    UpToDate,
    /// Push the local commits.
    Push {
        /// Commits only in the store.
        commits: u64,
    },
    /// Fast-forward the store to the remote.
    FastForward {
        /// Commits only on the remote.
        commits: u64,
    },
    /// Both sides have commits the other lacks: stop.
    Diverged {
        /// Commits only in the store.
        local: u64,
        /// Commits only on the remote.
        remote: u64,
    },
}

/// The sides of a sync after the fetch: whether each has a commit, and when both do, the
/// commits only in the store (`ahead`) and only on the remote (`behind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sides {
    /// Neither the store nor the remote has a commit.
    Empty,
    /// Only the store has commits, this many.
    LocalOnly(u64),
    /// Only the remote has commits, this many.
    RemoteOnly(u64),
    /// Both have commits.
    Both {
        /// Commits only in the store.
        ahead: u64,
        /// Commits only on the remote.
        behind: u64,
    },
}

/// What to do with `sides`: push when only the store has new commits, fast-forward when only
/// the remote has, stop when both have. Never merge, rebase or force.
#[must_use]
pub const fn decide_sync(sides: Sides) -> SyncAction {
    match sides {
        Sides::Empty
        | Sides::Both {
            ahead: 0,
            behind: 0,
        } => SyncAction::UpToDate,
        Sides::LocalOnly(commits)
        | Sides::Both {
            ahead: commits,
            behind: 0,
        } => SyncAction::Push { commits },
        Sides::RemoteOnly(commits)
        | Sides::Both {
            ahead: 0,
            behind: commits,
        } => SyncAction::FastForward { commits },
        Sides::Both { ahead, behind } => SyncAction::Diverged {
            local: ahead,
            remote: behind,
        },
    }
}

/// `ahead` and `behind` from the output of `git rev-list --left-right --count A...B`
/// (`<ahead>\t<behind>`).
#[must_use]
pub fn parse_counts(output: &str) -> Option<(u64, u64)> {
    let mut parts = output.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((ahead, behind))
}

/// Whether a failed `git push` was refused by the remote (a non-fast-forward, a hook, a
/// protected branch) rather than not reaching it: git prints `[rejected]` or
/// `[remote rejected]` for the refused ref.
#[must_use]
pub fn push_refused(stderr: &str) -> bool {
    stderr.contains("[rejected]") || stderr.contains("[remote rejected]")
}

/// `remote` with the password of a URL (`scheme://user:password@host`) replaced with `***`:
/// what the API and the doctor show.
#[must_use]
pub fn redact_remote(remote: &str) -> String {
    let Some((scheme, rest)) = remote.split_once("://") else {
        return remote.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((user_info, host)) => match user_info.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:***@{host}{path}"),
            None => remote.to_owned(),
        },
        None => remote.to_owned(),
    }
}

/// The type and summary of `event`.
fn describe(event: &Event) -> (&'static str, String) {
    match event {
        Event::Created { task } => ("created", task.title.clone()),
        Event::Edited { changes } => (
            "edited",
            changes
                .iter()
                .map(loom_core::FieldChange::field)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Event::StatusChanged(change) => (
            "status_changed",
            format!("{} -> {}", status(&change.from), status(&change.to)),
        ),
        Event::NoteLogged { text } => ("note_logged", text.clone()),
        Event::Undone { of, .. } => ("undone", format!("event {of}")),
    }
}

fn status(status: &Status) -> String {
    match status {
        Status::Unknown(name) => name.clone(),
        Status::Inbox
        | Status::Ready
        | Status::Active
        | Status::Waiting { .. }
        | Status::Done
        | Status::Cancelled => status
            .name()
            .map(|name| name.to_string())
            .unwrap_or_default(),
    }
}

/// `text` on one line: whitespace runs (line breaks included) become one space, cut to
/// [`SUMMARY_MAX_CHARS`] characters with `…`.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= SUMMARY_MAX_CHARS {
        return joined;
    }
    let mut cut: String = joined.chars().take(SUMMARY_MAX_CHARS).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use loom_core::{Change, Event, FieldChange, Importance, Status};

    use super::{
        Scope, Sides, SyncAction, catch_up_message, commit_message, decide_sync, hand_edit_message,
        parse_counts, push_refused, redact_remote, removed_message, scope,
    };

    #[test]
    fn a_sync_pushes_or_fast_forwards_and_stops_on_divergence() {
        assert_eq!(decide_sync(Sides::Empty), SyncAction::UpToDate);
        assert_eq!(
            decide_sync(Sides::Both {
                ahead: 0,
                behind: 0
            }),
            SyncAction::UpToDate
        );
        assert_eq!(
            decide_sync(Sides::LocalOnly(3)),
            SyncAction::Push { commits: 3 }
        );
        assert_eq!(
            decide_sync(Sides::Both {
                ahead: 2,
                behind: 0
            }),
            SyncAction::Push { commits: 2 }
        );
        assert_eq!(
            decide_sync(Sides::RemoteOnly(4)),
            SyncAction::FastForward { commits: 4 }
        );
        assert_eq!(
            decide_sync(Sides::Both {
                ahead: 0,
                behind: 1
            }),
            SyncAction::FastForward { commits: 1 }
        );
        assert_eq!(
            decide_sync(Sides::Both {
                ahead: 2,
                behind: 1
            }),
            SyncAction::Diverged {
                local: 2,
                remote: 1
            }
        );
    }

    #[test]
    fn counts_are_read_from_rev_list() {
        assert_eq!(parse_counts("2\t1\n"), Some((2, 1)));
        assert_eq!(parse_counts("0\t0"), Some((0, 0)));
        assert_eq!(parse_counts("2"), None);
        assert_eq!(parse_counts("a\tb"), None);
        assert_eq!(parse_counts("1\t2\t3"), None);
    }

    #[test]
    fn a_refused_push_is_told_from_an_unreachable_remote() {
        assert!(push_refused(
            "To /srv/s.git\n ! [rejected]        HEAD -> main (fetch first)\nerror: failed to push some refs\n"
        ));
        assert!(push_refused(
            " ! [remote rejected] HEAD -> main (pre-receive hook declined)\n"
        ));
        assert!(!push_refused(
            "fatal: unable to access 'https://example.com/s.git/': Could not resolve host\n"
        ));
    }

    #[test]
    fn passwords_of_remote_urls_are_hidden() {
        assert_eq!(
            redact_remote("https://me:s3cret@example.com/store.git"),
            "https://me:***@example.com/store.git"
        );
        assert_eq!(
            redact_remote("https://me@example.com/a@b.git"),
            "https://me@example.com/a@b.git"
        );
        assert_eq!(
            redact_remote("git@example.com:me/store.git"),
            "git@example.com:me/store.git"
        );
        assert_eq!(redact_remote("/srv/store.git"), "/srv/store.git");
    }

    fn id() -> loom_api::TaskId {
        "k3q7zm2a".parse().unwrap()
    }

    fn record(event: Event) -> Event {
        event
    }

    #[test]
    fn a_change_of_a_known_task_takes_its_directory() {
        assert_eq!(scope(&id(), false, true), Scope::Task(id()));
        // An earlier commit failed: its changes go with this one.
        assert_eq!(scope(&id(), true, true), Scope::All);
        // The first commit of a store holds every file.
        assert_eq!(scope(&id(), false, false), Scope::All);
    }

    #[test]
    fn messages_name_the_event_type_the_task_and_a_summary() {
        let status = record(Event::StatusChanged(Change {
            from: Status::Ready,
            to: Status::Done,
        }));
        assert_eq!(
            commit_message(&id(), &[&status]).as_deref(),
            Some("status_changed(k3q7zm2a): ready -> done")
        );
        let edited = record(Event::Edited {
            changes: vec![
                FieldChange::Title(Change {
                    from: "a".to_owned(),
                    to: "b".to_owned(),
                }),
                FieldChange::Importance(Change {
                    from: Importance::P3,
                    to: Importance::P1,
                }),
            ],
        });
        assert_eq!(
            commit_message(&id(), &[&edited, &status]).as_deref(),
            Some("edited(k3q7zm2a): title, importance (+1 events)")
        );
        let undone = record(Event::Undone {
            of: "e0000009".parse().unwrap(),
            changes: Vec::new(),
        });
        assert_eq!(
            commit_message(&id(), &[&undone]).as_deref(),
            Some("undone(k3q7zm2a): event e0000009")
        );
        assert_eq!(commit_message(&id(), &[]), None);
    }

    #[test]
    fn summaries_fit_on_one_short_line() {
        let note = record(Event::NoteLogged {
            text: format!("first line\n\n  second {}", "x".repeat(80)),
        });
        let message = commit_message(&id(), &[&note]).unwrap();
        assert!(!message.contains('\n'), "{message}");
        assert!(
            message.starts_with("note_logged(k3q7zm2a): first line second xxx"),
            "{message}"
        );
        assert!(message.ends_with('…'), "{message}");
        let summary = message.split_once(": ").unwrap().1;
        assert_eq!(summary.chars().count(), super::SUMMARY_MAX_CHARS + 1);
    }

    #[test]
    fn other_changes_have_fixed_messages() {
        assert_eq!(
            hand_edit_message(&id()),
            "edited(k3q7zm2a): changed by hand"
        );
        assert_eq!(
            removed_message(&id()),
            "removed(k3q7zm2a): task directory removed"
        );
        assert_eq!(
            catch_up_message(&"work".parse().unwrap()),
            "sync(work): uncommitted changes"
        );
    }
}
