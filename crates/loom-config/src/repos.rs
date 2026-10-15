//! `default_context`, `[contexts]`, `[repos]` and `[forges]`: types of the repo index
//! configuration and their per-value validation. Cross-key checks (undefined contexts, ties)
//! are in [`crate::config`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use globset::Glob;
use loom_api::{ContextName, ForgeKind, RepoUpdatePolicy, UNASSIGNED};
use serde::de::{DeserializeSeed, Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use toml::Spanned;

use crate::duration::{format_duration, parse_duration};
use crate::machine::MachineName;
use crate::schedules::Schedule;

/// `[contexts.<name>]`: how repositories are matched to a context, and where its tasks live.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    /// `match.paths` and `match.hosts`.
    #[serde(rename = "match", default)]
    pub matching: ContextMatch,
    /// `update`: policy of the context's repositories; `None` leaves the default (`ff`).
    #[serde(default)]
    pub update: Option<RepoUpdatePolicy>,
    /// `store` table: the context's task store.
    #[serde(default)]
    pub store: ContextStore,
    /// `machines`: the machines that hold the context (`machine.toml` `id`); `None` for all.
    #[serde(default, deserialize_with = "machines")]
    pub machines: Option<Vec<MachineName>>,
    /// `[[contexts.<name>.schedules]]`: recurring tasks of the context.
    #[serde(default)]
    pub schedules: Vec<Schedule>,
}

impl ContextConfig {
    /// Whether the machine `machine` holds the context: `machines` is absent or names it.
    #[must_use]
    pub fn held_by(&self, machine: &MachineName) -> bool {
        self.machines
            .as_ref()
            .is_none_or(|machines| machines.contains(machine))
    }
}

fn machines<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<MachineName>>, D::Error> {
    let machines = Vec::<String>::deserialize(deserializer)?;
    if machines.is_empty() {
        return Err(D::Error::custom(
            "machines is empty: remove it (every machine) or give at least one machine",
        ));
    }
    machines
        .into_iter()
        .map(|name| {
            name.parse()
                .map_err(|error| D::Error::custom(format!("machines: {error}")))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// `store` table of a context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextStore {
    /// `store.path`: root of the context store; `None` for `<loom home>/contexts/<name>`.
    #[serde(default, deserialize_with = "store_path")]
    pub path: Option<Spanned<ConfigPath>>,
    /// `store.backup` table: where the store's git repository is pushed; `None` keeps the
    /// commits local.
    #[serde(default)]
    pub backup: Option<StoreBackup>,
}

/// `store.backup` table of a context.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreBackup {
    /// `store.backup.remote`: URL or path of the git repository the store is pushed to and
    /// pulled from.
    pub remote: Spanned<BackupRemote>,
}

/// `store.backup.remote`: a git URL or path, as `git remote add` takes it. Not empty, no
/// whitespace, not starting with `-` (git would read an option).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRemote(String);

impl BackupRemote {
    /// The URL, as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackupRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for BackupRemote {
    type Error = String;

    fn try_from(url: String) -> Result<Self, Self::Error> {
        if url.is_empty() {
            return Err("store.backup.remote: empty".to_owned());
        }
        if url.starts_with('-') {
            return Err(format!(
                "store.backup.remote `{url}`: starts with `-`, git would read an option"
            ));
        }
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!(
                "store.backup.remote `{url}`: whitespace and control characters are not allowed"
            ));
        }
        Ok(Self(url))
    }
}

impl<'de> Deserialize<'de> for BackupRemote {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

fn store_path<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Spanned<ConfigPath>>, D::Error> {
    let path = Spanned::<String>::deserialize(deserializer)?;
    let span = path.span();
    ConfigPath::try_from(path.into_inner())
        .map(|path| Some(Spanned::new(span, path)))
        .map_err(|error| D::Error::custom(format!("store.path: {error}")))
}

/// `match` table of a context. Each list, when given, has at least one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMatch {
    /// `match.paths`: directories whose repositories belong to the context.
    #[serde(default, deserialize_with = "match_paths")]
    pub paths: Vec<Spanned<ConfigPath>>,
    /// `match.hosts`: `host` or `host/path-prefix` of the primary remote.
    #[serde(default, deserialize_with = "match_hosts")]
    pub hosts: Vec<Spanned<HostPattern>>,
}

fn match_paths<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Spanned<ConfigPath>>, D::Error> {
    let paths = Vec::<Spanned<String>>::deserialize(deserializer)?;
    if paths.is_empty() {
        return Err(D::Error::custom(
            "match.paths is empty: remove it or give at least one path",
        ));
    }
    paths
        .into_iter()
        .map(|path| {
            let span = path.span();
            ConfigPath::try_from(path.into_inner())
                .map(|path| Spanned::new(span, path))
                .map_err(|error| D::Error::custom(format!("match.paths: {error}")))
        })
        .collect()
}

/// `roots`: paths, the key named in the error.
struct Roots(Vec<ConfigPath>);

impl<'de> Deserialize<'de> for Roots {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|path| {
                ConfigPath::try_from(path)
                    .map_err(|error| D::Error::custom(format!("roots: {error}")))
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

fn match_hosts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Spanned<HostPattern>>, D::Error> {
    let hosts = Vec::<Spanned<HostPattern>>::deserialize(deserializer)?;
    if hosts.is_empty() {
        return Err(D::Error::custom(
            "match.hosts is empty: remove it or give at least one host",
        ));
    }
    Ok(hosts)
}

/// `[contexts]` keys: valid context names other than [`UNASSIGNED`].
pub(crate) fn contexts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ContextName, ContextConfig>, D::Error> {
    let contexts = BTreeMap::<DefinableName, ContextConfig>::deserialize(deserializer)?;
    Ok(contexts
        .into_iter()
        .map(|(name, context)| (name.0, context))
        .collect())
}

/// A context name that can be defined: not [`UNASSIGNED`].
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct DefinableName(ContextName);

impl<'de> Deserialize<'de> for DefinableName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = ContextName::deserialize(deserializer)?;
        if name.is_unassigned() {
            return Err(D::Error::custom(reserved_message()));
        }
        Ok(Self(name))
    }
}

pub(crate) fn reserved_message() -> String {
    format!("`{UNASSIGNED}` is reserved for repositories without a context")
}

/// A directory in the configuration: absolute, or relative to the user's home with `~` or
/// `~/…`. Kept as written; [`ConfigPath::expand`] gives the path to compare.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigPath(String);

impl ConfigPath {
    /// The path as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path with a leading `~` replaced by `home`, without `.` components or trailing and
    /// repeated `/`. No filesystem access: symbolic links are kept.
    #[must_use]
    pub fn expand(&self, home: &Path) -> PathBuf {
        let rest = self
            .0
            .strip_prefix('~')
            .map(|rest| rest.trim_start_matches('/'));
        let path = rest.map_or_else(|| PathBuf::from(&self.0), |rest| home.join(rest));
        path.components()
            .filter(|component| *component != Component::CurDir)
            .collect()
    }

    /// Normalized text, `~` kept: equal for two spellings of the same path.
    pub(crate) fn normalized(&self) -> PathBuf {
        self.expand(Path::new("~"))
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ConfigPath {
    type Error = String;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let home_relative = path == "~" || path.starts_with("~/");
        if !home_relative && !path.starts_with('/') {
            return Err(format!(
                "invalid path `{path}`: expected an absolute path or one starting with `~/`"
            ));
        }
        if Path::new(&path)
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(format!("invalid path `{path}`: `..` is not allowed"));
        }
        Ok(Self(path))
    }
}

impl<'de> Deserialize<'de> for ConfigPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Whether `host` is a lowercase host name: ASCII letters, digits, `-` and `.`, not starting or
/// ending with `.` or `-`.
fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
}

/// A `match.hosts` entry: `host` or `host/path-prefix`, such as `gitlab.com/company-a`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPattern {
    host: String,
    segments: Vec<String>,
}

impl HostPattern {
    /// Host name, lowercase.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Path prefix, split on `/`; empty for a host alone.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }
}

/// `host` or `host/a/b`, without trailing `/`.
impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.host)?;
        for segment in &self.segments {
            write!(f, "/{segment}")?;
        }
        Ok(())
    }
}

impl TryFrom<String> for HostPattern {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let invalid = |why: &str| format!("invalid match.hosts entry `{text}`: {why}");
        if text.contains("://") || text.contains('@') {
            return Err(invalid("expected `host` or `host/path`, not a URL"));
        }
        let trimmed = text.strip_suffix('/').unwrap_or(&text);
        let mut parts = trimmed.split('/');
        let host = parts.next().unwrap_or_default();
        if !is_host(host) {
            return Err(invalid(
                "the host must be a lowercase host name without port (letters, digits, `-`, `.`)",
            ));
        }
        let segments: Vec<String> = parts.map(str::to_owned).collect();
        if segments
            .iter()
            .any(|segment| segment.is_empty() || segment.chars().any(char::is_whitespace))
        {
            return Err(invalid(
                "the path has an empty segment or a space; write `host/group/subgroup`",
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            segments,
        })
    }
}

impl<'de> Deserialize<'de> for HostPattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A `[forges."<host>"]` key: a lowercase host name, such as `gitlab.company-a.example`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForgeHost(String);

impl ForgeHost {
    /// The host name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ForgeHost {
    type Error = String;

    fn try_from(host: String) -> Result<Self, Self::Error> {
        if !is_host(&host) {
            return Err(format!(
                "invalid [forges] host `{host}`: expected a lowercase host name without port or path (letters, digits, `-`, `.`)"
            ));
        }
        Ok(Self(host))
    }
}

impl<'de> Deserialize<'de> for ForgeHost {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// `[forges."<host>"]`: the forge of a self-hosted host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeConfig {
    /// `kind`: `github` or `gitlab`.
    pub kind: ForgeKind,
}

/// A `[repos] exclude` glob pattern, such as `"archive"` or `"*-old"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludePattern(Glob);

impl ExcludePattern {
    /// The compiled glob.
    #[must_use]
    pub const fn glob(&self) -> &Glob {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ExcludePattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() {
            return Err(D::Error::custom("invalid exclude pattern: empty"));
        }
        Glob::new(&text)
            .map(Self)
            .map_err(|error| D::Error::custom(format!("invalid exclude pattern: {error}")))
    }
}

/// Shortest `[repos] fetch_interval` accepted.
pub const MIN_FETCH_INTERVAL: Duration = Duration::from_mins(1);
/// Default `[repos] fetch_interval`.
pub const DEFAULT_FETCH_INTERVAL: Duration = Duration::from_hours(1);
/// Default `[repos] max_depth`.
pub const DEFAULT_MAX_DEPTH: u32 = 4;
/// Default `[repos] fetch_concurrency`.
pub const DEFAULT_FETCH_CONCURRENCY: u32 = 4;
/// Default `[repos] roots`.
pub const DEFAULT_ROOTS: [&str; 2] = ["~/perso", "~/work"];

/// `[repos] fetch_interval`: a duration of at least [`MIN_FETCH_INTERVAL`], written as text
/// (`"1h"`). Default [`DEFAULT_FETCH_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchInterval(Duration);

impl FetchInterval {
    /// The duration.
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Default for FetchInterval {
    fn default() -> Self {
        Self(DEFAULT_FETCH_INTERVAL)
    }
}

impl<'de> Deserialize<'de> for FetchInterval {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let duration = parse_duration(&text)
            .map_err(|error| D::Error::custom(format!("fetch_interval: {error}")))?;
        if duration < MIN_FETCH_INTERVAL {
            return Err(D::Error::custom(format!(
                "fetch_interval `{text}` is shorter than the minimum {}",
                format_duration(MIN_FETCH_INTERVAL)
            )));
        }
        Ok(Self(duration))
    }
}

/// `[repos."<path>"]`: settings of one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoOverride {
    /// The repository path (the table key).
    pub path: Spanned<ConfigPath>,
    /// `context`: the repository's context, whatever the `match` rules say.
    pub context: Option<Spanned<ContextName>>,
    /// `update`: the repository's update policy, over the context's.
    pub update: Option<RepoUpdatePolicy>,
}

/// Body of a `[repos."<path>"]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoOverrideBody {
    #[serde(default)]
    context: Option<Spanned<ContextName>>,
    #[serde(default)]
    update: Option<RepoUpdatePolicy>,
}

/// `[repos]` table: discovery, fetch schedule and per-repository settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReposConfig {
    /// `roots`: directories walked to find repositories, besides every `match.paths`.
    pub roots: Vec<ConfigPath>,
    /// `max_depth`: directory levels walked below each root.
    pub max_depth: u32,
    /// `exclude`: glob patterns of directories not walked, besides the built-in ones.
    pub exclude: Vec<ExcludePattern>,
    /// `fetch_interval`: time between two fetches of a repository, before jitter.
    pub fetch_interval: FetchInterval,
    /// `fetch_concurrency`: fetches running at the same time, at least 1.
    pub fetch_concurrency: u32,
    /// `[repos."<path>"]` tables, sorted by key.
    pub overrides: Vec<RepoOverride>,
}

impl Default for ReposConfig {
    fn default() -> Self {
        Self {
            roots: DEFAULT_ROOTS
                .iter()
                .map(|root| ConfigPath((*root).to_owned()))
                .collect(),
            max_depth: DEFAULT_MAX_DEPTH,
            exclude: Vec::new(),
            fetch_interval: FetchInterval::default(),
            fetch_concurrency: DEFAULT_FETCH_CONCURRENCY,
            overrides: Vec::new(),
        }
    }
}

/// Fixed keys of `[repos]`, listed in the error for an unknown key.
const REPOS_KEYS: &str =
    "`roots`, `max_depth`, `exclude`, `fetch_interval`, `fetch_concurrency`, or a repository path";

/// A key of `[repos]`: a setting, or the path of a `[repos."<path>"]` table.
enum ReposKey {
    Roots,
    MaxDepth,
    Exclude,
    FetchInterval,
    FetchConcurrency,
    Repo(ConfigPath),
}

impl<'de> Deserialize<'de> for ReposKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Ok(match key.as_str() {
            "roots" => Self::Roots,
            "max_depth" => Self::MaxDepth,
            "exclude" => Self::Exclude,
            "fetch_interval" => Self::FetchInterval,
            "fetch_concurrency" => Self::FetchConcurrency,
            _ if key.starts_with(['/', '~']) => {
                Self::Repo(ConfigPath::try_from(key).map_err(D::Error::custom)?)
            }
            _ => {
                return Err(D::Error::custom(format!(
                    "unknown field `{key}`, expected {REPOS_KEYS}"
                )));
            }
        })
    }
}

/// `max_depth`: a whole number of levels.
struct MaxDepth(u32);

impl<'de> Deserialize<'de> for MaxDepth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        u32::try_from(value).map(Self).map_err(|_| {
            D::Error::custom(format!(
                "max_depth `{value}` must be between 0 and {}",
                u32::MAX
            ))
        })
    }
}

/// `fetch_concurrency`: at least 1.
struct FetchConcurrency(u32);

impl<'de> Deserialize<'de> for FetchConcurrency {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        u32::try_from(value)
            .ok()
            .filter(|value| *value >= 1)
            .map(Self)
            .ok_or_else(|| {
                D::Error::custom(format!(
                    "fetch_concurrency `{value}` must be between 1 and {}",
                    u32::MAX
                ))
            })
    }
}

impl<'de> Deserialize<'de> for ReposConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ReposVisitor)
    }
}

struct ReposVisitor;

impl<'de> Visitor<'de> for ReposVisitor {
    type Value = ReposConfig;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a [repos] table")
    }

    // Errors are raised while reading a key or a value, so that they carry its location.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut repos = ReposConfig::default();
        while let Some(key) = map.next_key_seed(SpannedSeed::<ReposKey>::default())? {
            let span = key.span();
            match key.into_inner() {
                ReposKey::Roots => repos.roots = map.next_value::<Roots>()?.0,
                ReposKey::MaxDepth => repos.max_depth = map.next_value::<MaxDepth>()?.0,
                ReposKey::Exclude => repos.exclude = map.next_value()?,
                ReposKey::FetchInterval => repos.fetch_interval = map.next_value()?,
                ReposKey::FetchConcurrency => {
                    repos.fetch_concurrency = map.next_value::<FetchConcurrency>()?.0;
                }
                ReposKey::Repo(path) => {
                    let body: RepoOverrideBody = map.next_value()?;
                    repos.overrides.push(RepoOverride {
                        path: Spanned::new(span, path),
                        context: body.context,
                        update: body.update,
                    });
                }
            }
        }
        repos
            .overrides
            .sort_by(|a, b| a.path.get_ref().cmp(b.path.get_ref()));
        Ok(repos)
    }
}

/// Deserializes a `Spanned<T>`: a seed, because `next_key` needs `Spanned<T>: Deserialize`
/// and keys are read through it.
struct SpannedSeed<T>(std::marker::PhantomData<T>);

impl<T> Default for SpannedSeed<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for SpannedSeed<T> {
    type Value = Spanned<T>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        Spanned::<T>::deserialize(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigPath, ForgeHost, HostPattern};
    use std::path::{Path, PathBuf};

    #[test]
    fn config_paths_are_absolute_or_home_relative() {
        for path in ["/", "/srv/git", "~", "~/work", "~/work/", "/a//b/./c"] {
            assert!(ConfigPath::try_from(path.to_owned()).is_ok(), "{path}");
        }
        for (path, needle) in [
            ("", "absolute"),
            ("work", "absolute"),
            ("./work", "absolute"),
            ("~user/work", "absolute"),
            ("/a/../b", "`..`"),
            ("~/..", "`..`"),
        ] {
            let error = ConfigPath::try_from(path.to_owned()).unwrap_err();
            assert!(error.contains(needle), "{path}: {error}");
        }
    }

    #[test]
    fn config_paths_expand_the_home_directory() {
        let home = Path::new("/home/me");
        for (path, expanded) in [
            ("~", "/home/me"),
            ("~/", "/home/me"),
            ("~/work", "/home/me/work"),
            ("~//work/./a/", "/home/me/work/a"),
            ("/srv/git/", "/srv/git"),
            ("/a//b/./c", "/a/b/c"),
        ] {
            let path = ConfigPath::try_from(path.to_owned()).unwrap();
            assert_eq!(path.expand(home), PathBuf::from(expanded), "{path}");
        }
    }

    #[test]
    fn normalized_paths_are_equal_for_two_spellings() {
        let normalized = |path: &str| ConfigPath::try_from(path.to_owned()).unwrap().normalized();
        assert_eq!(normalized("~/work"), normalized("~/work/"));
        assert_eq!(normalized("~/work"), normalized("~//./work"));
        assert_ne!(normalized("~/work"), normalized("/work"));
    }

    #[test]
    fn host_patterns_parse() {
        for (text, host, segments) in [
            ("github.com", "github.com", vec![]),
            ("github.com/", "github.com", vec![]),
            ("gitlab.com/company-a", "gitlab.com", vec!["company-a"]),
            (
                "gitlab.com/Company-A/Sub/",
                "gitlab.com",
                vec!["Company-A", "Sub"],
            ),
            (
                "gitlab.company-a.example",
                "gitlab.company-a.example",
                vec![],
            ),
        ] {
            let pattern = HostPattern::try_from(text.to_owned()).unwrap();
            assert_eq!(pattern.host(), host, "{text}");
            assert_eq!(pattern.segments(), segments.as_slice(), "{text}");
            assert_eq!(pattern.to_string(), text.trim_end_matches('/'));
        }
    }

    #[test]
    fn invalid_host_patterns_are_rejected() {
        for (text, needle) in [
            ("", "host name"),
            ("/company-a", "host name"),
            ("GitHub.com", "lowercase"),
            ("gitlab.com:2222/a", "without port"),
            ("https://github.com/me", "not a URL"),
            ("git@github.com", "not a URL"),
            ("github.com//me", "empty segment"),
            ("github.com/a b", "space"),
            (".github.com", "host name"),
        ] {
            let error = HostPattern::try_from(text.to_owned()).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
            assert!(error.contains("match.hosts"), "{text}: {error}");
        }
    }

    #[test]
    fn forge_hosts_are_lowercase_host_names() {
        assert!(ForgeHost::try_from("gitlab.company-a.example".to_owned()).is_ok());
        for host in [
            "",
            "GitLab.example",
            "gitlab.example/x",
            "gitlab.example:22",
        ] {
            let error = ForgeHost::try_from(host.to_owned()).unwrap_err();
            assert!(error.contains("[forges]"), "{host}: {error}");
        }
    }
}
