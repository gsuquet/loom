//! GitHub releases: the parts of the REST API responses the updater reads, and pure
//! functions over them (tags, dev selection, asset names).

use semver::Version;
use serde::Deserialize;

/// A GitHub release, as returned by `GET /repos/{owner}/{repo}/releases[/…]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    /// Git tag: `vX.Y.Z` or `vX.Y.Z-dev.N`.
    pub tag_name: String,
    /// Drafts are not published releases; they are ignored.
    #[serde(default)]
    pub draft: bool,
    /// Files attached to the release.
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    /// Asset ID, for `GET /repos/{owner}/{repo}/releases/assets/{id}`.
    pub id: u64,
    /// File name.
    pub name: String,
}

/// Version of a release tag: `vX.Y.Z` or `vX.Y.Z-dev.N`, nothing else.
#[must_use]
pub fn tag_version(tag: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v')?).ok()?;
    let pre_ok = version.pre.is_empty() || dev_number(&version).is_some();
    (pre_ok && version.build.is_empty()).then_some(version)
}

/// `N` of a `X.Y.Z-dev.N` version.
fn dev_number(version: &Version) -> Option<u64> {
    let n = version.pre.as_str().strip_prefix("dev.")?;
    // Semver numeric identifiers have no leading zero; `parse` would accept one.
    if n.is_empty() || (n.len() > 1 && n.starts_with('0')) {
        return None;
    }
    n.parse().ok()
}

/// Published releases of a release list with their versions: drafts and tags other than
/// `vX.Y.Z` or `vX.Y.Z-dev.N` are skipped. Pure.
#[must_use]
pub fn published(releases: Vec<Release>) -> Vec<(Version, Release)> {
    releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| Some((tag_version(&release.tag_name)?, release)))
        .collect()
}

/// Target triple of the release archive for a binary built for `build_target`.
///
/// Linux releases are static musl builds: a local `*-linux-gnu` build upgrades to the
/// `*-linux-musl` archive of the same architecture.
#[must_use]
pub fn release_target(build_target: &str) -> String {
    build_target.strip_suffix("-linux-gnu").map_or_else(
        || build_target.to_owned(),
        |arch| format!("{arch}-linux-musl"),
    )
}

/// File name of the release archive of `version` for `target`.
#[must_use]
pub fn archive_name(version: &Version, target: &str) -> String {
    format!("loom-{version}-{target}.tar.gz")
}

/// Why an asset cannot be picked from a release.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    /// No asset has this name.
    #[error("release {tag} has no asset {name}")]
    Missing {
        /// Release tag.
        tag: String,
        /// Asset name.
        name: String,
    },
    /// Several assets have this name.
    #[error("release {tag} lists asset {name} more than once")]
    Duplicate {
        /// Release tag.
        tag: String,
        /// Asset name.
        name: String,
    },
}

/// ID of the asset named `name`.
///
/// # Errors
///
/// [`AssetError`] when no asset, or more than one, has this name.
pub fn asset_id(release: &Release, name: &str) -> Result<u64, AssetError> {
    let mut matching = release.assets.iter().filter(|asset| asset.name == name);
    let error = |missing: bool| {
        let (tag, name) = (release.tag_name.clone(), name.to_owned());
        if missing {
            AssetError::Missing { tag, name }
        } else {
            AssetError::Duplicate { tag, name }
        }
    };
    let first = matching.next().ok_or_else(|| error(true))?;
    if matching.next().is_some() {
        return Err(error(false));
    }
    Ok(first.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool) -> Release {
        Release {
            tag_name: tag.to_owned(),
            draft,
            assets: Vec::new(),
        }
    }

    #[test]
    fn tags_are_v_prefixed_stable_or_dev_versions() {
        assert_eq!(tag_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(
            tag_version("v1.2.3-dev.42").map(|v| v.to_string()),
            Some("1.2.3-dev.42".to_owned())
        );
        for bad in [
            "1.2.3",
            "v1.2",
            "v1.2.3-rc.1",
            "v1.2.3-dev",
            "v1.2.3-dev.x",
            "v1.2.3+meta",
            "latest",
        ] {
            assert_eq!(tag_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn published_skips_drafts_and_other_tags() {
        let releases = vec![
            release("v0.2.0-dev.9", false),
            release("v0.2.0", false),
            release("v0.3.0-dev.15", true),
            release("v0.3.0-rc.1", false),
            release("nightly", false),
        ];
        let versions: Vec<String> = published(releases)
            .iter()
            .map(|(version, release)| {
                assert_eq!(release.tag_name, format!("v{version}"));
                version.to_string()
            })
            .collect();
        assert_eq!(versions, ["0.2.0-dev.9", "0.2.0"]);
        assert_eq!(published(Vec::new()), Vec::new());
    }

    #[test]
    fn gnu_builds_upgrade_to_musl_archives() {
        assert_eq!(
            release_target("x86_64-unknown-linux-gnu"),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            release_target("aarch64-unknown-linux-musl"),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            release_target("aarch64-apple-darwin"),
            "aarch64-apple-darwin"
        );
    }

    #[test]
    fn archive_names_follow_the_release_layout() {
        let version = Version::parse("0.1.0-dev.42").unwrap();
        assert_eq!(
            archive_name(&version, "aarch64-apple-darwin"),
            "loom-0.1.0-dev.42-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn asset_ids_by_name() {
        let mut release = release("v1.0.0", false);
        release.assets = vec![
            Asset {
                id: 1,
                name: "SHA256SUMS".to_owned(),
            },
            Asset {
                id: 2,
                name: "dup".to_owned(),
            },
            Asset {
                id: 3,
                name: "dup".to_owned(),
            },
        ];
        assert_eq!(asset_id(&release, "SHA256SUMS"), Ok(1));
        assert!(matches!(
            asset_id(&release, "dup"),
            Err(AssetError::Duplicate { .. })
        ));
        let missing = asset_id(&release, "nope").unwrap_err();
        assert_eq!(missing.to_string(), "release v1.0.0 has no asset nope");
    }

    #[test]
    fn parses_github_release_json() {
        let json = r#"{"tag_name":"v1.0.0","draft":false,"prerelease":false,
            "assets":[{"id":7,"name":"SHA256SUMS","size":10}],"body":"notes"}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.0.0");
        assert_eq!(release.assets[0].id, 7);
    }
}
