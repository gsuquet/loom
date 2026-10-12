//! Markdown CLI reference generated from `clap` definitions (ADR-0010).

use std::fmt::Write as _;

use clap::{Arg, ArgAction, Command};

/// Renders the reference of `command` and its visible subcommands as markdown.
///
/// The output depends only on the `clap` definition: no version, no date.
#[must_use]
pub fn render(command: &Command) -> String {
    let mut command = command.clone();
    command.build();
    let name = command.get_name().to_owned();
    let mut out = format!(
        "# `{name}` CLI reference\n\n\
         > Generated from the `clap` definitions by `mise run docs`. Do not edit by hand.\n\
         > Conventions shared by every command: [conventions.md](conventions.md).\n\n"
    );
    push_about(&mut out, &command);
    out.push_str("## Usage\n\n");
    push_usage(&mut out, &mut command);
    let subcommands = visible_subcommands(&command);
    let title = if subcommands.is_empty() {
        "Options"
    } else {
        "Global options"
    };
    push_args(&mut out, &command, "###", title, true);
    if !subcommands.is_empty() {
        out.push_str("\n## Commands\n");
    }
    for sub in subcommands {
        if let Some(sub) = command.find_subcommand_mut(&sub) {
            render_subcommand(&mut out, sub, &name);
        }
    }
    out
}

fn render_subcommand(out: &mut String, command: &mut Command, parent: &str) {
    let path = format!("{parent} {}", command.get_name());
    let _ = write!(out, "\n### `{path}`\n\n");
    push_about(out, command);
    push_usage(out, command);
    push_args(out, command, "####", "Options", false);
    if command.get_arguments().any(Arg::is_global_set) {
        out.push_str("\nGlobal options apply.\n");
    }
    // `help <COMMAND>` mirrors the whole tree: its children add nothing.
    if command.get_name() == "help" {
        return;
    }
    for sub in visible_subcommands(command) {
        if let Some(sub) = command.find_subcommand_mut(&sub) {
            render_subcommand(out, sub, &path);
        }
    }
}

fn push_about(out: &mut String, command: &Command) {
    if let Some(about) = command.get_long_about().or_else(|| command.get_about()) {
        let _ = write!(out, "{}\n\n", about.to_string().trim_end());
    }
}

fn push_usage(out: &mut String, command: &mut Command) {
    let usage = command.render_usage().to_string();
    let usage = usage.trim_start_matches("Usage: ").trim_end();
    let _ = write!(out, "```text\n{usage}\n```\n");
}

fn push_args(out: &mut String, command: &Command, heading: &str, options_title: &str, root: bool) {
    let (positionals, options): (Vec<&Arg>, Vec<&Arg>) =
        visible_args(command, root).partition(|arg| arg.is_positional());
    render_args(out, heading, "Arguments", &positionals);
    render_args(out, heading, options_title, &options);
}

fn visible_subcommands(command: &Command) -> Vec<String> {
    command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .collect()
}

/// Visible arguments; on subcommands, global options are documented once on the root.
fn visible_args(command: &Command, root: bool) -> impl Iterator<Item = &Arg> {
    command
        .get_arguments()
        .filter(move |arg| !arg.is_hide_set() && (root || !arg.is_global_set()))
}

fn render_args(out: &mut String, heading: &str, title: &str, args: &[&Arg]) {
    if args.is_empty() {
        return;
    }
    let _ = write!(
        out,
        "\n{heading} {title}\n\n| Option | Values | Default | Description |\n| --- | --- | --- | --- |\n"
    );
    for arg in args {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&flag(arg)),
            cell(&values(arg)),
            cell(&default(arg)),
            cell(&description(arg)),
        );
    }
}

fn flag(arg: &Arg) -> String {
    let value = value_name(arg);
    if arg.is_positional() {
        return format!("`{value}`");
    }
    let suffix = if takes_value(arg) {
        format!(" {value}")
    } else {
        String::new()
    };
    let mut forms = Vec::new();
    if let Some(short) = arg.get_short() {
        forms.push(format!("`-{short}{suffix}`"));
    }
    if let Some(long) = arg.get_long() {
        forms.push(format!("`--{long}{suffix}`"));
    }
    forms.join(", ")
}

fn takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

fn value_name(arg: &Arg) -> String {
    let name = arg
        .get_value_names()
        .and_then(<[_]>::first)
        .map_or_else(|| arg.get_id().as_str().to_owned(), ToString::to_string);
    format!("<{name}>")
}

fn values(arg: &Arg) -> String {
    if matches!(arg.get_action(), ArgAction::Count) {
        return "repeatable".to_owned();
    }
    arg.get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| format!("`{}`", value.get_name()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn default(arg: &Arg) -> String {
    // Flags and counters have implicit `false`/`0` defaults: not worth a cell.
    if !takes_value(arg) {
        return String::new();
    }
    arg.get_default_values()
        .iter()
        .map(|value| format!("`{}`", value.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn description(arg: &Arg) -> String {
    arg.get_long_help()
        .or_else(|| arg.get_help())
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// Escapes a markdown table cell.
fn cell(text: &str) -> String {
    text.trim().replace('|', "\\|").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::render;
    use clap::{Arg, ArgAction, Command};

    fn sample() -> Command {
        Command::new("tool")
            .about("Does things.")
            .arg(
                Arg::new("level")
                    .short('l')
                    .long("level")
                    .global(true)
                    .value_name("N")
                    .default_value("1")
                    .help("Level | pipe"),
            )
            .arg(
                Arg::new("loud")
                    .short('v')
                    .global(true)
                    .action(ArgAction::Count)
                    .help("Louder."),
            )
            .arg(Arg::new("secret").long("secret").hide(true))
            .subcommand(
                Command::new("run")
                    .about("Runs.")
                    .arg(Arg::new("target").required(true).help("What to run."))
                    .arg(
                        Arg::new("mode")
                            .long("mode")
                            .value_parser(["fast", "slow"])
                            .help("Mode."),
                    ),
            )
            .subcommand(Command::new("internal").hide(true))
    }

    #[test]
    fn renders_root_and_subcommands() {
        let doc = render(&sample());
        assert!(doc.starts_with("# `tool` CLI reference\n"), "{doc}");
        assert!(doc.contains("Does things."));
        assert!(
            doc.contains("## Usage\n\n```text\ntool [OPTIONS] [COMMAND]\n```"),
            "{doc}"
        );
        assert!(doc.contains("### Global options"));
        assert!(doc.contains("## Commands"));
        assert!(doc.contains("### `tool run`\n\nRuns."), "{doc}");
    }

    #[test]
    fn renders_flags_values_and_defaults() {
        let doc = render(&sample());
        assert!(
            doc.contains("| `-l <N>`, `--level <N>` |  | `1` | Level \\| pipe |"),
            "{doc}"
        );
        assert!(doc.contains("| `-v` | repeatable |  | Louder. |"), "{doc}");
        assert!(
            doc.contains("| `--mode <mode>` | `fast`, `slow` |  | Mode. |"),
            "{doc}"
        );
        assert!(doc.contains("| `<target>` |  |  | What to run. |"), "{doc}");
    }

    #[test]
    fn globals_are_documented_once() {
        let doc = render(&sample());
        assert_eq!(doc.matches("`--level <N>`").count(), 1, "{doc}");
        assert!(doc.contains("Global options apply."));
    }

    #[test]
    fn help_children_are_skipped() {
        let doc = render(&sample());
        assert!(doc.contains("### `tool help`"), "{doc}");
        assert!(!doc.contains("`tool help run`"), "{doc}");
    }

    #[test]
    fn command_without_subcommands_has_options() {
        let doc = render(
            &Command::new("solo").arg(Arg::new("flag").long("flag").action(ArgAction::SetTrue)),
        );
        assert!(doc.contains("### Options"), "{doc}");
        assert!(!doc.contains("Global options"), "{doc}");
        assert!(!doc.contains("## Commands"), "{doc}");
    }

    #[test]
    fn hidden_items_are_skipped() {
        let doc = render(&sample());
        assert!(!doc.contains("secret"));
        assert!(!doc.contains("internal"));
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(render(&sample()), render(&sample()));
    }
}
