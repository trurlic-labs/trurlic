//! The way from an older format to the current one, as data: the registry
//! of steps and the tree of tables they rewrite. Carrying a migration out
//! on a store is `store::migrate`'s.
//!
//! Every graph file is read as a TOML table into a [`Tree`]. The chain of
//! steps of the registry (`steps.rs`) from the store's version rewrites the
//! tree one format at a time, each recording what it drops, a [`Loss`]
//! carrying the dropped value's text, and what else the owner must be able
//! to read after the fact, a [`Fact`]. The tree is then read as the current
//! format, which refuses any key it has no place for, so a step must map or
//! drop every value it meets.

mod format_0_5;
mod placement;
mod report;
mod steps;

use std::collections::BTreeMap;
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};

use crate::store::Store;
use crate::store::model::{
    COMPONENTS_DIR, DECISIONS_DIR, FORMAT_VERSION, FormatVersion, PROJECT_FILE, Placement,
};
use crate::{Error, Result};

use super::check::{read_table, version_key};
use super::load::list_toml_stems;

pub use placement::PlacementDoubt;
pub(crate) use placement::PlacementSource;
pub(crate) use report::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, RefText, StepReport,
};
pub(in crate::store) use steps::{STEPS, Step};

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
    losses: Vec<Loss>,
    /// What else the running step reports.
    facts: Vec<Fact>,
    /// The placement the step to format 0.5 writes, decided over the files
    /// on disk before any step ran.
    placement: (Placement, PlacementBasis),
}

impl Tree {
    /// The table of `project.toml`, which every tree read from a store
    /// holds.
    pub(in crate::store) fn project(&mut self) -> &mut toml::Table {
        self.files.entry(File::Project).or_default()
    }

    /// Write `version` under the key `project.toml` holds the version in.
    fn stamp(&mut self, version: FormatVersion) {
        let project = self.project();
        let key = version_key(project).unwrap_or("format_version");
        project.insert(key.to_owned(), version.to_string().into());
    }
}

/// `value`, at `field` of `file`, as a [`Loss`].
fn loss(file: &File, field: &str, value: &toml::Value) -> Loss {
    Loss {
        file: file.to_string(),
        field: field.to_owned(),
        value: value.to_string(),
    }
}

/// Whether a migration step starts or ends at `version`: a format some
/// trurlic wrote.
pub(in crate::store) fn names(version: FormatVersion) -> bool {
    STEPS
        .iter()
        .any(|step| step.from == version || step.to == version)
}

/// The steps that lead from `from` to [`FORMAT_VERSION`], one after the
/// other, or `None` when `steps` hold no such chain.
pub(in crate::store) fn chain(steps: &[Step], from: FormatVersion) -> Option<&[Step]> {
    let start = steps.iter().position(|step| step.from == from)?;
    let rest = steps.get(start..)?;
    let end = rest.iter().position(|step| step.to == FORMAT_VERSION)?;
    let chain = rest.get(..=end)?;
    let linked = chain
        .iter()
        .zip(chain.iter().skip(1))
        .all(|(step, next)| step.to == next.from);
    linked.then_some(chain)
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
            placement: self.placement(placement, &files)?,
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

/// Walk `steps` over `tree`, stamping each step's version after it.
pub(in crate::store) fn walk(tree: &mut Tree, steps: &[Step]) -> Result<Vec<StepReport>> {
    let mut reports = Vec::with_capacity(steps.len());
    for step in steps {
        (step.apply)(tree)?;
        tree.stamp(step.to);
        // Sorted, so the report shows each kind of fact as one section; a
        // fact two equal edges or refs gave is one.
        let mut facts = mem::take(&mut tree.facts);
        facts.sort_unstable();
        facts.dedup();
        reports.push(StepReport {
            from: step.from,
            to: step.to,
            change: step.change,
            losses: mem::take(&mut tree.losses),
            facts,
        });
    }
    Ok(reports)
}

#[cfg(test)]
pub(in crate::store) mod testing {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::store::STORE_DIR;

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// A store holding the committed fixture graph of format `version`.
    pub(in crate::store) fn fixture_store(version: &str) -> (TempDir, Store) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/formats")
            .join(version);
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(STORE_DIR);
        copy_dir(&fixture, &root);
        (tmp, Store::at(root))
    }

    /// The placement the tests state, so no test asks git.
    pub(in crate::store) fn tracked() -> PlacementSource {
        PlacementSource::Stated(Placement::Tracked)
    }

    /// The fixture graph of format `version` as a tree.
    pub(super) fn fixture_tree(version: &str) -> (TempDir, Tree) {
        let (tmp, store) = fixture_store(version);
        let (tree, _) = store.read_tree(tracked()).unwrap();
        (tmp, tree)
    }

    /// A step from `from` to `to` that changes nothing.
    pub(in crate::store) fn step(from: (u32, u32, u32), to: (u32, u32, u32)) -> Step {
        Step {
            from: FormatVersion::new(from.0, from.1, from.2),
            to: FormatVersion::new(to.0, to.1, to.2),
            change: "a test step",
            apply: |_| Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::step;
    use super::*;

    #[test]
    fn a_chain_runs_from_a_version_to_the_current_one_unbroken() {
        let current = (0, 5, 0);
        let registry = [
            step((0, 1, 0), (0, 2, 0)),
            step((0, 2, 0), (0, 4, 0)),
            step((0, 4, 0), current),
        ];
        let from = |at: (u32, u32, u32)| {
            chain(&registry, FormatVersion::new(at.0, at.1, at.2)).map(<[Step]>::len)
        };
        assert_eq!(from((0, 1, 0)), Some(3));
        assert_eq!(from((0, 4, 0)), Some(1));
        assert_eq!(from((0, 3, 0)), None, "no step starts there");

        let broken = [step((0, 1, 0), (0, 2, 0)), step((0, 3, 0), current)];
        assert!(chain(&broken, FormatVersion::new(0, 1, 0)).is_none());
        let short = [step((0, 1, 0), (0, 2, 0))];
        assert!(chain(&short, FormatVersion::new(0, 1, 0)).is_none());
    }
}
