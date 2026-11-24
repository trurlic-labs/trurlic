use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

use super::limits::MAX_NAME_BYTES;
use super::schema::{
    COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, PROJECT_COMPONENT, PROJECT_FILE, Project,
};

// -- ProjectState -------------------------------------------------------------

/// The graph as its files hold it, keyed by file stem (`"auth"`,
/// `"error-strategy"`). Each edge is a field of the node it leaves, so the
/// state is the node files and nothing derived from them.
#[derive(Debug, Clone)]
pub struct ProjectState {
    pub project: Project,
    pub components: BTreeMap<String, Component>,
    pub decisions: BTreeMap<String, Decision>,
    /// The files whose bytes are not the canonical form of what they hold,
    /// sorted: hand edits that `trurlic fmt` rewrites.
    pub non_canonical: Vec<NodeFile>,
    /// The tree the graph was read for, which anchors are relative to:
    /// the store's code root. Empty in a state a test builds, which checks
    /// no anchor on disk.
    pub(crate) code_root: PathBuf,
}

impl ProjectState {
    /// Every file of the graph, `project.toml` first, then the components
    /// and the decisions by name.
    #[must_use]
    pub fn files(&self) -> Vec<NodeFile> {
        let components = self.components.keys().cloned().map(NodeFile::Component);
        let decisions = self.decisions.keys().cloned().map(NodeFile::Decision);
        std::iter::once(NodeFile::Project)
            .chain(components)
            .chain(decisions)
            .collect()
    }
}

/// A file of the graph, as a path under `.trurlic/` names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeFile {
    Project,
    Component(String),
    Decision(String),
}

impl fmt::Display for NodeFile {
    /// The path under `.trurlic/`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project => f.write_str(PROJECT_FILE),
            Self::Component(name) => write!(f, "{COMPONENTS_DIR}/{name}.toml"),
            Self::Decision(name) => write!(f, "{DECISIONS_DIR}/{name}.toml"),
        }
    }
}

// -- Name validation ----------------------------------------------------------

/// Why a stem is no node name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameFault {
    /// Not kebab-case: see [`is_valid_kebab_case`].
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
pub(super) fn name_fault(name: &str) -> Option<NameFault> {
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

/// Check whether a name is valid kebab-case.
/// Rules: non-empty, lowercase ASCII letters + digits + hyphens only,
/// no leading/trailing/consecutive hyphens.
#[must_use]
pub fn is_valid_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Reject ASCII control characters that could corrupt TOML files or
/// produce surprising on-disk content. Allows common whitespace
/// (newline, carriage return, tab).
#[must_use]
pub fn has_control_chars(s: &str) -> bool {
    s.bytes()
        .any(|b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
}

// -- Directory listing --------------------------------------------------------

/// The stems of the node files in `dir`, sorted; none when `dir` is gone.
/// A node file is every entry whose name ends in `.toml`, as the history's
/// pathspecs match them, so none is skipped: a name that is not UTF-8 is
/// [`Error::NodeFileName`]. A stem that is no node name is listed, and
/// validation reports it; an entry that is no regular file is refused by
/// the read of it (`load::read_node_bytes`), which runs in parallel.
pub(super) fn list_toml_stems(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::io(dir))?;
        let name = entry.file_name();
        if !name.as_encoded_bytes().ends_with(b".toml") {
            continue;
        }
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".toml")) else {
            return Err(Error::NodeFileName { path: entry.path() });
        };
        names.push(stem.to_owned());
    }
    names.sort_unstable();
    Ok(names)
}

/// Refuse `path` unless it is a regular file once its symlinks are
/// followed: checked before it is opened, so a FIFO never blocks a read.
pub(super) fn ensure_regular(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).map_err(Error::io(path))?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(Error::NotAFile {
            path: path.to_path_buf(),
        })
    }
}

// -- Tests --------------------------------------------------------------------

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

    #[test]
    fn control_chars_are_refused_and_whitespace_and_unicode_pass() {
        for text in ["hello\x00world", "alert\x07", "\x1b[31m red"] {
            assert!(has_control_chars(text), "{text:?}");
        }
        for text in ["line\nnext\r\nand\ttab", "", "caf\u{e9} na\u{ef}ve"] {
            assert!(!has_control_chars(text), "{text:?}");
        }
    }

    #[test]
    fn a_node_file_displays_as_its_path_under_the_store() {
        assert_eq!(NodeFile::Project.to_string(), "project.toml");
        assert_eq!(
            NodeFile::Component("auth".into()).to_string(),
            "components/auth.toml"
        );
        assert_eq!(
            NodeFile::Decision("use-jwt".into()).to_string(),
            "decisions/use-jwt.toml"
        );
    }
}
