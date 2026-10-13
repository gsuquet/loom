//! `loom reindex`: asks loomd to rebuild the tasks index. The output is built in
//! `loom::reindex`.

use std::time::Duration;

use loom::cli::ReindexArgs;
use loom::reindex::{REINDEX_PATH, render_reindex};
use loom_api::ReindexResult;
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Longest wait for a reindex: it reads every task file of every context store.
const REINDEX_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs `loom reindex`, starting loomd when it is not running.
pub fn run(args: &ReindexArgs, layout: &Layout) -> Result<()> {
    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let result: ReindexResult = runtime.block_on(async {
        let client = connect(layout, true).await?;
        client
            .with_timeout(REINDEX_TIMEOUT)
            .post(REINDEX_PATH, None::<&()>)
            .await
            .map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&result).into_diagnostic()? + "\n"
    } else {
        render_reindex(&result)
    };
    print(&text)
}
