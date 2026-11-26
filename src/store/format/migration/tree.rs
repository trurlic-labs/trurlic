//! The tree a migration rewrites: every graph file of any format, read as
//! a TOML table, and what the running step drops and reports. The keys
//! that have held the format version are read here, so a step stamps its
//! version under the key the store has.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::store::format::Store;
use crate::store::format::load::{list_toml_stems, read_table};
use crate::store::model::{COMPONENTS_DIR, DECISIONS_DIR, FormatVersion, PROJECT_FILE, Placement};
use crate::{Error, Result};

use super::placement::PlacementSource;
use super::report::{Fact, Loss, PlacementBasis};

/// The index of formats before 0.5, read so a step can move its edges.
pub(in crate::store) const GRAPH_FILE: &str = "graph.toml";

/// The pattern nodes of formats before 0.5.
pub(in crate::store) const PATTERNS_DIR: &str = "patterns";

/// A graph file of any format.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::store) enum File {
    Project,
    Index,
    Component(String),
    Decision(String),
    Pattern(String),
}

impl File {
    pub(in crate::store) fn path(&self, root: &Path) -> PathBuf {
        root.join(self.to_string())
    }
}

impl fmt::Display for File {
    /// The path under `.trurlic/`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project => f.write_str(PROJECT_FILE),
            Self::Index => f.write_str(GRAPH_FILE),
            Self::Component(name) => write!(f, "{COMPONENTS_DIR}/{name}.toml"),
            Self::Decision(name) => write!(f, "{DECISIONS_DIR}/{name}.toml"),
            Self::Pattern(name) => write!(f, "{PATTERNS_DIR}/{name}.toml"),
        }
    }
}

/// The store's graph files as TOML tables, which the steps rewrite in
/// place. A file a step removes from the tree is removed from the store.
#[derive(Debug)]
pub(in crate::store) struct Tree {
    pub(in crate::store) files: BTreeMap<File, toml::Table>,
    /// What the running step dropped.
    pub(super) losses: Vec<Loss>,
    /// What else the running step reports.
    pub(super) facts: Vec<Fact>,
    /// The placement the step to format 0.5 writes, decided over the files
    /// on disk before any step ran.
    pub(super) placement: (Placement, PlacementBasis),
}

impl Tree {
    /// The table of `project.toml`, which every tree read from a store
    /// holds.
    pub(in crate::store) fn project(&mut self) -> &mut toml::Table {
        self.files.entry(File::Project).or_default()
    }

    /// Write `version` under the key `project.toml` holds the version in.
    pub(super) fn stamp(&mut self, version: FormatVersion) {
        let project = self.project();
        let key = version_key(project).unwrap_or("format_version");
        project.insert(key.to_owned(), version.to_string().into());
    }
}

/// `value`, at `field` of `file`, as a [`Loss`].
pub(super) fn loss(file: &File, field: &str, value: &toml::Value) -> Loss {
    Loss {
        file: file.to_string(),
        field: field.to_owned(),
        value: value.to_string(),
    }
}

/// The keys that have held the format version, newest first.
const VERSION_KEYS: [&str; 3] = ["format_version", "trurlic_version", "trurl_version"];

/// The newest of the keys that have held the format version that
/// `project`, a `project.toml` table, has.
pub(in crate::store) fn version_key(project: &toml::Table) -> Option<&'static str> {
    VERSION_KEYS
        .into_iter()
        .find(|key| project.contains_key(*key))
}

impl Store {
    /// Every graph file as a table, with the bytes each had: `project.toml`,
    /// `graph.toml` when there is one, then the node files of every kind.
    /// The placement is decided over these files: every chain of the
    /// registry ends in a format that states it, and none before 0.5 did.
    pub(in crate::store) fn read_tree(
        &self,
        placement: PlacementSource,
    ) -> Result<(Tree, BTreeMap<File, Vec<u8>>)> {
        let mut files = vec![File::Project];
        let index = self.root.join(GRAPH_FILE);
        if index.try_exists().map_err(Error::io(&index))? {
            files.push(File::Index);
        }
        let stems = |dir: &str| list_toml_stems(&self.root.join(dir));
        files.extend(stems(COMPONENTS_DIR)?.into_iter().map(File::Component));
        files.extend(stems(DECISIONS_DIR)?.into_iter().map(File::Decision));
        files.extend(stems(PATTERNS_DIR)?.into_iter().map(File::Pattern));
        let mut tree = Tree {
            files: BTreeMap::new(),
            losses: Vec::new(),
            facts: Vec::new(),
            placement: self.placement(placement, files.iter().map(ToString::to_string))?,
        };
        let mut original = BTreeMap::new();
        for file in files {
            let (table, bytes) = read_table(&file.path(&self.root))?;
            // clone: the file keys both the table and the original bytes.
            tree.files.insert(file.clone(), table);
            original.insert(file, bytes);
        }
        Ok((tree, original))
    }
}
