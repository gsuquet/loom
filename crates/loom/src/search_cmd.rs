//! `loom search`: queries loomd. The request and the output are built in `loom::search`.

use loom::cli::{GlobalArgs, SearchArgs, SearchType};
use loom::search::{render_results, search_api_path};
use loom_api::SearchResults;
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Runs `loom search`, starting loomd when it is not running.
pub fn run(args: &SearchArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let found: SearchResults = runtime.block_on(async {
        let client = connect(layout, true).await?;
        let path = search_api_path(
            &args.query,
            global.context.as_ref(),
            args.kind.map(SearchType::kind),
            args.limit,
        );
        client.get(&path).await.map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&found).into_diagnostic()? + "\n"
    } else {
        render_results(&found.results)
    };
    print(&text)
}
