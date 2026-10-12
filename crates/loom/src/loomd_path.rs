//! Where `loom` looks for the `loomd` binary, decided without I/O.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// File name of the daemon binary.
pub const LOOMD_BIN: &str = "loomd";

/// Paths where `loomd` may be, in the order to try them: the `loomd` next to the `loom`
/// executable `current_exe`, then `loomd` in each entry of `path_env` (`PATH`), in order.
///
/// Empty `PATH` entries are skipped, and a path already listed is not repeated. The caller
/// picks the first candidate that exists and is a file.
#[must_use]
pub fn loomd_candidates(current_exe: &Path, path_env: Option<&OsStr>) -> Vec<PathBuf> {
    let sibling = current_exe.parent().map(|dir| dir.join(LOOMD_BIN));
    let from_path = path_env
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(LOOMD_BIN));

    let mut candidates: Vec<PathBuf> = Vec::new();
    for candidate in sibling.into_iter().chain(from_path) {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::loomd_candidates;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    fn candidates(exe: &str, path: Option<&str>) -> Vec<PathBuf> {
        loomd_candidates(Path::new(exe), path.map(OsStr::new))
    }

    #[test]
    fn sibling_comes_first_then_path_in_order() {
        assert_eq!(
            candidates("/opt/loom/bin/loom", Some("/usr/local/bin:/usr/bin")),
            [
                "/opt/loom/bin/loomd",
                "/usr/local/bin/loomd",
                "/usr/bin/loomd"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn duplicates_are_listed_once() {
        assert_eq!(
            candidates("/usr/bin/loom", Some("/usr/bin:/bin:/usr/bin:/bin")),
            ["/usr/bin/loomd", "/bin/loomd"].map(PathBuf::from)
        );
    }

    #[test]
    fn empty_path_entries_are_skipped() {
        assert_eq!(
            candidates("/opt/loom", Some(":/bin::")),
            ["/opt/loomd", "/bin/loomd"].map(PathBuf::from)
        );
    }

    #[test]
    fn without_path_only_the_sibling() {
        assert_eq!(
            candidates("/opt/loom/loom", None),
            [PathBuf::from("/opt/loom/loomd")]
        );
        assert_eq!(
            candidates("/opt/loom/loom", Some("")),
            [PathBuf::from("/opt/loom/loomd")]
        );
    }

    #[test]
    fn executable_without_parent_gives_only_path_entries() {
        assert_eq!(candidates("/", Some("/bin")), [PathBuf::from("/bin/loomd")]);
    }
}
