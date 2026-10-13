//! Task and event identifiers: 8 random Crockford base32 characters. Task IDs are shown as
//! the shortest unique prefix.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Number of characters of a task or event ID.
pub const ID_LEN: usize = 8;

/// Crockford base32, lowercase: no `i`, `l`, `o` or `u`.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Draws before an ID generator gives up on collisions. With 40 random bits, reaching it
/// means the random source is broken, not that the store is full.
const MAX_ATTEMPTS: usize = 16;

/// Source of randomness, injected so that ID generation is deterministic in tests.
pub trait RandomSource {
    /// Next 64 random bits.
    fn next_u64(&mut self) -> u64;
}

impl<F: FnMut() -> u64> RandomSource for F {
    fn next_u64(&mut self) -> u64 {
        self()
    }
}

/// Why an ID cannot be parsed or generated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The text is not 8 lowercase Crockford base32 characters.
    #[error("an ID is {ID_LEN} lowercase Crockford base32 characters, got `{0}`")]
    Invalid(String),
    /// Every draw collided with an existing ID.
    #[error("no free ID after {MAX_ATTEMPTS} draws: the random source is not random")]
    Exhausted,
}

/// Why a prefix does not resolve to exactly one task.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrefixError {
    /// The prefix is empty, too long, or has a character outside Crockford base32.
    #[error("a task ID prefix is 1 to {ID_LEN} Crockford base32 characters, got `{0}`")]
    Invalid(String),
    /// No task ID starts with the prefix.
    #[error("no task matches `{0}`")]
    Unknown(String),
    /// Several task IDs start with the prefix.
    #[error("`{prefix}` matches several tasks: {}", list(candidates))]
    Ambiguous {
        /// The prefix, normalized.
        prefix: String,
        /// Every matching ID, sorted.
        candidates: Vec<TaskId>,
    },
}

fn list(ids: &[TaskId]) -> String {
    ids.iter()
        .map(TaskId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Defines an ID type: 8 lowercase Crockford base32 characters, serialized as a string.
macro_rules! crockford_id {
    ($(#[$doc:meta])* $name:ident, $what:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Draws a new ID from `random`, retrying while it collides with one of `taken`.
            ///
            /// # Errors
            ///
            /// [`IdError::Exhausted`] when 16 draws in a row collide.
            pub fn generate(
                random: &mut impl RandomSource,
                taken: &[Self],
            ) -> Result<Self, IdError> {
                for _ in 0..MAX_ATTEMPTS {
                    let id = Self(from_bits(random.next_u64()));
                    if !taken.contains(&id) {
                        return Ok(id);
                    }
                }
                Err(IdError::Exhausted)
            }

            /// The ID as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            /// Accepts the canonical form only: 8 lowercase Crockford base32 characters.
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                if text.len() == ID_LEN && text.bytes().all(|byte| ALPHABET.contains(&byte)) {
                    Ok(Self(text.to_owned()))
                } else {
                    Err(IdError::Invalid(text.to_owned()))
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(D::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "description": concat!($what, ": 8 lowercase Crockford base32 characters, such as k3q7zm2a."),
                    "type": "string",
                    "pattern": "^[0-9a-hjkmnp-tv-z]{8}$"
                })
            }
        }
    };
}

crockford_id!(
    /// Identifier of a task: 8 lowercase Crockford base32 characters, such as `k3q7zm2a`.
    TaskId,
    "Task ID"
);

crockford_id!(
    /// Identifier of an event, unique within the log of its task: 8 lowercase Crockford base32
    /// characters. Undo refers to events by ID, which survives a union of logs.
    EventId,
    "Event ID"
);

/// 8 Crockford characters from the 40 low bits of `bits`, most significant first.
fn from_bits(bits: u64) -> String {
    (0..ID_LEN)
        .rev()
        .map(|position| {
            let index = ((bits >> (5 * position)) & 0x1f) as usize;
            char::from(ALPHABET[index])
        })
        .collect()
}

/// The shortest prefix of each ID that no other ID of `ids` starts with, in the order of `ids`.
///
/// An ID listed twice gets its full length.
#[must_use]
pub fn shortest_unique_prefixes(ids: &[TaskId]) -> Vec<&str> {
    let mut sorted: Vec<&TaskId> = ids.iter().collect();
    sorted.sort();
    let common = |a: &TaskId, b: &TaskId| {
        a.0.bytes()
            .zip(b.0.bytes())
            .take_while(|(x, y)| x == y)
            .count()
    };
    ids.iter()
        .map(|id| {
            let position = sorted.partition_point(|other| *other < id);
            let before = position.checked_sub(1).map_or(0, |i| common(sorted[i], id));
            // The first entry equal to `id` is at `position`; a duplicate follows it.
            let after = sorted.get(position + 1).map_or(0, |next| common(next, id));
            let length = (before.max(after) + 1).min(ID_LEN);
            &id.0[..length]
        })
        .collect()
}

/// The single ID of `ids` that starts with `prefix`.
///
/// The prefix is read as Crockford base32: case-insensitive, `i` and `l` read as `1`, `o` as `0`.
///
/// # Errors
///
/// [`PrefixError::Invalid`] for a malformed prefix, [`PrefixError::Unknown`] when no ID matches,
/// [`PrefixError::Ambiguous`] when several do.
pub fn resolve_prefix(prefix: &str, ids: &[TaskId]) -> Result<TaskId, PrefixError> {
    let normalized = normalize(prefix).ok_or_else(|| PrefixError::Invalid(prefix.to_owned()))?;
    let mut candidates: Vec<TaskId> = ids
        .iter()
        .filter(|id| id.0.starts_with(&normalized))
        .cloned()
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Err(PrefixError::Unknown(normalized)),
        1 => Ok(candidates.remove(0)),
        _ => Err(PrefixError::Ambiguous {
            prefix: normalized,
            candidates,
        }),
    }
}

/// Canonical lowercase form of a Crockford base32 prefix, or `None` if it is not one.
fn normalize(prefix: &str) -> Option<String> {
    if prefix.is_empty() || prefix.len() > ID_LEN {
        return None;
    }
    prefix
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            'i' | 'l' => Some('1'),
            'o' => Some('0'),
            other => u8::try_from(other)
                .ok()
                .filter(|byte| ALPHABET.contains(byte))
                .map(char::from),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> TaskId {
        text.parse().unwrap()
    }

    fn sequence(values: Vec<u64>) -> impl FnMut() -> u64 {
        let mut values = values.into_iter();
        move || values.next().unwrap()
    }

    #[test]
    fn generates_eight_crockford_characters_from_the_random_bits() {
        assert_eq!(TaskId::generate(&mut || 0, &[]).unwrap(), id("00000000"));
        assert_eq!(
            TaskId::generate(&mut || u64::MAX, &[]).unwrap(),
            id("zzzzzzzz")
        );
        // 40 low bits, 5 per character, most significant first: 1 and 31.
        assert_eq!(
            TaskId::generate(&mut || (1 << 35) | 31, &[]).unwrap(),
            id("1000000z")
        );
    }

    #[test]
    fn generation_is_deterministic_for_a_given_source() {
        let first = TaskId::generate(&mut sequence(vec![0x00de_adbe_ef42]), &[]).unwrap();
        let second = TaskId::generate(&mut sequence(vec![0x00de_adbe_ef42]), &[]).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_collision_draws_again() {
        let taken = [id("00000000")];
        let generated = TaskId::generate(&mut sequence(vec![0, 1]), &taken).unwrap();
        assert_eq!(generated, id("00000001"));
    }

    #[test]
    fn a_source_that_always_collides_is_reported() {
        let taken = [id("00000000")];
        assert_eq!(TaskId::generate(&mut || 0, &taken), Err(IdError::Exhausted));
    }

    #[test]
    fn parsing_accepts_only_the_canonical_form() {
        assert_eq!(id("k3q7zm2a").as_str(), "k3q7zm2a");
        for bad in [
            "",
            "k3q7zm2",
            "k3q7zm2a0",
            "K3Q7ZM2A",
            "k3q7zm2u",
            "k3q7zm2i",
        ] {
            assert_eq!(bad.parse::<TaskId>(), Err(IdError::Invalid(bad.to_owned())));
        }
    }

    #[test]
    fn serializes_as_a_string_and_rejects_invalid_ids() {
        let json = serde_json::to_string(&id("k3q7zm2a")).unwrap();
        assert_eq!(json, "\"k3q7zm2a\"");
        assert_eq!(
            serde_json::from_str::<TaskId>(&json).unwrap(),
            id("k3q7zm2a")
        );
        assert!(serde_json::from_str::<TaskId>("\"nope\"").is_err());
    }

    #[test]
    fn shortest_unique_prefixes_grow_with_collisions() {
        let ids = [
            id("k3q7zm2a"),
            id("k3x00000"),
            id("h8w2nc4d"),
            id("k4000000"),
        ];
        assert_eq!(shortest_unique_prefixes(&ids), ["k3q", "k3x", "h", "k4"]);
    }

    #[test]
    fn shortest_unique_prefixes_of_edge_cases() {
        assert_eq!(shortest_unique_prefixes(&[]), Vec::<&str>::new());
        assert_eq!(shortest_unique_prefixes(&[id("k3q7zm2a")]), ["k"]);
        let near = [id("k3q7zm2a"), id("k3q7zm2b")];
        assert_eq!(shortest_unique_prefixes(&near), ["k3q7zm2a", "k3q7zm2b"]);
        let twice = [id("k3q7zm2a"), id("k3q7zm2a"), id("h8w2nc4d")];
        assert_eq!(
            shortest_unique_prefixes(&twice),
            ["k3q7zm2a", "k3q7zm2a", "h"]
        );
    }

    #[test]
    fn every_shortest_prefix_resolves_to_its_id() {
        let ids = [
            id("k3q7zm2a"),
            id("k3x00000"),
            id("h8w2nc4d"),
            id("k4000000"),
        ];
        for (prefix, expected) in shortest_unique_prefixes(&ids).into_iter().zip(&ids) {
            assert_eq!(&resolve_prefix(prefix, &ids).unwrap(), expected);
        }
    }

    #[test]
    fn resolution_normalizes_crockford_input() {
        let ids = [id("k3q7zm2a"), id("10000000")];
        assert_eq!(resolve_prefix("K3Q", &ids).unwrap(), id("k3q7zm2a"));
        assert_eq!(resolve_prefix("lO", &ids).unwrap(), id("10000000"));
        assert_eq!(resolve_prefix("i0", &ids).unwrap(), id("10000000"));
    }

    #[test]
    fn resolution_reports_unknown_ambiguous_and_invalid_prefixes() {
        let ids = [id("k3q7zm2a"), id("k3x00000"), id("h8w2nc4d")];
        assert_eq!(
            resolve_prefix("z", &ids),
            Err(PrefixError::Unknown("z".to_owned()))
        );
        assert_eq!(
            resolve_prefix("K3", &ids),
            Err(PrefixError::Ambiguous {
                prefix: "k3".to_owned(),
                candidates: vec![id("k3q7zm2a"), id("k3x00000")],
            })
        );
        for bad in ["", "k3q7zm2a0", "k-3", "u", "é"] {
            assert_eq!(
                resolve_prefix(bad, &ids),
                Err(PrefixError::Invalid(bad.to_owned()))
            );
        }
    }

    #[test]
    fn a_duplicated_id_still_resolves() {
        let ids = [id("k3q7zm2a"), id("k3q7zm2a")];
        assert_eq!(resolve_prefix("k", &ids).unwrap(), id("k3q7zm2a"));
    }
}
