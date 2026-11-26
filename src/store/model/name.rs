//! The rule a node's name meets: its file stem, kebab-case, bounded, and
//! not the name of the project-wide component. A load lists every stem and
//! validation reports one that breaks it; a write refuses one.

use std::fmt;

use super::limits::MAX_NAME_BYTES;
use super::schema::PROJECT_COMPONENT;

/// Why a stem is no node name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameFault {
    NotKebab,
    TooLong {
        len: usize,
    },
    /// The name of the project-wide component, which has no file.
    Reserved,
}

impl fmt::Display for NameFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotKebab => f.write_str(
                "is not kebab-case: lowercase ASCII letters and digits in words joined by one `-`",
            ),
            Self::TooLong { len } => {
                write!(f, "is {len} bytes, over the limit of {MAX_NAME_BYTES}")
            }
            Self::Reserved => write!(
                f,
                "is reserved: `{PROJECT_COMPONENT}` is the component of the project-wide decisions"
            ),
        }
    }
}

/// The first rule a node's name breaks: kebab-case, at most
/// [`MAX_NAME_BYTES`], not [`PROJECT_COMPONENT`].
#[must_use]
pub(in crate::store) fn name_fault(name: &str) -> Option<NameFault> {
    if !is_valid_kebab_case(name) {
        Some(NameFault::NotKebab)
    } else if name.len() > MAX_NAME_BYTES {
        Some(NameFault::TooLong { len: name.len() })
    } else if name == PROJECT_COMPONENT {
        Some(NameFault::Reserved)
    } else {
        None
    }
}

/// Non-empty words of lowercase ASCII letters and digits, each joined to
/// the next by one `-`.
fn is_valid_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_valid_names() {
        for name in [
            "auth",
            "rate-limiter",
            "mcp-server",
            "a",
            "component1",
            "my-app-v2",
        ] {
            assert!(is_valid_kebab_case(name), "{name}");
        }
    }

    #[test]
    fn kebab_rejects_invalid() {
        for name in [
            "",
            "-leading",
            "trailing-",
            "double--hyphen",
            "UpperCase",
            "has_underscore",
            "has space",
            "has.dot",
            "special!char",
        ] {
            assert!(!is_valid_kebab_case(name), "{name}");
        }
    }

    #[test]
    fn a_name_is_kebab_case_bounded_and_not_reserved() {
        assert_eq!(name_fault("rate-limiter"), None);
        assert_eq!(name_fault(&"a".repeat(MAX_NAME_BYTES)), None);
        assert_eq!(
            name_fault(&"a".repeat(MAX_NAME_BYTES + 1)),
            Some(NameFault::TooLong {
                len: MAX_NAME_BYTES + 1
            })
        );
        assert_eq!(name_fault("Rate_Limiter"), Some(NameFault::NotKebab));
        assert_eq!(name_fault(""), Some(NameFault::NotKebab));
        assert_eq!(name_fault(PROJECT_COMPONENT), Some(NameFault::Reserved));
    }
}
