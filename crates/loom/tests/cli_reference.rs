//! The generated CLI reference matches the `clap` definitions.
//!
//! `LOOM_UPDATE_DOCS=1` rewrites the files instead (`mise run docs`).

use std::path::PathBuf;

use clap::CommandFactory;
use loom::cli_doc::render;

fn reference(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reference/cli")
        .join(file)
}

fn check(file: &str, rendered: &str) -> std::io::Result<()> {
    let path = reference(file);
    if std::env::var_os("LOOM_UPDATE_DOCS").is_some_and(|value| value == "1") {
        return std::fs::write(&path, rendered);
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "{} is stale: run `mise run docs` and commit the result",
        path.display()
    );
    Ok(())
}

#[test]
fn loom_reference_is_up_to_date() {
    check("loom.md", &render(&loom::cli::Cli::command())).unwrap();
}

#[test]
fn loomd_reference_is_up_to_date() {
    check("loomd.md", &render(&loomd::cli::Cli::command())).unwrap();
}
