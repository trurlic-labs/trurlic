//! Migrating a store in an older format to the current one.
//!
//! Every graph file is read as a TOML table into a [`Tree`]. The chain of
//! steps of the registry (`steps.rs`) from the store's version rewrites the
//! tree one format at a time, each recording what it drops: a [`Loss`]
//! carries the dropped value's text. The tree is then read as the current
//! format, which refuses any key it has no place for, so a step must map or
//! drop every value it meets. It lands in one journaled commit that writes
//! each file whose canonical bytes differ and removes each file the tree no
//! longer holds.

mod steps;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::canonical::Canonical;
use super::commit::PendingWrite;
use super::schema::{
    COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, FORMAT_VERSION, PROJECT_FILE, Project,
};
use super::state::list_toml_stems;
use super::version::{FormatVersion, Standing, read_table, standing, version_key};
use super::{Store, StoreLock};

use steps::{STEPS, Step};

/// The index of formats before 0.5, read so a step can move its edges.
const GRAPH_FILE: &str = "graph.toml";

/// The pattern nodes of formats before 0.5.
const PATTERNS_DIR: &str = "patterns";

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
    /// The files the commit writes, by path under `.trurlic/`, in path order.
    pub(crate) rewritten: Vec<String>,
    /// The files the commit removes, by path under `.trurlic/`.
    pub(crate) removed: Vec<String>,
}

/// A planned migration: its report, and the commit that carries it out.
#[must_use = "a planned migration is carried out by Store::commit_migration"]
pub(crate) struct Migration {
    pub(crate) report: MigrationReport,
    writes: Vec<PendingWrite>,
    removes: Vec<PathBuf>,
}

/// A graph file of any format.
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

/// Whether a migration step starts or ends at `version`: a format some
/// trurlic wrote.
pub(super) fn names(version: FormatVersion) -> bool {
    STEPS
        .iter()
        .any(|step| step.from == version || step.to == version)
}

/// The steps that lead from `from` to [`FORMAT_VERSION`], one after the
/// other, or `None` when `steps` hold no such chain.
fn chain(steps: &[Step], from: FormatVersion) -> Option<&[Step]> {
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
    /// Plan the migration under the exclusive lock, for
    /// [`commit_migration`](Self::commit_migration). A store this binary
    /// cannot migrate is refused before anything in it is touched; then what
    /// an interrupted write left is finished, which completes a migration
    /// that stopped after its journal. `None` when the store is current.
    pub(crate) fn plan_migration(&self, lock: &StoreLock) -> Result<Option<Migration>> {
        self.migration_chain(&STEPS)?;
        self.recover(lock)?;
        self.plan(&STEPS)
    }

    /// The migration [`plan_migration`](Self::plan_migration) would make,
    /// planned under the shared lock, which no commit holds. A journal an
    /// interrupted write left stays for the migration to finish.
    pub(crate) fn preview_migration(&self) -> Result<Option<Migration>> {
        let _shared = self.lock_shared()?;
        self.plan(&STEPS)
    }

    /// Carry out `migration` in one journaled commit.
    pub(crate) fn commit_migration(
        &self,
        lock: &StoreLock,
        migration: Migration,
    ) -> Result<MigrationReport> {
        let Migration {
            report,
            writes,
            removes,
        } = migration;
        self.commit_batch(lock, &writes, &removes)?;
        Ok(report)
    }

    /// The version `project.toml` states and the steps of `registry` from
    /// it, or `None` when the store is current. A newer, unknown or
    /// unparseable version, and one no chain leads from, are refused before
    /// the other files are read.
    fn migration_chain<'r>(
        &self,
        registry: &'r [Step],
    ) -> Result<Option<(FormatVersion, &'r [Step])>> {
        let path = self.project_path();
        let Standing::Older(from) = standing(&path, &read_table(&path)?.0)? else {
            return Ok(None);
        };
        let steps = chain(registry, from).ok_or(Error::NoMigration {
            path,
            found: from,
            expected: FORMAT_VERSION,
        })?;
        Ok(Some((from, steps)))
    }

    /// The migration of the files on disk to the current format along
    /// `registry`, writing nothing. `None` when the store is current.
    fn plan(&self, registry: &[Step]) -> Result<Option<Migration>> {
        let Some((from, steps)) = self.migration_chain(registry)? else {
            return Ok(None);
        };
        let (mut tree, original) = self.read_tree()?;
        let reports = walk(&mut tree, steps)?;
        let mut current = Current::new(self);
        for (file, table) in tree.files {
            let before = original.get(&file).map(Vec::as_slice);
            current.add(&file, table, before)?;
        }
        for file in original.keys().filter(|file| !current.kept.contains(file)) {
            current.removed.push(file.to_string());
            current.removes.push(file.path(self.root()));
        }
        Ok(Some(Migration {
            report: MigrationReport {
                from,
                steps: reports,
                rewritten: current.rewritten,
                removed: current.removed,
            },
            writes: current.writes,
            removes: current.removes,
        }))
    }

    /// Every graph file as a table, with the bytes each had: `project.toml`,
    /// `graph.toml` when there is one, then the node files of every kind.
    fn read_tree(&self) -> Result<(Tree, BTreeMap<File, Vec<u8>>)> {
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
fn walk(tree: &mut Tree, steps: &[Step]) -> Result<Vec<StepReport>> {
    let mut reports = Vec::with_capacity(steps.len());
    for step in steps {
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
    /// Every file the current format holds.
    kept: BTreeSet<File>,
    writes: Vec<PendingWrite>,
    rewritten: Vec<String>,
    removes: Vec<PathBuf>,
    removed: Vec<String>,
}

impl<'a> Current<'a> {
    fn new(store: &'a Store) -> Self {
        Self {
            store,
            kept: BTreeSet::new(),
            writes: Vec::new(),
            rewritten: Vec::new(),
            removes: Vec::new(),
            removed: Vec::new(),
        }
    }

    /// Read `table` as `file` in the current format; `before` is the bytes
    /// the file had on disk. A file the current format has no place for is
    /// refused: the step that leads to it must have removed it.
    fn add(&mut self, file: &File, table: toml::Table, before: Option<&[u8]>) -> Result<()> {
        match file {
            File::Project => self.node::<Project>(file, table, before),
            File::Component(_) => self.node::<Component>(file, table, before),
            File::Decision(_) => self.node::<Decision>(file, table, before),
            File::Index | File::Pattern(_) => Err(Error::Unmigrated {
                path: file.path(self.store.root()),
            }),
        }
    }

    /// `table` read as the node file `file`, written when its canonical
    /// bytes differ from `before`.
    fn node<T: Canonical + DeserializeOwned + PartialEq>(
        &mut self,
        file: &File,
        table: toml::Table,
        before: Option<&[u8]>,
    ) -> Result<()> {
        let path = file.path(self.store.root());
        let parsed: T = table.try_into().map_err(Error::toml(&path))?;
        let write = self.store.prepare_write(&path, &parsed)?;
        if before != Some(write.content.as_bytes()) {
            self.rewritten.push(file.to_string());
            self.writes.push(write);
        }
        // clone: the commit owns the files it keeps; the tree keeps its own.
        self.kept.insert(file.clone());
        Ok(())
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
        let (tree, _) = store.read_tree().unwrap();
        (tmp, tree)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::testing::fixture_store;
    use super::*;
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
    #[ignore = "needs the 0.4.1 -> 0.5.0 step, which lands with the migration"]
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
            assert_eq!(state.validate(), [], "{version}");
            assert_eq!(state.non_canonical, [], "{version}");
            assert!(store.preview_migration().unwrap().is_none(), "{version}");
        }
    }

    #[test]
    #[ignore = "needs the 0.4.1 -> 0.5.0 step, which lands with the migration"]
    fn a_plan_writes_nothing() {
        let (_tmp, store) = fixture_store("0.1.0");
        let before = snapshot(store.root());

        let plan = store.preview_migration().unwrap().unwrap();

        assert!(!plan.report.rewritten.is_empty());
        assert_eq!(snapshot(store.root()), before);
    }

    /// The step to 0.5 maps every field of 0.4.1 and reports the rest, now
    /// that the typed read refuses what it has no place for.
    #[test]
    #[ignore = "needs the 0.4.1 -> 0.5.0 step, which lands with the migration"]
    fn a_field_the_current_format_does_not_hold_is_reported() {
        let (_tmp, store) = fixture_store("0.4.0");
        let path = store.decision_path("one-toml-file-per-node");
        let text = fs::read_to_string(&path).unwrap();
        let text = text.replacen("[decision]\n", "[decision]\nweight = 3\n", 1);
        let text = text.replacen("symbol = ", "line = 12\nsymbol = ", 1);
        fs::write(&path, text).unwrap();

        let report = migrate(&store);

        let losses: Vec<(&str, &str, &str)> = report
            .steps
            .iter()
            .flat_map(|step| &step.losses)
            .map(|loss| (loss.file.as_str(), loss.field.as_str(), loss.value.as_str()))
            .collect();
        let file = "decisions/one-toml-file-per-node.toml";
        assert!(
            losses.contains(&(file, "decision.weight", "3")),
            "{losses:?}"
        );
        assert!(
            losses.contains(&(file, "decision.code_refs[0].line", "12")),
            "{losses:?}"
        );
    }

    #[test]
    #[ignore = "needs the 0.4.1 -> 0.5.0 step, which lands with the migration"]
    fn an_edge_to_no_node_is_reported_and_dropped() {
        let (_tmp, store) = fixture_store("0.4.0");
        let path = store.root().join(GRAPH_FILE);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("\n[[edges]]\nfrom = \"api\"\nto = \"gone\"\nkind = \"connects_to\"\n");
        fs::write(&path, text).unwrap();

        let report = migrate(&store);

        let dropped = report
            .steps
            .iter()
            .flat_map(|step| &step.losses)
            .any(|loss| loss.file == GRAPH_FILE && loss.value.contains("\"gone\""));
        assert!(dropped, "{:?}", report.steps);
        let state = store.load_state().unwrap();
        assert!(!state.components["api"].connects_to.contains("gone"));
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

    #[test]
    fn a_format_no_step_names_is_refused_by_migrate() {
        let (_tmp, _, err) = refused_untouched("0.3.7");

        assert!(
            matches!(&err, Error::NoMigration { found, .. }
                if *found == FormatVersion::new(0, 3, 7)),
            "{err:?}"
        );
    }

    fn step(from: (u32, u32, u32), to: (u32, u32, u32)) -> Step {
        Step {
            from: FormatVersion::new(from.0, from.1, from.2),
            to: FormatVersion::new(to.0, to.1, to.2),
            change: "a test step",
            apply: |_| Ok(()),
        }
    }

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

    /// A 0.4.1 store of one decision, with the index and a pattern 0.5 has
    /// no place for.
    fn store_at_0_4_1() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.4.1");
        fs::write(store.root().join(GRAPH_FILE), "version = 1\n").unwrap();
        fs::create_dir_all(store.root().join(PATTERNS_DIR)).unwrap();
        fs::write(
            store.root().join(PATTERNS_DIR).join("p.toml"),
            "[pattern]\n",
        )
        .unwrap();
        fs::write(
            store.decision_path("d"),
            "component = \"project\"\nchoice = \"Sign tokens\"\n\
             reason = \"Verification needs no session store\"\nstatus = \"accepted\"\n\
             created = \"2025-06-01T10:00:00Z\"\n",
        )
        .unwrap();
        (tmp, store)
    }

    /// What the step to 0.5 leaves of the project table, the decision
    /// already in shape.
    fn reshape_project(tree: &mut Tree) {
        let project = tree.project();
        project.remove("project");
        project.insert("name".into(), "test-project".into());
        project.insert("description".into(), "".into());
        project.insert("placement".into(), "tracked".into());
    }

    #[test]
    fn the_commit_writes_changed_files_and_removes_the_ones_the_steps_drop() {
        let (_tmp, store) = store_at_0_4_1();
        let registry = [Step {
            apply: |tree| {
                reshape_project(tree);
                tree.files
                    .retain(|file, _| !matches!(file, File::Index | File::Pattern(_)));
                Ok(())
            },
            ..step((0, 4, 1), (0, 5, 0))
        }];

        let migration = store.plan(&registry).unwrap().unwrap();

        assert_eq!(migration.report.rewritten, [PROJECT_FILE]);
        assert_eq!(migration.report.removed, [GRAPH_FILE, "patterns/p.toml"]);
        store
            .commit_migration(&store.lock().unwrap(), migration)
            .unwrap();
        let state = store.load_state().unwrap();
        assert_eq!(state.non_canonical, []);
        assert_eq!(state.decisions["d"].choice, "Sign tokens");
        assert!(!store.root().join(GRAPH_FILE).exists());
    }

    #[test]
    fn a_file_the_steps_leave_without_a_place_in_0_5_is_refused() {
        let (_tmp, store) = store_at_0_4_1();
        let registry = [Step {
            apply: |tree| {
                reshape_project(tree);
                Ok(())
            },
            ..step((0, 4, 1), (0, 5, 0))
        }];

        let err = store.plan(&registry).map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Unmigrated { path } if *path == store.root().join(GRAPH_FILE)),
            "{err:?}"
        );
    }
}
