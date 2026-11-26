//! A graph's format version, `MAJOR.MINOR.PATCH`: ordered by its numbers,
//! independent of the crate's version, and read and written only in the
//! one form [`fmt::Display`] writes.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// A format version, `MAJOR.MINOR.PATCH` in decimal without leading zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FormatVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl FormatVersion {
    pub(crate) const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// `None` unless `text` is exactly the form [`fmt::Display`] writes, so
    /// `0.4`, `0.04.0`, `+0.4.0` and ` 0.4.0` are refused, never read as
    /// some other version.
    pub(in crate::store) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(|part| {
            let canonical = part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            part.parse::<u32>().ok().filter(|_| canonical)
        });
        let version = Self::new(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for FormatVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FormatVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(FormatVersionVisitor)
    }
}

struct FormatVersionVisitor;

impl Visitor<'_> for FormatVersionVisitor {
    type Value = FormatVersion;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a format version MAJOR.MINOR.PATCH such as 0.4.1")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> std::result::Result<FormatVersion, E> {
        FormatVersion::parse(text).ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_what_display_writes() {
        for (text, version) in [
            ("0.4.1", FormatVersion::new(0, 4, 1)),
            ("10.0.20", FormatVersion::new(10, 0, 20)),
            ("4294967295.0.0", FormatVersion::new(u32::MAX, 0, 0)),
        ] {
            assert_eq!(FormatVersion::parse(text), Some(version));
            assert_eq!(version.to_string(), text);
        }
    }

    #[test]
    fn parse_refuses_every_other_form() {
        for text in [
            "",
            "garbage",
            "0.4",
            "0.4.1.0",
            "0.04.1",
            "+0.4.1",
            "0.4.-1",
            " 0.4.1",
            "0.4.1 ",
            "0..1",
            "0.4.1-rc",
            "4294967296.0.0",
            "v0.4.1",
        ] {
            assert_eq!(FormatVersion::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_order_by_number_not_text() {
        assert!(FormatVersion::new(0, 10, 0) > FormatVersion::new(0, 9, 9));
        assert!(FormatVersion::new(1, 0, 0) > FormatVersion::new(0, 99, 99));
        assert!(FormatVersion::new(0, 4, 1) > FormatVersion::new(0, 4, 0));
    }
}
