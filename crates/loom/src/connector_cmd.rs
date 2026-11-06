//! `loom connector status`: ask loomd what the last polls found. The output is built in
//! `loom::connector`.

use loom::cli::{ConnectorStatusArgs, GlobalArgs};
use loom::connector::{render_status, status_path};
use loom_api::ConnectorList;
use loom_config::layout::Layout;
use miette::{IntoDiagnostic as _, Result};

use crate::print;
use crate::repo_cmd::{api_error, connect};

/// Runs `loom connector status`, starting loomd when it is not running.
pub fn status(args: &ConnectorStatusArgs, global: &GlobalArgs, layout: &Layout) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let path = status_path(global.context.as_ref());
    let list: ConnectorList = runtime.block_on(async {
        let client = connect(layout, true).await?;
        client.get(&path).await.map_err(|error| api_error(&error))
    })?;
    let text = if args.json {
        serde_json::to_string_pretty(&list).into_diagnostic()? + "\n"
    } else {
        render_status(&list)
    };
    print(&text)
}
