//! `loom review sync`: asks loomd to poll the forges for the MRs to review now. The output is
//! built in `loom::review`.

use std::time::Duration;

use loom::cli::{GlobalArgs, ReviewSyncArgs};
use loom::review::{SYNC_PATH, failures, render_sync};
use loom_api::{ReviewSyncRequest, ReviewSyncResult};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Longest wait for a sync: every MR of every host, each call bounded by loomd.
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs `loom review sync`, starting loomd when it is not running. Fails when a poll fails.
pub fn sync(args: &ReviewSyncArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let request = ReviewSyncRequest {
        context: global.context.clone(),
    };
    let result: ReviewSyncResult = runtime.block_on(async {
        let client = connect(layout, true).await?;
        client
            .with_timeout(SYNC_TIMEOUT)
            .post(SYNC_PATH, Some(&request))
            .await
            .map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&result).into_diagnostic()? + "\n"
    } else {
        render_sync(&result)
    };
    print(&text)?;
    match failures(&result) {
        0 => Ok(()),
        count => Err(miette!(
            "{count} forge poll(s) failed: their MRs keep their last snapshot"
        )),
    }
}
