//! `loom pr`: the checks of the branch, the push command and the output, without I/O. The git
//! commands and the loomd request are in the binary.

use std::fmt::Write as _;

use loom_api::{MrOutcome, PrReport};

/// Why `loom pr` refuses the branch, before any push.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrRefusal {
    /// `HEAD` is not on a branch.
    #[error("HEAD is detached: check out the branch of the change first")]
    Detached,
    /// The branch is the default branch of the remote.
    #[error("`{0}` is the default branch: create a branch for the change first")]
    DefaultBranch(String),
    /// The branch is the target branch.
    #[error("`{0}` is the target branch: give another one with --base")]
    SameAsBase(String),
    /// The branch has no commit that the base does not have.
    #[error("`{branch}` has no commit ahead of `{base}`: commit the change first")]
    NothingAhead {
        /// The branch.
        branch: String,
        /// The target branch.
        base: String,
    },
    /// The upstream of the branch is a local branch (remote `.`).
    #[error(
        "the upstream of `{0}` is a local branch: set a remote with `git branch --set-upstream-to <remote>/<branch>`"
    )]
    LocalUpstream(String),
    /// The upstream remote of the branch is a URL, not the name of a remote.
    #[error(
        "the upstream remote of `{branch}` is the URL `{remote}`, not a remote name: add it with `git remote add` and set it with `git branch --set-upstream-to`"
    )]
    UrlUpstream {
        /// The branch.
        branch: String,
        /// The `branch.<b>.remote` value.
        remote: String,
    },
    /// The upstream of the branch has another name: the push goes to the same name.
    #[error(
        "`{branch}` tracks `{merge}`, another name: rename the branch, or unset its upstream with `git branch --unset-upstream`"
    )]
    OtherUpstreamName {
        /// The branch.
        branch: String,
        /// The `branch.<b>.merge` value.
        merge: String,
    },
}

/// Checks the upstream of `branch`: `remote` is the name of a remote, and `merge`
/// (`branch.<b>.merge`, when set) is `refs/heads/<branch>`, where the push goes. Pure.
///
/// # Errors
///
/// [`PrRefusal`].
pub fn check_upstream(branch: &str, remote: &str, merge: Option<&str>) -> Result<(), PrRefusal> {
    if remote == "." {
        return Err(PrRefusal::LocalUpstream(branch.to_owned()));
    }
    // A remote name can hold `/` (`up/stream`); a URL or a path has `:` or starts like a path.
    if remote.contains(':')
        || ["/", "./", "../"]
            .iter()
            .any(|start| remote.starts_with(start))
    {
        return Err(PrRefusal::UrlUpstream {
            branch: branch.to_owned(),
            remote: remote.to_owned(),
        });
    }
    if let Some(merge) = merge
        && merge != format!("refs/heads/{branch}")
    {
        return Err(PrRefusal::OtherUpstreamName {
            branch: branch.to_owned(),
            merge: merge.to_owned(),
        });
    }
    Ok(())
}

/// Checks that `branch` can be the source of an MR into `base`: not the default branch of the
/// remote (`default`), not `base`, and `ahead` commits ahead of it. Pure.
///
/// # Errors
///
/// [`PrRefusal`].
pub fn check_branch(branch: &str, base: &str, default: &str, ahead: u64) -> Result<(), PrRefusal> {
    if default == branch {
        return Err(PrRefusal::DefaultBranch(branch.to_owned()));
    }
    if branch == base {
        return Err(PrRefusal::SameAsBase(branch.to_owned()));
    }
    if ahead == 0 {
        return Err(PrRefusal::NothingAhead {
            branch: branch.to_owned(),
            base: base.to_owned(),
        });
    }
    Ok(())
}

/// The push of `branch` to the branch of the same name on `remote`, with its upstream set. Pure.
///
/// `--` keeps a remote that starts with `-` from being an option, and the full refspec keeps a
/// tag with the name of the branch from making it ambiguous.
#[must_use]
pub fn push_command(remote: &str, branch: &str) -> Vec<String> {
    let reference = format!("refs/heads/{branch}");
    vec![
        "git".to_owned(),
        "push".to_owned(),
        "-u".to_owned(),
        "--".to_owned(),
        remote.to_owned(),
        format!("{reference}:{reference}"),
    ]
}

/// The default branch of `remote` from the output of
/// `git symbolic-ref --short refs/remotes/<remote>/HEAD`, such as `origin/main`. Pure.
#[must_use]
pub fn default_branch(remote: &str, symbolic_ref: &str) -> Option<String> {
    symbolic_ref
        .trim()
        .strip_prefix(remote)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
}

/// The text output of `loom pr`. Pure.
#[must_use]
pub fn render(report: &PrReport) -> String {
    let mut text = String::new();
    let push = report.push.command.join(" ");
    let result = &report.result;
    if report.dry_run {
        let _ = writeln!(text, "would run: {push}");
        for (index, request) in result.requests.iter().enumerate() {
            let _ = writeln!(text, "would send: {} {}", request.method, request.url);
            if let Some(body) = &request.body {
                let body = serde_json::to_string_pretty(body).unwrap_or_default();
                for line in body.lines() {
                    let _ = writeln!(text, "  {line}");
                }
            }
            if index == 0 && result.requests.len() > 1 {
                let _ = writeln!(
                    text,
                    "  (an open MR of the branch is linked instead of the request below)"
                );
            }
        }
        return text;
    }
    let _ = writeln!(
        text,
        "pushed {} to {}",
        report.push.branch, report.push.remote
    );
    if let Some(mr) = &result.mr {
        let verb = match result.outcome {
            MrOutcome::Opened => "opened draft MR",
            MrOutcome::Linked | MrOutcome::Planned => "linked open MR",
        };
        let url = mr.url.as_deref().unwrap_or_default();
        let _ = writeln!(text, "{verb} {} {url}", mr.key);
        let _ = writeln!(text, "task {}: delivery link {}", result.task, mr.key);
    }
    text
}

#[cfg(test)]
mod tests {
    use loom_api::{
        ExternalRef, ForgeKind, ForgeRequestPlan, MrOpenResult, MrOutcome, PrReport, PushPlan,
    };
    use serde_json::json;

    use super::{PrRefusal, check_branch, check_upstream, default_branch, push_command, render};

    #[test]
    fn the_default_branch_the_base_and_an_empty_branch_are_refused() {
        assert_eq!(
            check_branch("main", "main", "main", 3),
            Err(PrRefusal::DefaultBranch("main".to_owned()))
        );
        // Even with another base.
        assert_eq!(
            check_branch("main", "release", "main", 3),
            Err(PrRefusal::DefaultBranch("main".to_owned()))
        );
        assert_eq!(
            check_branch("release", "release", "main", 3),
            Err(PrRefusal::SameAsBase("release".to_owned()))
        );
        assert_eq!(
            check_branch("fix/a", "main", "main", 0),
            Err(PrRefusal::NothingAhead {
                branch: "fix/a".to_owned(),
                base: "main".to_owned(),
            })
        );
        assert_eq!(check_branch("fix/a", "main", "main", 1), Ok(()));
    }

    #[test]
    fn an_upstream_must_be_a_remote_name_and_the_same_branch_name() {
        assert_eq!(check_upstream("fix/a", "origin", None), Ok(()));
        assert_eq!(check_upstream("fix/a", "up/stream", None), Ok(()));
        assert_eq!(
            check_upstream("fix/a", "origin", Some("refs/heads/fix/a")),
            Ok(())
        );
        assert_eq!(
            check_upstream("fix/a", ".", Some("refs/heads/main")),
            Err(PrRefusal::LocalUpstream("fix/a".to_owned()))
        );
        for url in [
            "https://github.com/acme/shop.git",
            "git@github.com:acme/shop.git",
            "../shop",
            "/srv/shop.git",
        ] {
            assert_eq!(
                check_upstream("fix/a", url, None),
                Err(PrRefusal::UrlUpstream {
                    branch: "fix/a".to_owned(),
                    remote: url.to_owned(),
                })
            );
        }
        assert_eq!(
            check_upstream("fix/a", "origin", Some("refs/heads/main")),
            Err(PrRefusal::OtherUpstreamName {
                branch: "fix/a".to_owned(),
                merge: "refs/heads/main".to_owned(),
            })
        );
    }

    #[test]
    fn push_and_default_branch() {
        assert_eq!(
            push_command("-x", "fix/a"),
            [
                "git",
                "push",
                "-u",
                "--",
                "-x",
                "refs/heads/fix/a:refs/heads/fix/a"
            ]
        );
        assert_eq!(
            default_branch("origin", "origin/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            default_branch("up", "up/release/2026").as_deref(),
            Some("release/2026")
        );
        assert_eq!(default_branch("origin", "upstream/main"), None);
        assert_eq!(default_branch("origin", "origin/"), None);
    }

    fn report(dry_run: bool, outcome: MrOutcome) -> PrReport {
        PrReport {
            dry_run,
            push: PushPlan {
                remote: "origin".to_owned(),
                branch: "fix/a".to_owned(),
                command: push_command("origin", "fix/a"),
                pushed: !dry_run,
            },
            result: MrOpenResult {
                task: "k3q7zm2a".parse().unwrap(),
                forge: ForgeKind::Github,
                outcome,
                mr: (!dry_run).then(|| ExternalRef {
                    provider: "github".to_owned(),
                    key: "acme/shop#7".to_owned(),
                    url: Some("https://github.com/acme/shop/pull/7".to_owned()),
                }),
                requests: vec![
                    ForgeRequestPlan {
                        method: "GET".to_owned(),
                        url: "https://api.github.com/repos/acme/shop/pulls?head=acme%3Afix%2Fa"
                            .to_owned(),
                        body: None,
                    },
                    ForgeRequestPlan {
                        method: "POST".to_owned(),
                        url: "https://api.github.com/repos/acme/shop/pulls".to_owned(),
                        body: Some(json!({ "draft": true })),
                    },
                ],
            },
        }
    }

    #[test]
    fn a_dry_run_prints_the_push_and_the_requests() {
        assert_eq!(
            render(&report(true, MrOutcome::Planned)),
            "would run: git push -u -- origin refs/heads/fix/a:refs/heads/fix/a\n\
             would send: GET https://api.github.com/repos/acme/shop/pulls?head=acme%3Afix%2Fa\n  \
             (an open MR of the branch is linked instead of the request below)\n\
             would send: POST https://api.github.com/repos/acme/shop/pulls\n  \
             {\n    \"draft\": true\n  }\n"
        );
    }

    #[test]
    fn a_run_prints_the_mr_and_the_link() {
        assert_eq!(
            render(&report(false, MrOutcome::Opened)),
            "pushed fix/a to origin\n\
             opened draft MR acme/shop#7 https://github.com/acme/shop/pull/7\n\
             task k3q7zm2a: delivery link acme/shop#7\n"
        );
        assert!(render(&report(false, MrOutcome::Linked)).contains("linked open MR acme/shop#7"));
    }
}
