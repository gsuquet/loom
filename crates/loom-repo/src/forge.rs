//! Forge of a host: `[forges]` overrides, then the public forges.

use std::collections::BTreeMap;

use loom_api::ForgeKind;
use loom_config::repos::{ForgeConfig, ForgeHost};

/// Forge of `host` (lowercase, SSH aliases resolved): its `[forges."<host>"] kind` when
/// configured, otherwise GitHub for `github.com` and GitLab for `gitlab.com`, otherwise
/// `None`. Pure.
#[must_use]
pub fn detect_forge(host: &str, forges: &BTreeMap<ForgeHost, ForgeConfig>) -> Option<ForgeKind> {
    if let Some((_, forge)) = forges.iter().find(|(name, _)| name.as_str() == host) {
        return Some(forge.kind);
    }
    match host {
        "github.com" => Some(ForgeKind::Github),
        "gitlab.com" => Some(ForgeKind::Gitlab),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::detect_forge;
    use loom_api::ForgeKind;
    use loom_config::repos::{ForgeConfig, ForgeHost};
    use std::collections::BTreeMap;

    #[test]
    fn detects_public_forges_and_overrides() {
        let forges = BTreeMap::from([
            (
                ForgeHost::try_from("gitlab.company-a.example".to_owned()).unwrap(),
                ForgeConfig {
                    kind: ForgeKind::Gitlab,
                },
            ),
            (
                ForgeHost::try_from("github.company-b.example".to_owned()).unwrap(),
                ForgeConfig {
                    kind: ForgeKind::Github,
                },
            ),
            // An override wins over the built-in kind.
            (
                ForgeHost::try_from("gitlab.com".to_owned()).unwrap(),
                ForgeConfig {
                    kind: ForgeKind::Github,
                },
            ),
        ]);
        for (host, expected) in [
            ("github.com", Some(ForgeKind::Github)),
            ("gitlab.com", Some(ForgeKind::Github)),
            ("gitlab.company-a.example", Some(ForgeKind::Gitlab)),
            ("github.company-b.example", Some(ForgeKind::Github)),
            ("git.example", None),
            ("www.github.com", None),
        ] {
            assert_eq!(detect_forge(host, &forges), expected, "{host}");
        }
        assert_eq!(
            detect_forge("gitlab.com", &BTreeMap::new()),
            Some(ForgeKind::Gitlab)
        );
    }
}
