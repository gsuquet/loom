//! `LOOM_HOME/machine.toml`: the name of this machine (ADR-0013, decision 8). Local and
//! unversioned, unlike `config.toml`, which is the same on every machine.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Deserializer};

use crate::config::{ConfigError, location};

/// Longest machine name, in bytes.
pub const MACHINE_NAME_MAX_LEN: usize = 64;

/// Name given to a machine whose host name is empty.
pub const UNKNOWN_MACHINE: &str = "unknown";

/// The name of a machine: 1 to [`MACHINE_NAME_MAX_LEN`] bytes, without whitespace or control
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineName(String);

impl MachineName {
    /// The name, as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of a machine whose host name is `host`: the host name with whitespace and
    /// control characters removed and cut to [`MACHINE_NAME_MAX_LEN`] bytes, or
    /// [`UNKNOWN_MACHINE`] when nothing is left.
    #[must_use]
    pub fn from_host_name(host: &str) -> Self {
        let mut name = String::new();
        for c in host.chars().filter(|c| valid_char(*c)) {
            if name.len() + c.len_utf8() > MACHINE_NAME_MAX_LEN {
                break;
            }
            name.push(c);
        }
        if name.is_empty() {
            UNKNOWN_MACHINE.to_owned()
        } else {
            name
        }
        .parse()
        .unwrap_or_else(|_| Self(UNKNOWN_MACHINE.to_owned()))
    }
}

const fn valid_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control()
}

impl std::str::FromStr for MachineName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err("invalid machine name: empty".to_owned());
        }
        if name.len() > MACHINE_NAME_MAX_LEN {
            return Err(format!(
                "invalid machine name `{name}`: longer than {MACHINE_NAME_MAX_LEN} bytes"
            ));
        }
        if !name.chars().all(valid_char) {
            return Err(format!(
                "invalid machine name `{name}`: whitespace and control characters are not allowed"
            ));
        }
        Ok(Self(name.to_owned()))
    }
}

impl fmt::Display for MachineName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MachineName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Content of `machine.toml`. Unknown keys are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    /// `id`: the name of this machine, matched against `[contexts.<name>] machines` and written
    /// as the `machine_id` of every event.
    pub id: MachineName,
}

/// Parses the content of `machine.toml` at `path`.
///
/// # Errors
///
/// [`ConfigError`] on a TOML syntax error, an unknown key, a missing or invalid `id`.
pub fn parse_machine(text: &str, path: &Path) -> Result<MachineConfig, ConfigError> {
    toml::from_str(text).map_err(|error: toml::de::Error| ConfigError {
        path: path.to_path_buf(),
        location: error.span().map(|span| location(text, span.start)),
        message: error.message().trim_end().to_owned(),
    })
}

/// The `machine.toml` written at first start for the machine `id`.
#[must_use]
pub fn render_machine(id: &MachineName) -> String {
    // A machine name has no control character: escaping `\` and `"` gives a TOML basic string.
    let escaped = id.as_str().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "# Name of this machine, local to it: matched against `[contexts.<name>] machines`\n# in config.toml and recorded on every event.\nid = \"{escaped}\"\n"
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{MACHINE_NAME_MAX_LEN, MachineConfig, MachineName, parse_machine, render_machine};

    const PATH: &str = "/home/me/.loom/machine.toml";

    fn name(text: &str) -> MachineName {
        text.parse().unwrap()
    }

    #[test]
    fn machine_toml_names_the_machine() {
        assert_eq!(
            parse_machine("id = \"laptop\"\n", Path::new(PATH)),
            Ok(MachineConfig { id: name("laptop") })
        );
    }

    #[test]
    fn missing_id_unknown_key_and_invalid_name_are_rejected() {
        let missing = parse_machine("", Path::new(PATH)).unwrap_err();
        assert!(missing.message.contains("missing field `id`"), "{missing}");
        let unknown = parse_machine("id = \"a\"\nhost = \"b\"\n", Path::new(PATH)).unwrap_err();
        assert_eq!(unknown.location.map(|at| at.line), Some(2));
        assert!(
            unknown.message.contains("unknown field `host`"),
            "{unknown}"
        );
        let blank = parse_machine("id = \"my laptop\"\n", Path::new(PATH)).unwrap_err();
        assert!(blank.message.contains("whitespace"), "{blank}");
        let empty = parse_machine("id = \"\"\n", Path::new(PATH)).unwrap_err();
        assert!(empty.message.contains("empty"), "{empty}");
    }

    #[test]
    fn names_are_bounded_and_without_whitespace() {
        assert!(
            "a".repeat(MACHINE_NAME_MAX_LEN)
                .parse::<MachineName>()
                .is_ok()
        );
        assert!(
            "a".repeat(MACHINE_NAME_MAX_LEN + 1)
                .parse::<MachineName>()
                .is_err()
        );
        assert!("a\tb".parse::<MachineName>().is_err());
        assert!("a\u{7}".parse::<MachineName>().is_err());
        assert_eq!(name("Mac-mini.local").as_str(), "Mac-mini.local");
    }

    #[test]
    fn host_names_become_valid_machine_names() {
        assert_eq!(MachineName::from_host_name("laptop"), name("laptop"));
        assert_eq!(MachineName::from_host_name("my laptop\n"), name("mylaptop"));
        assert_eq!(MachineName::from_host_name(""), name("unknown"));
        assert_eq!(MachineName::from_host_name(" \t"), name("unknown"));
        let long = "é".repeat(MACHINE_NAME_MAX_LEN);
        let cut = MachineName::from_host_name(&long);
        assert_eq!(cut.as_str().len(), MACHINE_NAME_MAX_LEN);
    }

    #[test]
    fn the_rendered_file_parses_back() {
        for id in ["laptop", "a\"quote", "back\\slash"] {
            let text = render_machine(&name(id));
            assert!(text.starts_with("# Name of this machine"), "{text}");
            assert_eq!(
                parse_machine(&text, Path::new(PATH)).unwrap().id,
                name(id),
                "{text}"
            );
        }
    }
}
