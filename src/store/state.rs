//! The graph as one load read it: [`ProjectState`], the node maps keyed by
//! file stem with no edge index beside them, and [`NodeFile`], the name of
//! one of its files.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use super::schema::{COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, PROJECT_FILE, Project};

/// The graph as its files hold it, keyed by file stem (`"auth"`,
/// `"error-strategy"`). Each edge is a field of the node it leaves; no edge
/// index is kept beside the node maps.
#[derive(Debug, Clone)]
pub struct ProjectState {
    pub project: Project,
    pub components: BTreeMap<String, Component>,
    pub decisions: BTreeMap<String, Decision>,
    /// The files whose bytes are not the canonical form of what they hold,
    /// sorted: hand edits that `trurlic fmt` rewrites.
    pub non_canonical: Vec<NodeFile>,
    /// The tree the graph was read for, which anchors are relative to:
    /// the store's code root. Empty in a state a test builds, whose anchors
    /// then resolve against the working directory.
    pub(crate) code_root: PathBuf,
}

impl ProjectState {
    /// The path under `.trurlic/` of every file of the graph,
    /// `project.toml` first, then the components and the decisions by name.
    pub(super) fn files(&self) -> Vec<String> {
        let components = self.components.keys().map(|name| {
            NodePath {
                dir: COMPONENTS_DIR,
                name,
            }
            .to_string()
        });
        let decisions = self.decisions.keys().map(|name| {
            NodePath {
                dir: DECISIONS_DIR,
                name,
            }
            .to_string()
        });
        std::iter::once(PROJECT_FILE.to_owned())
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
        let (dir, name) = match self {
            Self::Project => return f.write_str(PROJECT_FILE),
            Self::Component(name) => (COMPONENTS_DIR, name),
            Self::Decision(name) => (DECISIONS_DIR, name),
        };
        NodePath { dir, name }.fmt(f)
    }
}

/// The path under `.trurlic/` of the node `name` of the kind `dir` holds.
struct NodePath<'a> {
    dir: &'static str,
    name: &'a str,
}

impl fmt::Display for NodePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}.toml", self.dir, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
