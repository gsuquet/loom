//! Release archives: which entries are accepted ([`check_entry`], pure) and extraction into a
//! staging directory ([`extract`]).

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::EntryType;

/// Files a release archive holds, at its root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFile {
    /// The CLI.
    Loom,
    /// The daemon.
    Loomd,
    /// The license.
    License,
    /// The readme.
    Readme,
}

impl ArchiveFile {
    /// File name in the archive.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Loom => "loom",
            Self::Loomd => "loomd",
            Self::License => "LICENSE",
            Self::Readme => "README.md",
        }
    }

    /// Mode of the extracted file.
    #[must_use]
    pub const fn mode(self) -> u32 {
        match self {
            Self::Loom | Self::Loomd => 0o755,
            Self::License | Self::Readme => 0o644,
        }
    }

    const ALL: [Self; 4] = [Self::Loom, Self::Loomd, Self::License, Self::Readme];
}

/// Kind of a tar entry, as far as [`check_entry`] cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A pax global header: metadata only, nothing to extract.
    GlobalHeader,
    /// Anything else: directory, link, device, fifo…
    Other,
}

impl From<EntryType> for EntryKind {
    fn from(entry_type: EntryType) -> Self {
        if entry_type.is_file() {
            Self::File
        } else if entry_type.is_pax_global_extensions() {
            Self::GlobalHeader
        } else {
            Self::Other
        }
    }
}

/// Why an archive is rejected.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// An entry other than the four expected regular files at the root.
    #[error(
        "unexpected archive entry `{0}`: only the regular files loom, loomd, LICENSE and README.md are allowed"
    )]
    UnexpectedEntry(String),
    /// The same file twice.
    #[error("archive entry `{0}` appears more than once")]
    Duplicate(String),
    /// `loom` or `loomd` is missing.
    #[error("the archive has no `{0}`")]
    Missing(&'static str),
    /// Not a gzip-compressed tar archive, or truncated.
    #[error("cannot read the archive: {0}")]
    Read(#[source] io::Error),
    /// Writing to the staging directory failed.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// File being written.
        path: PathBuf,
        /// Cause.
        #[source]
        source: io::Error,
    },
}

/// What to do with an entry: extract it as an [`ArchiveFile`], skip it (`None`), or reject the
/// archive.
///
/// # Errors
///
/// [`ArchiveError::UnexpectedEntry`] for anything but a regular file named `loom`, `loomd`,
/// `LICENSE` or `README.md` at the root (no directory, no `./`, no link).
pub fn check_entry(path: &[u8], kind: EntryKind) -> Result<Option<ArchiveFile>, ArchiveError> {
    let rejected = || ArchiveError::UnexpectedEntry(String::from_utf8_lossy(path).into_owned());
    match kind {
        EntryKind::GlobalHeader => Ok(None),
        EntryKind::Other => Err(rejected()),
        EntryKind::File => ArchiveFile::ALL
            .into_iter()
            .find(|file| file.name().as_bytes() == path)
            .map(Some)
            .ok_or_else(rejected),
    }
}

/// Extracts the gzip-compressed tar archive `data` into the empty directory `dest`, after
/// checking every entry with [`check_entry`]. `loom` and `loomd` are required.
///
/// # Errors
///
/// [`ArchiveError`] on a rejected entry, a missing binary, or an I/O error. `dest` may then
/// hold some of the files: callers extract into a staging directory they throw away.
pub fn extract(data: impl Read, dest: &Path) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(GzDecoder::new(data));
    let mut seen = Vec::new();
    for entry in archive.entries().map_err(ArchiveError::Read)? {
        let mut entry = entry.map_err(ArchiveError::Read)?;
        let kind = EntryKind::from(entry.header().entry_type());
        let Some(file) = check_entry(&entry.path_bytes(), kind)? else {
            continue;
        };
        if seen.contains(&file) {
            return Err(ArchiveError::Duplicate(file.name().to_owned()));
        }
        seen.push(file);
        let path = dest.join(file.name());
        let write_error = |source| ArchiveError::Write {
            path: path.clone(),
            source,
        };
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(file.mode())
            .open(&path)
            .map_err(write_error)?;
        io::copy(&mut entry, &mut out).map_err(|error| {
            // A read error is the archive's fault, a write error the disk's.
            if error.kind() == io::ErrorKind::InvalidData
                || error.kind() == io::ErrorKind::UnexpectedEof
            {
                ArchiveError::Read(error)
            } else {
                write_error(error)
            }
        })?;
        out.sync_all().map_err(write_error)?;
    }
    for required in [ArchiveFile::Loom, ArchiveFile::Loomd] {
        if !seen.contains(&required) {
            return Err(ArchiveError::Missing(required.name()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    enum Fixture<'a> {
        File(&'a str, &'a [u8]),
        Symlink(&'a str, &'a str),
        Dir(&'a str),
    }

    fn archive(entries: &[Fixture<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Fixture::File(name, data) => {
                    header.set_entry_type(EntryType::Regular);
                    header.set_size(data.len() as u64);
                    header.set_mode(0o777);
                    // `set_path` refuses `..`: write the raw name.
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, *data).unwrap();
                }
                Fixture::Symlink(name, target) => {
                    header.set_entry_type(EntryType::Symlink);
                    header.set_size(0);
                    builder.append_link(&mut header, name, target).unwrap();
                }
                Fixture::Dir(name) => {
                    header.set_entry_type(EntryType::Directory);
                    header.set_size(0);
                    builder.append_data(&mut header, name, io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn release_files() -> Vec<Fixture<'static>> {
        vec![
            Fixture::File("loom", b"#!/bin/sh\n"),
            Fixture::File("loomd", b"#!/bin/sh\n"),
            Fixture::File("LICENSE", b"MIT"),
            Fixture::File("README.md", b"# loom"),
        ]
    }

    #[test]
    fn accepts_only_the_four_root_files() {
        assert!(matches!(
            check_entry(b"loom", EntryKind::File),
            Ok(Some(ArchiveFile::Loom))
        ));
        assert!(matches!(
            check_entry(b"README.md", EntryKind::File),
            Ok(Some(ArchiveFile::Readme))
        ));
        assert!(matches!(
            check_entry(b"pax_global_header", EntryKind::GlobalHeader),
            Ok(None)
        ));
        for (path, kind) in [
            (&b"./loom"[..], EntryKind::File),
            (b"bin/loom", EntryKind::File),
            (b"../loom", EntryKind::File),
            (b"/loom", EntryKind::File),
            (b"other", EntryKind::File),
            (b"loom", EntryKind::Other),
        ] {
            assert!(
                matches!(
                    check_entry(path, kind),
                    Err(ArchiveError::UnexpectedEntry(_))
                ),
                "{}",
                String::from_utf8_lossy(path)
            );
        }
    }

    #[test]
    fn extracts_with_release_modes() {
        let dir = tempfile::tempdir().unwrap();
        extract(&archive(&release_files())[..], dir.path()).unwrap();
        let mode = |name: &str| {
            fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("loom"), 0o755);
        assert_eq!(mode("loomd"), 0o755);
        assert_eq!(mode("LICENSE"), 0o644);
        assert_eq!(fs::read(dir.path().join("README.md")).unwrap(), b"# loom");
    }

    #[test]
    fn rejects_links_directories_paths_and_extra_files() {
        let bad: [Vec<Fixture<'static>>; 5] = [
            vec![Fixture::Symlink("loom", "/bin/sh")],
            vec![Fixture::Dir("bin/")],
            vec![Fixture::File("../loom", b"x")],
            vec![Fixture::File("bin/loomd", b"x")],
            vec![Fixture::File("extra", b"x")],
        ];
        for entries in bad {
            let dir = tempfile::tempdir().unwrap();
            let error = extract(&archive(&entries)[..], dir.path()).unwrap_err();
            assert!(matches!(error, ArchiveError::UnexpectedEntry(_)), "{error}");
            assert!(!dir.path().parent().unwrap().join("loom").exists());
        }
    }

    #[test]
    fn rejects_duplicates_and_missing_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = release_files();
        files.push(Fixture::File("loom", b"again"));
        assert!(matches!(
            extract(&archive(&files)[..], dir.path()),
            Err(ArchiveError::Duplicate(_))
        ));

        let dir = tempfile::tempdir().unwrap();
        let only_loom = [Fixture::File("loom", b"x")];
        assert!(matches!(
            extract(&archive(&only_loom)[..], dir.path()),
            Err(ArchiveError::Missing("loomd"))
        ));
    }

    #[test]
    fn rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            extract(&b"not a gzip stream"[..], dir.path()),
            Err(ArchiveError::Read(_))
        ));
    }
}
