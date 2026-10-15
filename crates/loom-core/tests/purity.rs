//! loom-core stays pure: no filesystem, process, network, clock, randomness or async runtime.
//!
//! The sources are embedded at compile time; a new module must be added to `SOURCES`.

const SOURCES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../Cargo.toml")),
    ("src/lib.rs", include_str!("../src/lib.rs")),
    ("src/archive.rs", include_str!("../src/archive.rs")),
    ("src/command.rs", include_str!("../src/command.rs")),
    ("src/context.rs", include_str!("../src/context.rs")),
    ("src/event.rs", include_str!("../src/event.rs")),
    ("src/event_log.rs", include_str!("../src/event_log.rs")),
    ("src/id.rs", include_str!("../src/id.rs")),
    ("src/priority.rs", include_str!("../src/priority.rs")),
    ("src/relations.rs", include_str!("../src/relations.rs")),
    ("src/task.rs", include_str!("../src/task.rs")),
    ("src/task_file.rs", include_str!("../src/task_file.rs")),
    ("src/timestamp.rs", include_str!("../src/timestamp.rs")),
];

const FORBIDDEN: &[&str] = &[
    "std::fs",
    "std::process",
    "std::net",
    "std::thread",
    "std::env",
    "SystemTime",
    "Instant::",
    "::now(",
    "jiff::tz",
    "Zoned",
    "async fn",
    "async move",
    ".await",
    "tokio",
    "rand::",
    "getrandom",
    "reqwest",
    "ureq",
];

#[test]
fn the_crate_has_no_io_clock_randomness_or_async_runtime() {
    for &(file, source) in SOURCES {
        for token in FORBIDDEN {
            assert!(!source.contains(token), "{file} uses `{token}`");
        }
    }
}

#[test]
fn every_module_is_checked() {
    let declared = SOURCES[1]
        .1
        .lines()
        .filter(|line| line.starts_with("mod "))
        .count();
    assert_eq!(declared + 2, SOURCES.len(), "add the new module to SOURCES");
}
