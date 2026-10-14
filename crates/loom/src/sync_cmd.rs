//! `loom sync`: asks loomd to sync the context stores with their backup remotes. The output is
//! built in `loom::backup`.

use std::time::Duration;

use loom::backup::{SYNC_PATH, failures, render_sync};
use loom::cli::{GlobalArgs, SyncArgs};
use loom_api::{BackupReport, BackupRequest};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Longest wait for a sync: a fetch and a push of every store, each bounded by loomd.
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs `loom sync`, starting loomd when it is not running. Fails when a store is not synced.
pub fn run(args: &SyncArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let request = BackupRequest {
        context: global.context.clone(),
    };
    let report: BackupReport = runtime.block_on(async {
        let client = connect(layout, true).await?;
        client
            .with_timeout(SYNC_TIMEOUT)
            .post(SYNC_PATH, Some(&request))
            .await
            .map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&report).into_diagnostic()? + "\n"
    } else {
        render_sync(&report)
    };
    print(&text)?;
    match failures(&report) {
        0 => Ok(()),
        1 => Err(miette!(
            "1 context store is not synced: `loom repo doctor` shows how to fix it"
        )),
        count => Err(miette!(
            "{count} context stores are not synced: `loom repo doctor` shows how to fix them"
        )),
    }
}
