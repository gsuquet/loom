//! `loom connector status`: the request path and the text output.

use std::fmt::Write as _;

use loom_api::{
    ConnectorFailure, ConnectorHealth, ConnectorList, ConnectorStatus, ContextName, PolledBy,
    Secret,
};

use crate::repo::encode_segment;

/// `GET /v1/connectors`, of one context when `context` names it.
#[must_use]
pub fn status_path(context: Option<&ContextName>) -> String {
    context.map_or_else(
        || "/v1/connectors".to_owned(),
        |context| {
            format!(
                "/v1/connectors?context={}",
                encode_segment(context.as_str())
            )
        },
    )
}

/// A text of a forge or a vendor on one line: anyone can write it, so it holds no control
/// character, such as a newline or an escape sequence that moves the terminal cursor.
fn clean(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

const fn secret_name(secret: Secret) -> &'static str {
    match secret {
        Secret::Token => "token",
        Secret::AppKey => "application key",
    }
}

fn health_text(health: &ConnectorHealth) -> String {
    match health {
        ConnectorHealth::NotPolledYet => "not polled yet".to_owned(),
        ConnectorHealth::Ok => "ok".to_owned(),
        ConnectorHealth::Partial { failed } => format!("partial, {} failed", failed.len()),
        ConnectorHealth::RateLimited { until } => format!("rate limited until {until}"),
        ConnectorHealth::Credential { which, error } => {
            // Refused by the host, or not resolved here: "refused" would claim too much.
            format!("{} problem: {}", secret_name(*which), clean(error))
        }
        ConnectorHealth::Forbidden { error, .. } => format!("forbidden: {}", clean(error)),
        ConnectorHealth::InvalidFilter { error } => format!("invalid filter: {}", clean(error)),
        ConnectorHealth::Unreachable { error, failures } => {
            format!("unreachable ({failures} failed in a row): {}", clean(error))
        }
    }
}

const fn failures_of(health: &ConnectorHealth) -> &[ConnectorFailure] {
    match health {
        ConnectorHealth::Partial { failed } | ConnectorHealth::Forbidden { failed, .. } => {
            failed.as_slice()
        }
        ConnectorHealth::NotPolledYet
        | ConnectorHealth::Ok
        | ConnectorHealth::RateLimited { .. }
        | ConnectorHealth::Credential { .. }
        | ConnectorHealth::InvalidFilter { .. }
        | ConnectorHealth::Unreachable { .. } => &[],
    }
}

fn render_one(text: &mut String, status: &ConnectorStatus) {
    let _ = write!(text, "{} {}: ", status.context, status.name);
    if let PolledBy::Other { machine } = &status.polled_by {
        // This machine does not know the health.
        let _ = write!(text, "polled by {machine}");
    } else {
        text.push_str(&health_text(&status.health));
        if status.stale {
            text.push_str(", stale");
        }
        let _ = write!(text, ", {} polled", status.items.listed);
        if let Some(next) = status.next_poll {
            let _ = write!(text, ", next poll {next}");
        }
    }
    if let Some(success) = status.last_success {
        let _ = write!(text, ", last success {success}");
    }
    text.push('\n');
    for failure in failures_of(&status.health) {
        let _ = writeln!(
            text,
            "  {}: {}",
            clean(&failure.key),
            clean(&failure.reason)
        );
    }
}

/// One line per connector, then one indented line per item or list that failed.
#[must_use]
pub fn render_status(list: &ConnectorList) -> String {
    if list.connectors.is_empty() {
        return "No connector: add a forge credential (`token_cmd` or `token_env`).\n".to_owned();
    }
    let mut text = String::new();
    for status in &list.connectors {
        render_one(&mut text, status);
    }
    text
}

#[cfg(test)]
mod tests {
    use loom_api::{ConnectorCounts, ConnectorKind, Timestamp};

    use super::*;

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(1_800_000_000 + secs).unwrap()
    }

    fn status(health: ConnectorHealth) -> ConnectorStatus {
        ConnectorStatus {
            context: "work".parse().unwrap(),
            name: "gitlab.com".to_owned(),
            kind: ConnectorKind::Gitlab,
            host: "gitlab.com".to_owned(),
            polled_by: PolledBy::ThisMachine,
            health,
            stale: false,
            last_attempt: None,
            last_success: None,
            next_poll: None,
            items: ConnectorCounts {
                listed: 3,
                ..ConnectorCounts::default()
            },
            capabilities: ConnectorKind::Gitlab.capabilities(),
        }
    }

    fn one(status: ConnectorStatus) -> String {
        render_status(&ConnectorList {
            connectors: vec![status],
        })
    }

    #[test]
    fn the_path_names_and_encodes_the_context() {
        assert_eq!(status_path(None), "/v1/connectors");
        assert_eq!(
            status_path(Some(&"work".parse().unwrap())),
            "/v1/connectors?context=work"
        );
    }

    #[test]
    fn an_empty_list_tells_what_to_do() {
        let text = render_status(&ConnectorList { connectors: vec![] });
        assert!(text.contains("add a forge credential"), "{text}");
    }

    #[test]
    fn each_health_has_its_text() {
        let until = at(0).to_string();
        let error = "why".to_owned();
        let cases = [
            (ConnectorHealth::NotPolledYet, "not polled yet".to_owned()),
            (ConnectorHealth::Ok, "ok".to_owned()),
            (
                ConnectorHealth::Partial { failed: vec![] },
                "partial, 0 failed".to_owned(),
            ),
            (
                ConnectorHealth::RateLimited { until: at(0) },
                format!("rate limited until {until}"),
            ),
            (
                ConnectorHealth::Credential {
                    which: Secret::Token,
                    error: error.clone(),
                },
                "token problem: why".to_owned(),
            ),
            (
                ConnectorHealth::Credential {
                    which: Secret::AppKey,
                    error: error.clone(),
                },
                "application key problem: why".to_owned(),
            ),
            (
                ConnectorHealth::Forbidden {
                    error: error.clone(),
                    failed: vec![],
                },
                "forbidden: why".to_owned(),
            ),
            (
                ConnectorHealth::InvalidFilter {
                    error: error.clone(),
                },
                "invalid filter: why".to_owned(),
            ),
            (
                ConnectorHealth::Unreachable { error, failures: 2 },
                "unreachable (2 failed in a row): why".to_owned(),
            ),
        ];
        for (health, expected) in cases {
            assert_eq!(
                one(status(health)),
                format!("work gitlab.com: {expected}, 3 polled\n")
            );
        }
    }

    #[test]
    fn the_times_and_stale_are_on_the_line() {
        let text = one(ConnectorStatus {
            stale: true,
            next_poll: Some(at(1)),
            last_success: Some(at(0)),
            ..status(ConnectorHealth::Ok)
        });
        assert_eq!(
            text,
            format!(
                "work gitlab.com: ok, stale, 3 polled, next poll {}, last success {}\n",
                at(1),
                at(0)
            )
        );
    }

    #[test]
    fn what_failed_is_on_its_own_lines_for_partial_and_forbidden() {
        let failed = vec![ConnectorFailure {
            key: "list:own".to_owned(),
            reason: "502".to_owned(),
        }];
        for health in [
            ConnectorHealth::Partial {
                failed: failed.clone(),
            },
            ConnectorHealth::Forbidden {
                error: "list:mentioned: 403".to_owned(),
                failed,
            },
        ] {
            let text = one(status(health));
            assert!(text.ends_with("\n  list:own: 502\n"), "{text}");
        }
    }

    #[test]
    fn another_machine_claims_no_health() {
        let text = one(ConnectorStatus {
            polled_by: PolledBy::Other {
                machine: "tower".to_owned(),
            },
            ..status(ConnectorHealth::NotPolledYet)
        });
        assert_eq!(text, "work gitlab.com: polled by tower\n");
    }

    #[test]
    fn a_text_of_the_forge_cannot_move_the_terminal() {
        let text = one(status(ConnectorHealth::Partial {
            failed: vec![ConnectorFailure {
                key: "a\nb".to_owned(),
                reason: "c\u{1b}[2Jd".to_owned(),
            }],
        }));
        assert!(text.ends_with("\n  a b: c [2Jd\n"), "{text:?}");
        let text = one(status(ConnectorHealth::Forbidden {
            error: "x\ny\u{1b}".to_owned(),
            failed: vec![],
        }));
        assert!(text.contains("forbidden: x y , 3"), "{text:?}");
        let text = one(status(ConnectorHealth::Unreachable {
            error: "x\r\ny".to_owned(),
            failures: 1,
        }));
        assert_eq!(text.lines().count(), 1, "{text:?}");
    }
}
