use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

use super::schema::{COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, PROJECT_FILE, Project};

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

/// The stems of the `.toml` files in `dir`, sorted; none when `dir` is gone.
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
        let path = entry.map_err(Error::io(dir))?.path();
        let is_toml = path.extension().is_some_and(|ext| ext == "toml");
        if is_toml && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort_unstable();
    Ok(names)
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
