//! Migrating a store in an older format to the current one.
//!
//! Every graph file is read as a TOML table into a [`Tree`]. The steps of
//! the registry (`steps.rs`) from the store's version on rewrite the tree
//! one format at a time, each recording what it drops. The tree is then
//! read as the current format, which records every field it has no place
//! for, and lands in one journaled commit with `graph.toml` last. Nothing
//! is dropped unreported: a [`Loss`] carries the dropped value's text.

mod steps;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::commit::PendingWrite;
use super::load::{NodeFiles, reconcile};
use super::schema::{
    COMPONENTS_DIR, DECISIONS_DIR, EdgeKind, GRAPH_FILE, GraphIndex, PATTERNS_DIR, ProjectFile,
};
use super::state::list_toml_stems;
use super::version::{FormatVersion, Standing, read_table, standing, version_key};
use super::{Store, StoreLock};

/// A value a migration dropped: `field` is its dotted key path in `file`,
/// a path under `.trurlic/`, and `value` its TOML text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Loss {
    pub(crate) file: String,
    pub(crate) field: String,
    pub(crate) value: String,
}

/// What one step dropped.
#[derive(Debug)]
pub(crate) struct StepReport {
    pub(crate) from: FormatVersion,
    pub(crate) to: FormatVersion,
    /// The format change, in a few words.
    pub(crate) change: &'static str,
    pub(crate) losses: Vec<Loss>,
}

/// What a migration does to the store, in the order it happens.
#[derive(Debug)]
pub(crate) struct MigrationReport {
    pub(crate) from: FormatVersion,
    pub(crate) steps: Vec<StepReport>,
    /// What the current format has no place for: fields it does not know,
    /// and edges whose endpoint is no node.
    pub(crate) unheld: Vec<Loss>,
    /// The files the commit writes, by path under `.trurlic/`, in the
    /// order it writes them.
    pub(crate) rewritten: Vec<String>,
}

/// A planned migration: its report, and the commit that carries it out.
#[must_use = "a planned migration is carried out by Store::commit_migration"]
pub(crate) struct Migration {
    pub(crate) report: MigrationReport,
    writes: Vec<PendingWrite>,
    index: GraphIndex,
}

/// A graph file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum File {
    Project,
    Index,
    Component(String),
    Decision(String),
    Pattern(String),
}

impl File {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(self.to_string())
    }
}

impl fmt::Display for File {
    /// The path under `.trurlic/`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project => f.write_str("project.toml"),
            Self::Index => f.write_str(GRAPH_FILE),
            Self::Component(name) => write!(f, "{COMPONENTS_DIR}/{name}.toml"),
            Self::Decision(name) => write!(f, "{DECISIONS_DIR}/{name}.toml"),
            Self::Pattern(name) => write!(f, "{PATTERNS_DIR}/{name}.toml"),
        }
    }
}

/// The store's graph files as TOML tables, which the steps rewrite in
/// place. The commit writes every file the tree holds and deletes none.
#[derive(Debug)]
pub(super) struct Tree {
    pub(super) files: BTreeMap<File, toml::Table>,
    /// What the running step dropped.
    pub(super) losses: Vec<Loss>,
}

impl Tree {
    /// The table of `project.toml`, which every tree read from a store
    /// holds.
    pub(super) fn project(&mut self) -> &mut toml::Table {
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
pub(super) fn loss(file: &File, field: &str, value: &toml::Value) -> Loss {
    Loss {
        file: file.to_string(),
        field: field.to_owned(),
        value: value.to_string(),
    }
}

/// Whether a migration step starts at `version`.
pub(super) fn starts_at(version: FormatVersion) -> bool {
    steps::STEPS.iter().any(|step| step.from == version)
}

impl Store {
    /// Plan the migration under the exclusive lock, for
    /// [`commit_migration`](Self::commit_migration). A store this binary
    /// cannot migrate is refused before anything in it is touched; then what
    /// an interrupted write left is finished, which completes a migration
    /// that stopped after its journal. `None` when the store is current.
    pub(crate) fn plan_migration(&self, lock: &StoreLock) -> Result<Option<Migration>> {
        let path = self.project_path();
        standing(&path, &read_table(&path)?.0)?;
        self.recover(lock)?;
        self.plan()
    }

    /// The migration [`plan_migration`](Self::plan_migration) would make,
    /// planned under the shared lock, which no commit holds. A journal an
    /// interrupted write left stays for the migration to finish.
    pub(crate) fn preview_migration(&self) -> Result<Option<Migration>> {
        let _shared = self.lock_shared()?;
        self.plan()
    }

    /// The migration of the files on disk to the current format, writing
    /// nothing. `None` when the store is in the current format; a newer,
    /// unknown or unparseable version is refused before the other files
    /// are read.
    fn plan(&self) -> Result<Option<Migration>> {
        let path = self.project_path();
        let (project, project_bytes) = read_table(&path)?;
        let Standing::Older(from) = standing(&path, &project)? else {
            return Ok(None);
        };
        let (mut tree, original) = self.read_tree(project, project_bytes)?;
        let steps = walk(&mut tree, from)?;
        let mut current = Current::new(self);
        for (file, table) in tree.files {
            let before = original.get(&file).map(Vec::as_slice);
            current.add(&file, &table, before)?;
        }
        current.into_migration(from, steps).map(Some)
    }

    /// Carry out `migration` in one journaled commit, `graph.toml` last.
    pub(crate) fn commit_migration(
        &self,
        lock: &StoreLock,
        migration: Migration,
    ) -> Result<MigrationReport> {
        let Migration {
            report,
            writes,
            mut index,
        } = migration;
        self.commit_batch(lock, writes, &[], Some(&mut index))?;
        Ok(report)
    }

    /// Every graph file as a table, with the bytes each had: `project`, the
    /// table of `project.toml` read from `project_bytes`, then `graph.toml`
    /// when there is one, then the node files.
    fn read_tree(
        &self,
        project: toml::Table,
        project_bytes: Vec<u8>,
    ) -> Result<(Tree, BTreeMap<File, Vec<u8>>)> {
        let mut files = Vec::new();
        let index = self.graph_path();
        if index.try_exists().map_err(Error::io(&index))? {
            files.push(File::Index);
        }
        let stems = |dir: &str| list_toml_stems(&self.root.join(dir));
        files.extend(stems(COMPONENTS_DIR)?.into_iter().map(File::Component));
        files.extend(stems(DECISIONS_DIR)?.into_iter().map(File::Decision));
        files.extend(stems(PATTERNS_DIR)?.into_iter().map(File::Pattern));
        let mut tree = Tree {
            files: BTreeMap::from([(File::Project, project)]),
            losses: Vec::new(),
        };
        let mut original = BTreeMap::from([(File::Project, project_bytes)]);
        for file in files {
            let (table, bytes) = read_table(&file.path(&self.root))?;
            // clone: the file keys both the table and the original bytes.
            tree.files.insert(file.clone(), table);
            original.insert(file, bytes);
        }
        Ok((tree, original))
    }
}

/// Walk the steps from `from` to the current format over `tree`.
fn walk(tree: &mut Tree, from: FormatVersion) -> Result<Vec<StepReport>> {
    let mut reports = Vec::new();
    for step in steps::STEPS.iter().skip_while(|step| step.from != from) {
        (step.apply)(tree)?;
        tree.stamp(step.to);
        reports.push(StepReport {
            from: step.from,
            to: step.to,
            change: step.change,
            losses: mem::take(&mut tree.losses),
        });
    }
    Ok(reports)
}

/// The migrated files read as the current format, gathered for the commit.
struct Current<'a> {
    store: &'a Store,
    /// `graph.toml` as the steps left it, for the edges node files lack.
    existing: GraphIndex,
    /// The hash of the bytes `project.toml` is written with.
    project_hash: String,
    /// Each node file in name order, with the hash of its bytes.
    nodes: NodeFiles,
    /// Every node name, `project` included.
    names: BTreeSet<String>,
    writes: Vec<PendingWrite>,
    rewritten: Vec<String>,
    unheld: Vec<Loss>,
}

impl<'a> Current<'a> {
    fn new(store: &'a Store) -> Self {
        Self {
            store,
            existing: GraphIndex {
                version: 1,
                rebuilt: None,
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            project_hash: String::new(),
            nodes: NodeFiles {
                components: Vec::new(),
                decisions: Vec::new(),
                patterns: Vec::new(),
            },
            names: BTreeSet::from(["project".to_owned()]),
            writes: Vec::new(),
            rewritten: Vec::new(),
            unheld: Vec::new(),
        }
    }

    /// Read `table` as `file` in the current format; `before` is the bytes
    /// the file had on disk.
    fn add(&mut self, file: &File, table: &toml::Table, before: Option<&[u8]>) -> Result<()> {
        match file {
            File::Project => {
                let (_, hash) = self.node::<ProjectFile>(file, table, before)?;
                self.project_hash = hash;
            }
            File::Index => self.existing = self.parse(file, table)?,
            File::Component(name) => {
                let (component, hash) = self.node(file, table, before)?;
                // clone: the node file and the name set each own the name.
                self.nodes.components.push((name.clone(), component, hash));
            }
            File::Decision(name) => {
                let (decision, hash) = self.node(file, table, before)?;
                // clone: the node file and the name set each own the name.
                self.nodes.decisions.push((name.clone(), decision, hash));
            }
            File::Pattern(name) => {
                let (pattern, hash) = self.node(file, table, before)?;
                // clone: the node file and the name set each own the name.
                self.nodes.patterns.push((name.clone(), pattern, hash));
            }
        }
        if let File::Component(name) | File::Decision(name) | File::Pattern(name) = file {
            // clone: the set owns its names; the file keeps its own.
            self.names.insert(name.clone());
        }
        Ok(())
    }

    /// `table` as the node file `file` and the hash of the bytes it is
    /// serialized to, which the commit writes when they differ from
    /// `before`.
    fn node<T: Serialize + DeserializeOwned>(
        &mut self,
        file: &File,
        table: &toml::Table,
        before: Option<&[u8]>,
    ) -> Result<(T, String)> {
        let parsed: T = self.parse(file, table)?;
        let write = self
            .store
            .prepare_write(&file.path(self.store.root()), &parsed)?;
        let hash = write.content_hash();
        if before != Some(write.content.as_bytes()) {
            self.rewritten.push(file.to_string());
            self.writes.push(write);
        }
        Ok((parsed, hash))
    }

    /// `table` as the current format's `T`, with every field `T` does not
    /// hold recorded as unheld.
    fn parse<T: Serialize + DeserializeOwned>(
        &mut self,
        file: &File,
        table: &toml::Table,
    ) -> Result<T> {
        let path = file.path(self.store.root());
        // clone: the table is compared with what `T` keeps of it.
        let parsed: T = table.clone().try_into().map_err(Error::toml(&path))?;
        let kept = toml::Table::try_from(&parsed)
            .map_err(|source| Error::TomlSerialize { path, source })?;
        record_unheld(file, "", table, &kept, &mut self.unheld);
        Ok(parsed)
    }

    /// The report and the commit: the index is rebuilt from the node files
    /// as a load rebuilds it, and an edge whose endpoint is no node is
    /// recorded as unheld, since the rebuild drops it.
    fn into_migration(mut self, from: FormatVersion, steps: Vec<StepReport>) -> Result<Migration> {
        let index_file = File::Index;
        for edge in &self.existing.edges {
            let dangles = !self.names.contains(&edge.from) || !self.names.contains(&edge.to);
            if edge.kind != EdgeKind::BelongsTo && dangles {
                let value = toml::Value::try_from(edge).map_err(|source| Error::TomlSerialize {
                    path: index_file.path(self.store.root()),
                    source,
                })?;
                self.unheld.push(loss(&index_file, "edges", &value));
            }
        }
        let (index, ..) = reconcile(self.existing, self.project_hash, self.nodes);
        self.rewritten.push(index_file.to_string());
        Ok(Migration {
            report: MigrationReport {
                from,
                steps,
                unheld: self.unheld,
                rewritten: self.rewritten,
            },
            writes: self.writes,
            index,
        })
    }
}

/// Record every key of `before` that `kept` lacks, descending into tables
/// and arrays of tables. `prefix` is the dotted path of `before` in `file`.
fn record_unheld(
    file: &File,
    prefix: &str,
    before: &toml::Table,
    kept: &toml::Table,
    unheld: &mut Vec<Loss>,
) {
    for (key, value) in before {
        let field = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match (value, kept.get(key)) {
            // An empty list or table holds nothing to lose, and the current
            // format omits the lists it holds when they are empty.
            (toml::Value::Array(items), None) if items.is_empty() => {}
            (toml::Value::Table(entries), None) if entries.is_empty() => {}
            (_, None) => unheld.push(loss(file, &field, value)),
            (toml::Value::Table(before), Some(toml::Value::Table(kept))) => {
                record_unheld(file, &field, before, kept, unheld);
            }
            (toml::Value::Array(before), Some(toml::Value::Array(kept))) => {
                for (at, pair) in before.iter().zip(kept).enumerate() {
                    if let (toml::Value::Table(before), toml::Value::Table(kept)) = pair {
                        record_unheld(file, &format!("{field}[{at}]"), before, kept, unheld);
                    }
                }
            }
            // Every other pair is a value the current format holds.
            (_, Some(_)) => {}
        }
    }
}

#[cfg(test)]
pub(super) mod testing {
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

    /// The fixture graph of format `version` as a tree.
    pub(in crate::store) fn fixture_tree(version: &str) -> (TempDir, Tree) {
        let (tmp, store) = fixture_store(version);
        let (project, bytes) = read_table(&store.project_path()).unwrap();
        let (tree, _) = store.read_tree(project, bytes).unwrap();
        (tmp, tree)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::testing::fixture_store;
    use super::*;
    use crate::store::graph::Severity;
    use crate::store::schema::FORMAT_VERSION;
    use crate::store::testing::{setup_store, setup_store_with_version};
    use tempfile::TempDir;

    const HISTORICAL: [&str; 4] = ["0.1.0", "0.2.0", "0.3.0", "0.4.0"];

    fn migrate(store: &Store) -> MigrationReport {
        let lock = store.lock().unwrap();
        let migration = store.plan_migration(&lock).unwrap().unwrap();
        store.commit_migration(&lock, migration).unwrap()
    }

    /// Every file under `root` but `.state/`, with its bytes.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != ".state") {
                    files.extend(snapshot(&path));
                }
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
        files
    }

    #[test]
    fn every_historical_fixture_migrates_to_a_store_that_loads_clean() {
        for version in HISTORICAL {
            let (_tmp, store) = fixture_store(version);
            assert!(store.check_version().is_err(), "{version}");

            let report = migrate(&store);

            assert_eq!(report.from.to_string(), version);
            assert_eq!(report.steps.last().unwrap().to, FORMAT_VERSION);
            store.check_version().unwrap();
            let state = store.load_state().unwrap();
            assert_eq!(state.project.format_version, FORMAT_VERSION);
            let errors: Vec<_> = state
                .validate()
                .into_iter()
                .filter(|issue| issue.severity() == Severity::Error)
                .collect();
            assert!(errors.is_empty(), "{version}: {errors:?}");
            let hashes = store.verify_hashes().unwrap();
            assert!(hashes.is_empty(), "{version}: {hashes:?}");
            assert!(store.preview_migration().unwrap().is_none(), "{version}");
        }
    }

    #[test]
    fn a_migration_rewrites_only_the_files_whose_bytes_change() {
        let (_tmp, store) = fixture_store("0.4.0");

        let report = migrate(&store);

        assert_eq!(report.rewritten, ["project.toml", "graph.toml"]);
    }

    #[test]
    fn a_plan_writes_nothing() {
        let (_tmp, store) = fixture_store("0.1.0");
        let before = snapshot(store.root());

        let plan = store.preview_migration().unwrap().unwrap();

        assert!(!plan.report.rewritten.is_empty());
        assert_eq!(snapshot(store.root()), before);
    }

    #[test]
    fn a_field_the_current_format_does_not_hold_is_reported() {
        let (_tmp, store) = fixture_store("0.4.0");
        let path = store.decision_path("one-toml-file-per-node");
        let text = fs::read_to_string(&path).unwrap();
        let text = text.replacen("[decision]\n", "[decision]\nweight = 3\nhistory = []\n", 1);
        let text = text.replacen("symbol = ", "line = 12\nsymbol = ", 1);
        fs::write(&path, text).unwrap();

        let report = migrate(&store);

        let file = "decisions/one-toml-file-per-node.toml";
        let unheld = |field: &str, value: &str| Loss {
            file: file.into(),
            field: field.into(),
            value: value.into(),
        };
        assert_eq!(
            report.unheld,
            [
                unheld("decision.code_refs[0].line", "12"),
                unheld("decision.weight", "3"),
            ]
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("weight"));
    }

    #[test]
    fn an_edge_to_no_node_is_reported_and_dropped() {
        let (_tmp, store) = fixture_store("0.4.0");
        let path = store.graph_path();
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("\n[[edges]]\nfrom = \"api\"\nto = \"gone\"\nkind = \"connects_to\"\n");
        fs::write(&path, text).unwrap();

        let report = migrate(&store);

        assert_eq!(report.unheld.len(), 1, "{:?}", report.unheld);
        assert_eq!(report.unheld[0].field, "edges");
        assert!(report.unheld[0].value.contains("\"gone\""));
        let state = store.load_state().unwrap();
        assert!(!state.graph_index.edges.iter().any(|edge| edge.to == "gone"));
    }

    #[test]
    fn a_current_store_has_nothing_to_migrate() {
        let tmp = TempDir::new().unwrap();
        assert!(
            setup_store(tmp.path())
                .preview_migration()
                .unwrap()
                .is_none()
        );
    }

    /// `plan_migration` under the lock on a store stating `version`, with a
    /// temp file a stopped writer left; the file must outlive the refusal.
    fn refused_untouched(version: &str) -> (TempDir, Store, Error) {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), version);
        let leftover = store.tmp_dir().join("4242-0");
        fs::create_dir_all(store.tmp_dir()).unwrap();
        fs::write(&leftover, "staged").unwrap();

        let lock = store.lock().unwrap();
        let err = store.plan_migration(&lock).map(|_| ()).unwrap_err();

        assert!(leftover.is_file(), "a refused store is not touched");
        drop(lock);
        assert_eq!(
            store
                .preview_migration()
                .map(|_| ())
                .unwrap_err()
                .to_string(),
            err.to_string()
        );
        (tmp, store, err)
    }

    #[test]
    fn an_unparseable_version_is_refused_by_migrate() {
        let (_tmp, store, err) = refused_untouched("garbage");

        assert!(
            matches!(&err, Error::BadFormatVersion { path, text }
                if *path == store.project_path() && text == "garbage"),
            "{err:?}"
        );
    }

    #[test]
    fn a_newer_format_is_refused_by_migrate() {
        let (_tmp, _, err) = refused_untouched("0.9.0");

        assert!(
            matches!(&err, Error::VersionMismatch { found, .. }
                if *found == FormatVersion::new(0, 9, 0)),
            "{err:?}"
        );
    }
}
