//! MR keys: `acme/shop#412` (GitHub), `company-a/checkout!88` (GitLab), and the directory name
//! of their mirror, `github-acme%2Fshop%23412`.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use loom_api::ForgeKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why a text is not an MR key or the directory name of one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MrKeyError {
    /// No `#` (GitHub) or `!` (GitLab) before the number.
    #[error("`{0}` is not an MR key: expected `<project>#<number>` or `<project>!<number>`")]
    NoSeparator(String),
    /// The number is not a positive integer.
    #[error("`{0}` is not an MR number: expected a positive integer")]
    Number(String),
    /// The project path is empty, has an empty, `.` or `..` segment, or a character that a
    /// forge does not allow.
    #[error("`{0}` is not a project path: expected segments of letters, digits, `-`, `_` and `.`")]
    Project(String),
    /// A GitHub project is `<owner>/<repository>`, exactly two segments.
    #[error("`{0}` is not a GitHub repository: expected `<owner>/<repository>`")]
    GithubProject(String),
    /// A GitLab project is in a group: at least two segments.
    #[error("`{0}` is not a GitLab project: expected `<group>/<project>`")]
    GitlabProject(String),
    /// The directory name of the key would be longer than [`MAX_DIR_NAME`] bytes.
    #[error("`{0}` is too long: its directory name is more than {MAX_DIR_NAME} bytes")]
    TooLong(String),
    /// The directory name does not start with `github-` or `gitlab-`, or is not encoded as
    /// [`MrKey::dir_name`] encodes it.
    #[error("`{0}` is not the directory name of an MR")]
    DirName(String),
}

/// The key of an MR: forge, project path and number. The key is an alias of the Review task.
///
/// The text form is `<project>#<number>` for GitHub and `<project>!<number>` for GitLab. The
/// separator gives the forge.
///
/// Both forges ignore the case of project paths, so two keys that differ only in the ASCII case
/// of the project are equal and have one directory name. The text form keeps the spelling it
/// was made with.
#[derive(Debug, Clone)]
pub struct MrKey {
    forge: ForgeKind,
    project: String,
    number: u64,
}

impl MrKey {
    /// The key of MR `number` of `project` on `forge`.
    ///
    /// # Errors
    ///
    /// [`MrKeyError`] when the number is 0 or the project path is not valid for the forge.
    pub fn new(forge: ForgeKind, project: &str, number: u64) -> Result<Self, MrKeyError> {
        if number == 0 {
            return Err(MrKeyError::Number(number.to_string()));
        }
        let segments = project_segments(project)?;
        match forge {
            ForgeKind::Github if segments != 2 => {
                return Err(MrKeyError::GithubProject(project.to_owned()));
            }
            ForgeKind::Gitlab if segments < 2 => {
                return Err(MrKeyError::GitlabProject(project.to_owned()));
            }
            ForgeKind::Github | ForgeKind::Gitlab => {}
        }
        let key = Self {
            forge,
            project: project.to_owned(),
            number,
        };
        if key.dir_name().len() > MAX_DIR_NAME {
            return Err(MrKeyError::TooLong(key.to_string()));
        }
        Ok(key)
    }

    /// Parses `acme/shop#412` (GitHub) or `company-a/checkout!88` (GitLab).
    ///
    /// # Errors
    ///
    /// [`MrKeyError`] when `text` is not a key.
    pub fn parse(text: &str) -> Result<Self, MrKeyError> {
        let (index, forge) = text
            .char_indices()
            .rev()
            .find_map(|(index, c)| match c {
                '#' => Some((index, ForgeKind::Github)),
                '!' => Some((index, ForgeKind::Gitlab)),
                _ => None,
            })
            .ok_or_else(|| MrKeyError::NoSeparator(text.to_owned()))?;
        let (project, number) = (&text[..index], &text[index + 1..]);
        Self::new(forge, project, parse_number(number)?)
    }

    /// The forge of the MR.
    #[must_use]
    pub const fn forge(&self) -> ForgeKind {
        self.forge
    }

    /// The path of the target project, such as `acme/shop`.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The number of the MR in its project (GitHub `number`, GitLab `iid`).
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// The name of the mirror directory: `<forge>-<key>` in ASCII lowercase, with every
    /// character other than an ASCII letter, digit, `-` or `_` encoded as `%XX`, such as
    /// `github-acme%2Fshop%23412`.
    ///
    /// The name has no `/`, no `.` and no `%` other than an encoded byte, so it is one path
    /// component and never `.` or `..`. It is at most [`MAX_DIR_NAME`] bytes, and the same on a
    /// file system that ignores case.
    #[must_use]
    pub fn dir_name(&self) -> String {
        let mut name = format!("{}-", self.forge);
        for byte in self.to_string().to_ascii_lowercase().bytes() {
            if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
                name.push(char::from(byte));
            } else {
                // Writing to a `String` cannot fail.
                let _ = write!(name, "%{byte:02X}");
            }
        }
        name
    }

    /// Parses a name made by [`Self::dir_name`].
    ///
    /// # Errors
    ///
    /// [`MrKeyError::DirName`] when `name` is not encoded as [`Self::dir_name`] encodes it, or
    /// does not name the forge of its key; another [`MrKeyError`] when the key is not valid.
    pub fn from_dir_name(name: &str) -> Result<Self, MrKeyError> {
        let error = || MrKeyError::DirName(name.to_owned());
        let (forge, encoded) = if let Some(rest) = name.strip_prefix("github-") {
            (ForgeKind::Github, rest)
        } else if let Some(rest) = name.strip_prefix("gitlab-") {
            (ForgeKind::Gitlab, rest)
        } else {
            return Err(error());
        };
        let key = Self::parse(&percent_decode(encoded).ok_or_else(error)?)?;
        // One spelling only: the name must be the one `dir_name` gives.
        if key.forge != forge || key.dir_name() != name {
            return Err(error());
        }
        Ok(key)
    }
}

/// The longest directory name of a key, in bytes: below the 255-byte limit of a path component,
/// with room for a suffix.
pub const MAX_DIR_NAME: usize = 200;

impl PartialEq for MrKey {
    fn eq(&self, other: &Self) -> bool {
        self.forge == other.forge
            && self.number == other.number
            && self.project.eq_ignore_ascii_case(&other.project)
    }
}

impl Eq for MrKey {}

impl std::hash::Hash for MrKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.forge.hash(state);
        self.number.hash(state);
        self.project.to_ascii_lowercase().hash(state);
    }
}

impl fmt::Display for MrKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let separator = match self.forge {
            ForgeKind::Github => '#',
            ForgeKind::Gitlab => '!',
        };
        write!(f, "{}{separator}{}", self.project, self.number)
    }
}

impl FromStr for MrKey {
    type Err = MrKeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for MrKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MrKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for MrKey {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MrKey".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "The key of an MR: `<project>#<number>` for GitHub (`<owner>/<repository>`), `<project>!<number>` for GitLab. Segments `.` and `..` are refused.",
            "pattern": r"^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)+[#!][1-9][0-9]*$",
        })
    }
}

/// The number of segments of `project`, a path of segments of ASCII letters, digits, `-`, `_`
/// and `.`, none empty, `.` or `..`.
fn project_segments(project: &str) -> Result<usize, MrKeyError> {
    let error = || MrKeyError::Project(project.to_owned());
    let mut count = 0;
    for segment in project.split('/') {
        let valid = !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !valid {
            return Err(error());
        }
        count += 1;
    }
    Ok(count)
}

/// A positive integer in decimal, without sign or leading zero.
fn parse_number(text: &str) -> Result<u64, MrKeyError> {
    let error = || MrKeyError::Number(text.to_owned());
    if text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(error());
    }
    text.parse().map_err(|_| error())
}

/// Decodes `%XX` sequences; `None` on a `%` without two hexadecimal digits or bytes that are
/// not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::{MrKey, MrKeyError};
    use loom_api::ForgeKind;

    #[test]
    fn parses_and_prints_the_keys_of_both_forges() {
        let github = MrKey::parse("acme/shop#412").unwrap();
        assert_eq!(github.forge(), ForgeKind::Github);
        assert_eq!(github.project(), "acme/shop");
        assert_eq!(github.number(), 412);
        assert_eq!(github.to_string(), "acme/shop#412");
        assert_eq!(github.dir_name(), "github-acme%2Fshop%23412");

        let gitlab = MrKey::parse("company-a/checkout!88").unwrap();
        assert_eq!(gitlab.forge(), ForgeKind::Gitlab);
        assert_eq!(gitlab.to_string(), "company-a/checkout!88");
        assert_eq!(gitlab.dir_name(), "gitlab-company-a%2Fcheckout%2188");

        let nested = MrKey::parse("group/sub.group/my_app!7").unwrap();
        assert_eq!(nested.project(), "group/sub.group/my_app");
        assert_eq!(nested.dir_name(), "gitlab-group%2Fsub%2Egroup%2Fmy_app%217");
        assert_eq!(MrKey::from_dir_name(&nested.dir_name()).unwrap(), nested);
    }

    #[test]
    fn refuses_texts_that_are_not_keys() {
        for (text, expected) in [
            ("acme/shop", MrKeyError::NoSeparator("acme/shop".to_owned())),
            ("acme/shop#", MrKeyError::Number(String::new())),
            ("acme/shop#0", MrKeyError::Number("0".to_owned())),
            ("acme/shop#012", MrKeyError::Number("012".to_owned())),
            ("acme/shop#+1", MrKeyError::Number("+1".to_owned())),
            (
                "acme/shop#99999999999999999999",
                MrKeyError::Number("99999999999999999999".to_owned()),
            ),
            ("acme#1", MrKeyError::GithubProject("acme".to_owned())),
            ("a/b/c#1", MrKeyError::GithubProject("a/b/c".to_owned())),
            (
                "checkout!1",
                MrKeyError::GitlabProject("checkout".to_owned()),
            ),
            ("#1", MrKeyError::Project(String::new())),
            ("/acme/shop#1", MrKeyError::Project("/acme/shop".to_owned())),
            ("acme/shop/#1", MrKeyError::Project("acme/shop/".to_owned())),
            ("acme//shop!1", MrKeyError::Project("acme//shop".to_owned())),
            (
                "acme/../shop!1",
                MrKeyError::Project("acme/../shop".to_owned()),
            ),
            (
                "acme/./shop!1",
                MrKeyError::Project("acme/./shop".to_owned()),
            ),
            ("acme/sh op#1", MrKeyError::Project("acme/sh op".to_owned())),
            (
                "acme/shop!1#2",
                MrKeyError::Project("acme/shop!1".to_owned()),
            ),
            ("acme/shöp#1", MrKeyError::Project("acme/shöp".to_owned())),
            (
                &format!("g/{}x!1", "a.".repeat(60)),
                MrKeyError::TooLong(format!("g/{}x!1", "a.".repeat(60))),
            ),
        ] {
            assert_eq!(MrKey::parse(text), Err(expected), "{text}");
        }
    }

    #[test]
    fn refuses_directory_names_that_dir_name_does_not_give() {
        for name in [
            "acme%2Fshop%23412",
            "github-",
            "github-acme%2Fshop%21412",
            "gitlab-acme%2Fshop%23412",
            "github-acme/shop%23412",
            "github-acme%2fshop%23412",
            "github-acme%2Fshop%2",
            "github-acme%2Fshop%ZZ412",
            "github-acme%2Fshop%FF412",
            "github-acme.x%2Fshop%23412",
        ] {
            assert!(MrKey::from_dir_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn keys_ignore_the_case_of_the_project() {
        let canonical = MrKey::parse("Acme/Shop#412").unwrap();
        let typed = MrKey::parse("acme/shop#412").unwrap();
        assert_eq!(canonical, typed);
        assert_eq!(canonical.to_string(), "Acme/Shop#412");
        assert_eq!(canonical.dir_name(), typed.dir_name());
        assert_eq!(canonical.dir_name(), "github-acme%2Fshop%23412");
        assert!(MrKey::from_dir_name("github-Acme%2Fshop%23412").is_err());
        assert_ne!(typed, MrKey::parse("acme/shop#413").unwrap());
        assert_ne!(typed, MrKey::parse("acme/shop!412").unwrap());
    }

    #[test]
    fn serializes_as_its_text() {
        let key = MrKey::parse("acme/shop#412").unwrap();
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"acme/shop#412\"");
        assert_eq!(serde_json::from_str::<MrKey>(&json).unwrap(), key);
        assert!(serde_json::from_str::<MrKey>("\"acme#412\"").is_err());
    }

    /// A small deterministic generator (xorshift64), so the property tests need no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> usize {
            usize::try_from(self.next() % bound).unwrap()
        }

        fn pick(&mut self, alphabet: &[u8]) -> u8 {
            alphabet[self.below(alphabet.len() as u64)]
        }
    }

    const SEGMENT: &[u8] = b"abcxyzABCXYZ0189-_.";
    const ANY: &[u8] = b"ab01-_./#!%: \\\0\xff";

    fn segment(rng: &mut Rng) -> String {
        loop {
            let len = 1 + rng.below(8);
            let text: String = (0..len).map(|_| char::from(rng.pick(SEGMENT))).collect();
            if text != "." && text != ".." {
                return text;
            }
        }
    }

    fn key(rng: &mut Rng) -> MrKey {
        let forge = if rng.next().is_multiple_of(2) {
            ForgeKind::Github
        } else {
            ForgeKind::Gitlab
        };
        let segments = match forge {
            ForgeKind::Github => 2,
            ForgeKind::Gitlab => 2 + rng.below(3),
        };
        let project: Vec<String> = (0..segments).map(|_| segment(rng)).collect();
        let number = 1 + rng.next() % 1_000_000;
        MrKey::new(forge, &project.join("/"), number).unwrap()
    }

    #[test]
    fn every_key_round_trips_through_its_text_and_its_directory_name() {
        let mut rng = Rng(0x5eed_1234_abcd_0001);
        for _ in 0..10_000 {
            let key = key(&mut rng);
            assert_eq!(MrKey::parse(&key.to_string()).unwrap(), key);
            let name = key.dir_name();
            assert_eq!(MrKey::from_dir_name(&name).unwrap(), key, "{name}");
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'%')),
                "{name}"
            );
            assert!(!name.contains(".."), "{name}");
            assert!(name.len() <= super::MAX_DIR_NAME, "{name}");
        }
    }

    #[test]
    fn any_text_parses_or_fails_without_panic_and_round_trips_when_it_parses() {
        let mut rng = Rng(0x0dd_ba11_cafe_f00d);
        for _ in 0..10_000 {
            let len = rng.below(16);
            let bytes: Vec<u8> = (0..len).map(|_| rng.pick(ANY)).collect();
            let text = String::from_utf8_lossy(&bytes);
            if let Ok(key) = MrKey::parse(&text) {
                assert_eq!(key.to_string(), text);
            }
            if let Ok(key) = MrKey::from_dir_name(&text) {
                assert_eq!(key.dir_name(), text);
            }
        }
    }
}
