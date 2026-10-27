//! `loom prompt`: the context of the current directory and its focus task, for a shell
//! prompt. The decisions and the rendering are here; `prompt_cmd.rs` does the I/O.

use std::fmt::Write as _;
use std::time::Duration;

use loom_api::{ContextAssignment, ContextName, PromptSegment};

use crate::cli::Shell;
use crate::task::resolve_context;

/// Default of `--max-title`.
pub const DEFAULT_MAX_TITLE: usize = 30;
/// Longest `loom prompt` waits for loomd, counted from its start.
pub const BUDGET: Duration = Duration::from_millis(100);
/// Test hook: replaces [`BUDGET`], in milliseconds, in a debug build.
pub const TEST_BUDGET_ENV: &str = "LOOM_TEST_PROMPT_BUDGET_MS";

/// The wait for loomd: `test` (milliseconds, [`TEST_BUDGET_ENV`]) when `allowed`, otherwise
/// [`BUDGET`].
#[must_use]
pub fn budget(test: Option<&str>, allowed: bool) -> Duration {
    test.filter(|_| allowed)
        .and_then(|text| text.parse().ok())
        .map_or(BUDGET, Duration::from_millis)
}

/// What loom knows about the git repository of the current directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdRepo<'a> {
    /// The current directory is not in a git repository.
    Outside,
    /// The index holds the repository of the current directory, with this assignment.
    Indexed(&'a ContextAssignment),
    /// The current directory is in a git repository that loomd did not find: not indexed yet,
    /// or loomd did not answer in time.
    Unknown,
}

/// Whether the context depends on the current directory: neither `-c` nor a non-empty
/// `LOOM_CONTEXT` gives it.
#[must_use]
pub fn needs_cwd(flag: Option<&ContextName>, env: Option<&str>) -> bool {
    flag.is_none() && env.is_none_or(str::is_empty)
}

/// The context of the prompt, in the order of [`resolve_context`].
///
/// `None` when no rule gives one, when `env` is invalid, or when the current directory is in a
/// repository that loomd did not find: that repository could give another context than the
/// directory rules.
#[must_use]
pub fn prompt_context(
    flag: Option<&ContextName>,
    env: Option<&str>,
    repo: CwdRepo<'_>,
    rule: Option<&ContextAssignment>,
) -> Option<ContextName> {
    let repo = if needs_cwd(flag, env) {
        match repo {
            CwdRepo::Outside => None,
            CwdRepo::Indexed(assignment) => Some(assignment),
            CwdRepo::Unknown => return None,
        }
    } else {
        None
    };
    resolve_context(flag, env, repo, rule)
        .ok()
        .map(|resolved| resolved.context)
}

/// The prompt line: `<context>`, or `<context> <prefix> <title>` when the context has a focus.
///
/// Nothing without a context. The title is cut to `max_title` characters, with `…` as the
/// last one, and left out when `max_title` is 0. Control and invisible characters become
/// spaces. For zsh,
/// `%` becomes `%%`, so that prompt expansion shows it as it is.
#[must_use]
pub fn render(segment: &PromptSegment, max_title: usize, shell: Option<Shell>) -> String {
    let Some(context) = &segment.context else {
        return String::new();
    };
    let mut line = context.to_string();
    if let Some(focus) = &segment.focus {
        let _ = write!(line, " {}", clean(&focus.task.prefix));
        let title = focus
            .task
            .task
            .as_ref()
            .map(|task| cut(clean(&task.title).trim(), max_title))
            .unwrap_or_default();
        if !title.is_empty() {
            let _ = write!(line, " {title}");
        }
    }
    let line = match shell {
        Some(Shell::Zsh) => line.replace('%', "%%"),
        Some(Shell::Bash) | None => line,
    };
    line + "\n"
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if hides_text(c) { ' ' } else { c })
        .collect()
}

/// Control characters, and the invisible characters that can reorder or hide the text around
/// them: zero-width characters and marks, line and paragraph separators, bidi embeddings,
/// overrides and isolates, word joiner, byte order mark.
const fn hides_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
        )
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let Some(kept) = max.checked_sub(1) else {
        return String::new();
    };
    let mut cut: String = text.chars().take(kept).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use loom_api::{
        ContextAssignment, ContextName, FocusEntry, Importance, Kind, PromptSegment, Relations,
        Status, Task, TaskSummary, Timestamp,
    };

    use std::time::Duration;

    use super::{BUDGET, CwdRepo, budget, needs_cwd, prompt_context, render};
    use crate::cli::Shell;

    fn name(text: &str) -> ContextName {
        text.parse().unwrap()
    }

    fn segment(title: Option<&str>) -> PromptSegment {
        let context = name("work");
        let created = Timestamp::from_unix_secs(1_791_795_600).unwrap();
        let task = title.map(|title| Task {
            id: "k3v9qx2m".parse().unwrap(),
            title: title.to_owned(),
            kind: Kind::Chore,
            context: context.clone(),
            status: Status::Active,
            importance: Importance::P2,
            due: None,
            pinned: false,
            links: Vec::new(),
            relations: Relations::default(),
            created,
            updated: created,
        });
        PromptSegment {
            context: Some(context.clone()),
            focus: Some(FocusEntry {
                context: context.clone(),
                task: TaskSummary {
                    id: "k3v9qx2m".parse().unwrap(),
                    prefix: "k3".to_owned(),
                    context,
                    task,
                    read_only: None,
                    rank: None,
                },
                since: created,
                repo_path: None,
            }),
        }
    }

    #[test]
    fn only_a_debug_build_reads_the_test_budget() {
        assert_eq!(budget(None, true), BUDGET);
        assert_eq!(budget(Some("5000"), true), Duration::from_secs(5));
        assert_eq!(budget(Some("5000"), false), BUDGET);
        assert_eq!(budget(Some("soon"), true), BUDGET);
    }

    #[test]
    fn the_flag_and_the_environment_make_the_directory_useless() {
        assert!(needs_cwd(None, None));
        assert!(needs_cwd(None, Some("")));
        assert!(!needs_cwd(None, Some("work")));
        assert!(!needs_cwd(Some(&name("work")), None));
    }

    #[test]
    fn the_context_follows_the_order_of_the_other_commands() {
        let repo = ContextAssignment::Override {
            context: name("work"),
        };
        let rule = ContextAssignment::Default {
            context: name("personal"),
        };
        let flag = name("home");
        let context = |flag, env, cwd, rule| prompt_context(flag, env, cwd, rule);
        assert_eq!(
            context(Some(&flag), Some("other"), CwdRepo::Unknown, Some(&rule)),
            Some(name("home"))
        );
        assert_eq!(
            context(None, Some("other"), CwdRepo::Unknown, Some(&rule)),
            Some(name("other"))
        );
        assert_eq!(
            context(None, None, CwdRepo::Indexed(&repo), Some(&rule)),
            Some(name("work"))
        );
        assert_eq!(
            context(None, Some(""), CwdRepo::Outside, Some(&rule)),
            Some(name("personal"))
        );
        // An unassigned repository leaves the directory rules.
        assert_eq!(
            context(
                None,
                None,
                CwdRepo::Indexed(&ContextAssignment::Unassigned),
                Some(&rule)
            ),
            Some(name("personal"))
        );
        assert_eq!(context(None, None, CwdRepo::Outside, None), None);
    }

    #[test]
    fn an_unknown_repository_or_a_bad_environment_gives_no_context() {
        let rule = ContextAssignment::Default {
            context: name("personal"),
        };
        // The repository could belong to another context than the directory.
        assert_eq!(
            prompt_context(None, None, CwdRepo::Unknown, Some(&rule)),
            None
        );
        assert_eq!(
            prompt_context(None, Some("Not A Name"), CwdRepo::Outside, Some(&rule)),
            None
        );
    }

    #[test]
    fn the_line_shows_the_context_then_the_focus() {
        assert_eq!(render(&PromptSegment::default(), 30, None), "");
        let alone = PromptSegment {
            context: Some(name("work")),
            focus: None,
        };
        assert_eq!(render(&alone, 30, None), "work\n");
        assert_eq!(
            render(&segment(Some("Fix the login")), 30, None),
            "work k3 Fix the login\n"
        );
        // An invalid `task.md` has no title: the prefix stays.
        assert_eq!(render(&segment(None), 30, None), "work k3\n");
        assert_eq!(render(&segment(Some("Fix")), 0, None), "work k3\n");
    }

    #[test]
    fn a_long_title_is_cut_with_an_ellipsis() {
        let line = render(&segment(Some("Rewrite the whole thing")), 12, None);
        assert_eq!(line, "work k3 Rewrite the…\n");
        assert_eq!(
            render(&segment(Some("Exactly 10")), 10, None),
            "work k3 Exactly 10\n"
        );
        assert_eq!(
            render(&segment(Some("éèàçùâêîôû!")), 4, None),
            "work k3 éèà…\n"
        );
        assert_eq!(render(&segment(Some("Two")), 1, None), "work k3 …\n");
    }

    #[test]
    fn control_characters_cannot_break_the_prompt() {
        let line = render(&segment(Some("a\nb\u{1b}[31mc\t")), 30, None);
        assert_eq!(line, "work k3 a b [31mc\n");
        // A bidi override would show the rest of the prompt reversed.
        let line = render(
            &segment(Some("a\u{202E}b\u{200B}c\u{2066}d\u{2028}e\u{FEFF}")),
            30,
            None,
        );
        assert_eq!(line, "work k3 a b c d e\n");
    }

    #[test]
    fn zsh_shows_a_percent_sign_as_it_is() {
        let title = Some("Cut 50% of %F{red}");
        assert_eq!(
            render(&segment(title), 30, Some(Shell::Zsh)),
            "work k3 Cut 50%% of %%F{red}\n"
        );
        assert_eq!(
            render(&segment(title), 30, Some(Shell::Bash)),
            "work k3 Cut 50% of %F{red}\n"
        );
    }
}
