//! `loom reindex`: the output, built without I/O.

use std::fmt::Write as _;

use loom_api::ReindexResult;

/// Path of the reindex endpoint.
pub const REINDEX_PATH: &str = "/v1/tasks/reindex";

/// The text output of `loom reindex`: the count, one line per invalid task file, then one line
/// per task that could not be read.
#[must_use]
pub fn render_reindex(result: &ReindexResult) -> String {
    let noun = |count: usize| if count == 1 { "task" } else { "tasks" };
    let mut text = if result.invalid.is_empty() {
        format!("Indexed {} {}.\n", result.tasks, noun(result.tasks))
    } else {
        format!(
            "Indexed {} {}, {} invalid (read-only until fixed by hand):\n",
            result.tasks,
            noun(result.tasks),
            result.invalid.len()
        )
    };
    for file in &result.invalid {
        let _ = writeln!(text, "  {}/{}: {}", file.context, file.id, file.error);
    }
    if !result.failed.is_empty() {
        let _ = writeln!(
            text,
            "{} {} not read (its previous index entry is kept):",
            result.failed.len(),
            noun(result.failed.len())
        );
    }
    for file in &result.failed {
        let _ = writeln!(text, "  {}: {}", file.path.display(), file.error);
    }
    text
}

#[cfg(test)]
mod tests {
    use loom_api::{FailedTaskFile, InvalidTaskFile, ReindexResult};

    use super::render_reindex;

    #[test]
    fn the_count_then_each_invalid_file() {
        assert_eq!(
            render_reindex(&ReindexResult::default()),
            "Indexed 0 tasks.\n"
        );
        let one = ReindexResult {
            tasks: 1,
            ..ReindexResult::default()
        };
        assert_eq!(render_reindex(&one), "Indexed 1 task.\n");
        let broken = ReindexResult {
            tasks: 12,
            invalid: vec![InvalidTaskFile {
                context: "work".parse().unwrap(),
                id: "k3q7zm2a".parse().unwrap(),
                error: "line 2: invalid TOML: expected `=`".to_owned(),
            }],
            failed: Vec::new(),
        };
        assert_eq!(
            render_reindex(&broken),
            "Indexed 12 tasks, 1 invalid (read-only until fixed by hand):\n  work/k3q7zm2a: line 2: invalid TOML: expected `=`\n"
        );
    }

    #[test]
    fn unreadable_tasks_follow_the_count() {
        let locked = ReindexResult {
            tasks: 2,
            invalid: Vec::new(),
            failed: vec![FailedTaskFile {
                path: "/l/contexts/work/tasks/k3q7zm2a".into(),
                error: "permission denied".to_owned(),
            }],
        };
        assert_eq!(
            render_reindex(&locked),
            "Indexed 2 tasks.\n1 task not read (its previous index entry is kept):\n  /l/contexts/work/tasks/k3q7zm2a: permission denied\n"
        );
    }
}
