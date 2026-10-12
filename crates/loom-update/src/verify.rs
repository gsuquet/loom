//! Verification of a release download: minisign signature of `SHA256SUMS`, then the
//! archive's SHA-256 against its `SHA256SUMS` line. Pure: bytes in, verdict out.

use std::fmt::Write as _;

use minisign_verify::{PublicKey, Signature};
use sha2::{Digest as _, Sha256};

/// `dist/minisign.pub`, embedded at build time.
const PUBLIC_KEY_FILE: &str = include_str!("../../../dist/minisign.pub");

/// Why a download is rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The public key cannot be decoded.
    #[error("invalid minisign public key: {0}")]
    PublicKey(String),
    /// The signature file cannot be decoded.
    #[error("invalid signature file SHA256SUMS.minisig: {0}")]
    SignatureFormat(String),
    /// The signature does not match `SHA256SUMS` and the public key.
    #[error("invalid signature on SHA256SUMS: {0}")]
    Signature(String),
    /// `SHA256SUMS` has no line for the archive.
    #[error("{0} is not listed in SHA256SUMS")]
    NotListed(String),
    /// `SHA256SUMS` has several lines, or a malformed line, for the archive.
    #[error("SHA256SUMS lists {0} more than once or with an invalid checksum")]
    BadListing(String),
    /// The archive's SHA-256 differs from `SHA256SUMS`.
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    Checksum {
        /// Archive name.
        name: String,
        /// Checksum in `SHA256SUMS`.
        expected: String,
        /// Checksum of the download.
        actual: String,
    },
}

/// Base64 public key of the release signing key: the second line of `dist/minisign.pub`.
#[must_use]
pub fn embedded_public_key() -> &'static str {
    public_key_line(PUBLIC_KEY_FILE).unwrap_or_default()
}

/// The key line of a minisign public key file (`untrusted comment: …`, then the key).
#[must_use]
pub fn public_key_line(file: &str) -> Option<&str> {
    file.lines()
        .nth(1)
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

/// Verifies the minisign signature `minisig` of `sums` with the base64 public key `public_key`.
/// Only prehashed signatures (the minisign default) are accepted.
///
/// # Errors
///
/// [`VerifyError`] on a bad key, a malformed signature or a signature that does not verify.
pub fn verify_signature(public_key: &str, sums: &[u8], minisig: &str) -> Result<(), VerifyError> {
    let key =
        PublicKey::from_base64(public_key).map_err(|e| VerifyError::PublicKey(e.to_string()))?;
    let signature =
        Signature::decode(minisig).map_err(|e| VerifyError::SignatureFormat(e.to_string()))?;
    key.verify(sums, &signature, false)
        .map_err(|e| VerifyError::Signature(e.to_string()))
}

/// Lowercase hex SHA-256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Checksum of `name` in `sums` (`sha256sum` format: `<hex>  <name>`, or `<hex> *<name>`),
/// lowercased.
///
/// # Errors
///
/// [`VerifyError::NotListed`] or [`VerifyError::BadListing`].
pub fn listed_checksum(sums: &str, name: &str) -> Result<String, VerifyError> {
    let mut found = sums.lines().filter_map(|line| {
        let (hash, file) = line.split_once(' ')?;
        let file = file.strip_prefix(' ').or_else(|| file.strip_prefix('*'))?;
        (file == name).then_some(hash)
    });
    let hash = found
        .next()
        .ok_or_else(|| VerifyError::NotListed(name.to_owned()))?;
    let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid || found.next().is_some() {
        return Err(VerifyError::BadListing(name.to_owned()));
    }
    Ok(hash.to_ascii_lowercase())
}

/// Checks that `data`, the archive `name`, matches its line in `sums`.
///
/// # Errors
///
/// [`VerifyError`] when the archive is not listed once or its checksum differs.
pub fn verify_checksum(sums: &str, name: &str, data: &[u8]) -> Result<(), VerifyError> {
    let expected = listed_checksum(sums, name)?;
    let actual = sha256_hex(data);
    if actual != expected {
        return Err(VerifyError::Checksum {
            name: name.to_owned(),
            expected,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `printf abc | sha256sum`
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn embedded_key_is_the_committed_one() {
        assert_eq!(
            embedded_public_key(),
            "RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+"
        );
        assert!(PublicKey::from_base64(embedded_public_key()).is_ok());
    }

    #[test]
    fn public_key_line_is_the_second_line() {
        assert_eq!(public_key_line("comment\nKEY\n"), Some("KEY"));
        assert_eq!(public_key_line("comment\n"), None);
        assert_eq!(public_key_line("comment\n\n"), None);
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(sha256_hex(b"abc"), ABC);
    }

    #[test]
    fn checksum_lines_in_both_formats() {
        let sums = format!("{ABC}  a.tar.gz\n{}  b.tar.gz\n", "0".repeat(64));
        assert_eq!(listed_checksum(&sums, "a.tar.gz"), Ok(ABC.to_owned()));
        let binary = format!("{} *a.tar.gz\n", ABC.to_uppercase());
        assert_eq!(listed_checksum(&binary, "a.tar.gz"), Ok(ABC.to_owned()));
    }

    #[test]
    fn checksum_listing_errors() {
        let name = "a.tar.gz";
        assert_eq!(
            listed_checksum("", name),
            Err(VerifyError::NotListed(name.to_owned()))
        );
        // Another file whose name ends with the archive name does not match.
        assert_eq!(
            listed_checksum(&format!("{ABC}  xa.tar.gz\n"), name),
            Err(VerifyError::NotListed(name.to_owned()))
        );
        for sums in [
            format!("{ABC}  {name}\n{ABC}  {name}\n"),
            format!("abc  {name}\n"),
            format!("{}  {name}\n", "z".repeat(64)),
        ] {
            assert_eq!(
                listed_checksum(&sums, name),
                Err(VerifyError::BadListing(name.to_owned())),
                "{sums}"
            );
        }
    }

    #[test]
    fn checksum_mismatch_is_reported() {
        let sums = format!("{ABC}  a.tar.gz\n");
        assert_eq!(verify_checksum(&sums, "a.tar.gz", b"abc"), Ok(()));
        let error = verify_checksum(&sums, "a.tar.gz", b"xyz").unwrap_err();
        assert!(matches!(error, VerifyError::Checksum { .. }), "{error}");
    }

    // Throwaway minisign pair, made once with `minisign -G -W`; its secret half was discarded.
    const TEST_PUBLIC: &str = "RWT+0wbAfqpMkNo/98fx3OKwXndFbrfpkz+7eBOmqX9GosEGj6MDWyku";
    const TEST_SIGNED: &[u8] = b"abc  a.tar.gz\n";
    const TEST_MINISIG: &str = "untrusted comment: signature from minisign secret key\n\
        RUT+0wbAfqpMkIe2BzDmxL5shgacgLaikzD7gjD1YTAHNSsOWdKtn2h39t1ac3K8TNa2x/3ZxSLal1uSTtwR1dgV+udFFY+d7AE=\n\
        trusted comment: timestamp:1791820637\tfile:sums\thashed\n\
        eVVKp7jOz9DtnvKEFDGVI6BjAtJQwvV2JikL1n/L67dFo8IS6W4bcp96nsoI0tnpzbN6xdCEMUamgtk2hH3/AQ==\n";

    #[test]
    fn valid_signature_verifies() {
        assert_eq!(
            verify_signature(TEST_PUBLIC, TEST_SIGNED, TEST_MINISIG),
            Ok(())
        );
    }

    #[test]
    fn tampered_data_or_wrong_key_fails() {
        assert!(matches!(
            verify_signature(TEST_PUBLIC, b"xyz  a.tar.gz\n", TEST_MINISIG),
            Err(VerifyError::Signature(_))
        ));
        // Signed by another key: the release key must not accept it.
        assert!(matches!(
            verify_signature(embedded_public_key(), TEST_SIGNED, TEST_MINISIG),
            Err(VerifyError::Signature(_))
        ));
    }

    #[test]
    fn malformed_signature_and_key_are_rejected() {
        assert!(matches!(
            verify_signature("not a key", b"x", "x"),
            Err(VerifyError::PublicKey(_))
        ));
        assert!(matches!(
            verify_signature(embedded_public_key(), b"x", "garbage"),
            Err(VerifyError::SignatureFormat(_))
        ));
    }
}
