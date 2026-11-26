//! A git object name ([`Oid`]), checked when read.

use std::ffi::OsStr;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A git object name as git prints it: 40 lowercase hex digits, or 64 in
/// a SHA-256 repository. Checked when read, from git or from a journal,
/// so a name passed to git is never an option or a revision expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct Oid(String);

impl Oid {
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let hex = std::str::from_utf8(bytes).ok()?;
        Self::try_from(hex.to_owned()).ok()
    }

    /// The null name of this one's length, which `--index-info` reads as
    /// "no such path".
    pub(crate) fn null(&self) -> Self {
        Self("0".repeat(self.0.len()))
    }

    pub(super) fn is_null(&self) -> bool {
        self.0.bytes().all(|byte| byte == b'0')
    }

    pub(super) fn as_os_str(&self) -> &OsStr {
        OsStr::new(&self.0)
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl TryFrom<String> for Oid {
    type Error = &'static str;

    fn try_from(hex: String) -> std::result::Result<Self, &'static str> {
        let digits = hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if digits && matches!(hex.len(), 40 | 64) {
            Ok(Self(hex))
        } else {
            Err("an object name of 40 or 64 lowercase hex digits")
        }
    }
}

impl From<Oid> for String {
    fn from(oid: Oid) -> Self {
        oid.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_name_is_40_or_64_lowercase_hex_digits() {
        for good in ["a".repeat(40), "0123456789abcdef".repeat(4)] {
            assert!(Oid::try_from(good.clone()).is_ok(), "{good}");
        }
        for bad in [
            "a".repeat(39),
            "A".repeat(40),
            "g".repeat(40),
            format!("-{}", "a".repeat(39)),
            String::new(),
        ] {
            assert!(Oid::try_from(bad.clone()).is_err(), "{bad}");
        }
        let oid = Oid::try_from("ab".repeat(20)).unwrap();
        assert_eq!(oid.null().to_string(), "0".repeat(40));
        assert!(oid.null().is_null() && !oid.is_null());
    }
}
