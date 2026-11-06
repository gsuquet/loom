//! `loom review sync`, `queue` and `start`: ask loomd to poll the forges now, list the MRs that
//! wait on me, and start on one. The output is built in `loom::review`.

use std::time::Duration;

use loom::cli::{GlobalArgs, ReviewQueueArgs, ReviewStartArgs, ReviewSyncArgs};
use loom::connector;
use loom::review::{
    SYNC_PATH, failure_message, queue_path, render_queue, render_start, render_sync, start_path,
};
use loom_api::{
    ConnectorList, ReviewQueue, ReviewStartRequest, ReviewStartResult, ReviewSyncRequest,
    ReviewSyncResult,
};
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result, miette};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Longest wait for a sync: every MR of every host, each call bounded by loomd.
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// Runs `loom review queue`, starting loomd when it is not running.
pub fn queue(args: &ReviewQueueArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let path = queue_path(global.context.as_ref());
    let connectors_path = connector::status_path(global.context.as_ref());
    let want_status = !args.json;
    let (queue, connectors): (ReviewQueue, Option<ConnectorList>) = runtime.block_on(async {
        let client = connect(layout, true).await?;
        let queue: ReviewQueue = client.get(&path).await.map_err(|error| api_error(&error))?;
        // Only an empty queue needs the status. An older loomd, an error or a timeout gives
        // `None`: the answer is then the one of an empty queue, never a failure.
        let connectors = if want_status && queue.entries.is_empty() {
            client.get::<ConnectorList>(&connectors_path).await.ok()
        } else {
            None
        };
        Ok::<_, miette::Report>((queue, connectors))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&queue).into_diagnostic()? + "\n"
    } else {
        render_queue(&queue, connectors.as_ref())
    };
    print(&text)
}

/// Runs `loom review start`, starting loomd when it is not running.
pub fn start(args: &ReviewStartArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let request = ReviewStartRequest {
        context: global.context.clone(),
    };
    let path = start_path(&args.key);
    let result: ReviewStartResult = runtime.block_on(async {
        let client = connect(layout, true).await?;
        // A start waits for a poll that runs, as a sync does.
        client
            .with_timeout(SYNC_TIMEOUT)
            .post(&path, Some(&request))
            .await
            .map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&result).into_diagnostic()? + "\n"
    } else {
        render_start(&result)
    };
    print(&text)
}

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
    failure_message(&result).map_or(Ok(()), |message| Err(miette!("{message}")))
}
