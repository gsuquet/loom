//! Durations written as text in the configuration: `"6h"`, `"1h30m"`, `"10m"`, `"45s"`.

use std::time::Duration;

use thiserror::Error;

/// Units accepted, with their length in seconds, from the largest.
const UNITS: [(char, u64); 4] = [('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];

/// A duration that cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "invalid duration `{text}`: expected whole numbers with a unit among d, h, m, s, such as \"6h\" or \"1h30m\""
)]
pub struct DurationError {
    /// The text given.
    pub text: String,
}

/// Parses one or more `<integer><unit>` parts, units `d`, `h`, `m`, `s`, in any order, summed.
/// No sign, no fraction, no spaces. Pure.
///
/// # Errors
///
/// [`DurationError`] on an empty text, a missing unit or number, an unknown unit, or an
/// overflow.
pub fn parse_duration(text: &str) -> Result<Duration, DurationError> {
    let error = || DurationError {
        text: text.to_owned(),
    };
    if text.is_empty() {
        return Err(error());
    }
    let mut total: u64 = 0;
    let mut number: Option<u64> = None;
    for c in text.chars() {
        if let Some(digit) = c.to_digit(10) {
            let value = number
                .unwrap_or(0)
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(digit)))
                .ok_or_else(error)?;
            number = Some(value);
            continue;
        }
        let (_, secs) = UNITS
            .iter()
            .find(|(unit, _)| *unit == c)
            .ok_or_else(error)?;
        let part = number
            .take()
            .ok_or_else(error)?
            .checked_mul(*secs)
            .ok_or_else(error)?;
        total = total.checked_add(part).ok_or_else(error)?;
    }
    // Digits without a unit at the end.
    if number.is_some() {
        return Err(error());
    }
    Ok(Duration::from_secs(total))
}

/// Text form of `duration`, in whole seconds, largest units first: `6h`, `1h30m`, `0s`.
/// [`parse_duration`] reads it back. Pure.
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let mut secs = duration.as_secs();
    if secs == 0 {
        return "0s".to_owned();
    }
    let mut text = String::new();
    for (unit, length) in UNITS {
        if secs >= length {
            text.push_str(&(secs / length).to_string());
            text.push(unit);
            secs %= length;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{format_duration, parse_duration};
    use std::time::Duration;

    #[test]
    fn parses_units_and_sums() {
        for (text, secs) in [
            ("6h", 21_600),
            ("10m", 600),
            ("45s", 45),
            ("1d", 86_400),
            ("1h30m", 5_400),
            ("30m1h", 5_400),
            ("0s", 0),
            ("90m", 5_400),
        ] {
            assert_eq!(
                parse_duration(text),
                Ok(Duration::from_secs(secs)),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_malformed_text() {
        for text in [
            "", "6", "h", "6 h", "-6h", "6.5h", "6H", "6hours", "6x", "1h30", " 6h",
        ] {
            let error = parse_duration(text).unwrap_err();
            assert_eq!(error.text, text);
            assert!(error.to_string().contains("\"1h30m\""), "{error}");
        }
    }

    #[test]
    fn rejects_overflow() {
        assert!(parse_duration("99999999999999999999s").is_err());
        assert!(parse_duration("9999999999999999d").is_err());
    }

    #[test]
    fn formats_largest_units_first() {
        for (secs, text) in [
            (0, "0s"),
            (45, "45s"),
            (600, "10m"),
            (5_400, "1h30m"),
            (21_600, "6h"),
            (90_061, "1d1h1m1s"),
        ] {
            assert_eq!(format_duration(Duration::from_secs(secs)), text);
            assert_eq!(parse_duration(text), Ok(Duration::from_secs(secs)));
        }
    }
}
