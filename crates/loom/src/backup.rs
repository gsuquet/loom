//! `loom sync`: the output, built without I/O.

use std::fmt::Write as _;

use loom_api::BackupReport;

/// Path of the sync endpoint.
pub const SYNC_PATH: &str = "/v1/tasks/sync";
/// Path of the backup state endpoint.
pub const BACKUP_PATH: &str = "/v1/tasks/backup";
/// Path of the task store problems endpoint.
pub const HEALTH_PATH: &str = "/v1/tasks/health";

/// The text output of `loom sync`: one line per context synced, `<context>: <outcome>`, then
/// the uncommitted changes.
#[must_use]
pub fn render_sync(report: &BackupReport) -> String {
    if report.contexts.is_empty() {
        return "No context store has a store.backup.remote.\n".to_owned();
    }
    let mut text = String::new();
    for context in &report.contexts {
        let outcome = context
            .last_sync
            .as_ref()
            .map_or_else(|| "not synced".to_owned(), |sync| sync.outcome.to_string());
        let _ = writeln!(text, "{}: {outcome}", context.context);
        if let Some(error) = &context.uncommitted {
            let _ = writeln!(text, "  uncommitted changes: {error}");
        }
    }
    text
}

/// Number of contexts whose sync did not leave the store and its remote equal, or that have
/// uncommitted changes.
#[must_use]
pub fn failures(report: &BackupReport) -> usize {
    report
        .contexts
        .iter()
        .filter(|context| context.failed())
        .count()
}

#[cfg(test)]
mod tests {
    use loom_api::{BackupOutcome, BackupReport, BackupSync, ContextBackup, Timestamp};

    use super::{failures, render_sync};

    fn context(name: &str, outcome: BackupOutcome, uncommitted: Option<&str>) -> ContextBackup {
        ContextBackup {
            context: name.parse().unwrap(),
            path: format!("/l/contexts/{name}").into(),
            remote: Some("/srv/s.git".to_owned()),
            uncommitted: uncommitted.map(str::to_owned),
            last_sync: Some(BackupSync {
                time: Timestamp::EPOCH,
                outcome,
            }),
        }
    }

    #[test]
    fn one_line_per_context() {
        let report = BackupReport {
            machine: "desk".to_owned(),
            contexts: vec![
                context("home", BackupOutcome::Pushed { commits: 2 }, None),
                context(
                    "work",
                    BackupOutcome::Diverged {
                        local: 1,
                        remote: 2,
                    },
                    Some("git commit failed: exit status: 1"),
                ),
            ],
            not_held: Vec::new(),
        };
        assert_eq!(
            render_sync(&report),
            "home: pushed 2 commits\nwork: diverged: 1 local and 2 remote commits (nothing merged)\n  uncommitted changes: git commit failed: exit status: 1\n"
        );
        assert_eq!(failures(&report), 1);
        let empty = BackupReport {
            contexts: Vec::new(),
            ..report
        };
        assert_eq!(
            render_sync(&empty),
            "No context store has a store.backup.remote.\n"
        );
    }
}
