//! Shared by the fixture tests: the SHAs of the scenario and JSON helpers.

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test crate uses part of the helpers; a failing helper must fail the test"
)]

use loom_api::Timestamp;
use loom_forge::model::Sha;

/// `main`, base of version 1.
pub const M1: &str = "506ce3f338acb7125973db71a5c43fd062c4f046";
/// Head of version 1.
pub const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
/// `main` after a README change, base of version 2.
pub const M2: &str = "ee047f5164ae63069c7237b6f24c66d34f9eff26";
/// Head of version 2.
pub const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";

pub fn sha(text: &str) -> Sha {
    Sha::try_from(text.to_owned()).unwrap()
}

pub fn time(text: &str) -> Timestamp {
    serde_json::from_value(text.into()).unwrap()
}

/// `body` with the change of `change`, as JSON bytes.
pub fn edit(body: &[u8], change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(body).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}
