//! Carrying a migration out: planned under the exclusive lock, or
//! previewed under the shared lock, from the registry of
//! `store::format::migration`, and committed in one journaled commit
//! that writes each file whose canonical bytes differ and removes each
//! file the migrated tree no longer holds.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::console::diag;
use crate::store::format::migration::{
    File, MigrationReport, PATTERNS_DIR, PlacementSource, STEPS, Step, chain, walk,
};
use crate::store::format::{Canonical, Standing, read_table, standing};
use crate::store::model::{Component, Decision, FORMAT_VERSION, FormatVersion, Placement, Project};
use crate::{Error, Result};

use super::commit::PendingWrite;
use super::history::History;
use super::lock::StoreLock;
use super::message::{CommitMessage, Tool};
use crate::store::format::Store;

/// A planned migration: its report, and the commit that carries it out.
#[must_use = "a planned migration is carried out by Store::commit_migration"]
pub(crate) struct Migration {
    pub(crate) report: MigrationReport,
    writes: Vec<PendingWrite>,
    removes: Vec<PathBuf>,
    /// The placement the migrated `project.toml` states: the commit's
    /// history goes where it says.
    placement: Placement,
}

impl Store {
    /// Plan the migration under the exclusive lock, for
    /// [`commit_migration`](Self::commit_migration). A store this binary
    /// cannot migrate is refused before anything in it is touched; then what
    /// an interrupted write left is finished, which completes a migration
    /// that stopped after its journal. `None` when the store is current.
    pub(crate) fn plan_migration(
        &self,
        lock: &StoreLock,
        placement: PlacementSource,
    ) -> Result<Option<Migration>> {
        self.migration_chain(&STEPS)?;
        self.recover(lock)?;
        self.plan(&STEPS, placement)
    }

    /// The migration [`plan_migration`](Self::plan_migration) would make,
    /// planned under the shared lock, which no commit holds. A journal an
    /// interrupted write left stays for the migration to finish.
    pub(crate) fn preview_migration(
        &self,
        placement: PlacementSource,
    ) -> Result<Option<Migration>> {
        let _shared = self.lock_shared()?;
        self.plan(&STEPS, placement)
    }

    /// Carry out `migration` in one journaled commit, then remove the
    /// `patterns/` directory it emptied. A migration to `shared` makes the
    /// graph's repository first, when there is none, and its commit is then
    /// the repository's first, holding every graph file.
    pub(crate) fn commit_migration(
        &self,
        lock: &StoreLock,
        migration: Migration,
    ) -> Result<MigrationReport> {
        let Migration {
            report,
            writes,
            removes,
            placement,
        } = migration;
        if placement == Placement::Shared {
            self.ensure_repository(lock)?;
        }
        let message = CommitMessage::of(Tool::Migrate);
        self.commit_batch(lock, &writes, &removes, History::of(placement, &message))?;
        let patterns = self.root.join(PATTERNS_DIR);
        match fs::remove_dir(&patterns) {
            // A directory that holds other files keeps them.
            Ok(()) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {}
            Err(e) => diag!("warning: cannot remove {}: {e}", patterns.display()),
        }
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
    fn plan(&self, registry: &[Step], placement: PlacementSource) -> Result<Option<Migration>> {
        let Some((from, steps)) = self.migration_chain(registry)? else {
            return Ok(None);
        };
        let (mut tree, original) = self.read_tree(placement)?;
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
        let placement = current.placement.ok_or_else(|| Error::Unmigrated {
            path: self.project_path(),
        })?;
        Ok(Some(Migration {
            report: MigrationReport {
                from,
                steps: reports,
                rewritten: current.rewritten,
                removed: current.removed,
            },
            writes: current.writes,
            removes: current.removes,
            placement,
        }))
    }
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
    /// The placement the migrated `project.toml` states, once it is read.
    placement: Option<Placement>,
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
            placement: None,
        }
    }

    /// Read `table` as `file` in the current format; `before` is the bytes
    /// the file had on disk. A file the current format has no place for is
    /// refused: the step that leads to it must have removed it.
    fn add(&mut self, file: &File, table: toml::Table, before: Option<&[u8]>) -> Result<()> {
        match file {
            File::Project => {
                let project = self.node::<Project>(file, table, before)?;
                self.placement = Some(project.placement);
                Ok(())
            }
            File::Component(_) => self.node::<Component>(file, table, before).map(drop),
            File::Decision(_) => self.node::<Decision>(file, table, before).map(drop),
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
    ) -> Result<T> {
        let path = file.path(self.store.root());
        let parsed: T = table.try_into().map_err(Error::toml(&path))?;
        let write = self.store.prepare_write(&path, &parsed)?;
        if before != Some(write.content.as_bytes()) {
            self.rewritten.push(file.to_string());
            self.writes.push(write);
        }
        // clone: the commit owns the files it keeps; the tree keeps its own.
        self.kept.insert(file.clone());
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::store::corpus::Corpus;
    use crate::store::format::list_toml_stems;
    use crate::store::format::migration::testing::{fixture_store, step, tracked};
    use crate::store::format::migration::{
        Fact, GRAPH_FILE, PatternRecord, RefText, StepReport, Tree,
    };
    use crate::store::model::{COMPONENTS_DIR, DECISIONS_DIR, PROJECT_FILE};
    use crate::store::testing::{setup_store, setup_store_with_version};
    use crate::store::{PROJECT_COMPONENT, ProjectState, Status};
    use tempfile::TempDir;

    fn migrate(store: &Store) -> MigrationReport {
        let lock = store.lock().unwrap();
        let migration = store.plan_migration(&lock, tracked()).unwrap().unwrap();
        store.commit_migration(&lock, migration).unwrap()
    }

    /// The step to 0.5 maps every field of 0.4.1 and reports the rest, now
    /// that the typed read refuses what it has no place for.
    #[test]
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
                .preview_migration(tracked())
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
        let err = store
            .plan_migration(&lock, tracked())
            .map(|_| ())
            .unwrap_err();

        assert!(leftover.is_file(), "a refused store is not touched");
        drop(lock);
        assert_eq!(
            store
                .preview_migration(tracked())
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

        let migration = store.plan(&registry, tracked()).unwrap().unwrap();

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

        let err = store.plan(&registry, tracked()).map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Unmigrated { path } if *path == store.root().join(GRAPH_FILE)),
            "{err:?}"
        );
    }

    /// A 0.4.1 graph read straight from its TOML, as the check of its
    /// migration compares it: no code of the migration reads it.
    struct Old {
        /// The `[decision]` table of each decision file.
        decisions: BTreeMap<String, toml::Table>,
        components: BTreeSet<String>,
        /// The `[pattern]` table of each pattern file.
        patterns: BTreeMap<String, toml::Table>,
        /// `(from, to, kind)` of each edge of `graph.toml`.
        edges: BTreeSet<(String, String, String)>,
        /// The tags `graph.toml` gives each node.
        tags: BTreeMap<String, Vec<String>>,
    }

    fn read_old(root: &Path) -> Old {
        let table = |path: PathBuf| -> toml::Table {
            toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
        };
        let section = |dir: &str, key: &str| -> BTreeMap<String, toml::Table> {
            list_toml_stems(&root.join(dir))
                .unwrap()
                .into_iter()
                .map(|name| {
                    let file = table(root.join(dir).join(format!("{name}.toml")));
                    (name, file[key].as_table().unwrap().clone())
                })
                .collect()
        };
        let index = table(root.join(GRAPH_FILE));
        let text = |value: &toml::Value, key: &str| value[key].as_str().unwrap().to_owned();
        let strings = |value: Option<&toml::Value>| -> Vec<String> {
            value.map_or_else(Vec::new, |list| {
                list.as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item.as_str().unwrap().to_owned())
                    .collect()
            })
        };
        Old {
            decisions: section(DECISIONS_DIR, "decision"),
            components: section(COMPONENTS_DIR, "component").into_keys().collect(),
            patterns: section(PATTERNS_DIR, "pattern"),
            edges: index["edges"]
                .as_array()
                .unwrap()
                .iter()
                .map(|edge| (text(edge, "from"), text(edge, "to"), text(edge, "kind")))
                .collect(),
            tags: index["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|node| (text(node, "name"), strings(node.get("tags"))))
                .collect(),
        }
    }

    impl Old {
        /// The targets of the edges of `kind` from `from`, and the sources
        /// of the edges of `reversed` to it.
        fn targets(&self, from: &str, kind: &str, reversed: &str) -> BTreeSet<String> {
            let forward = self
                .edges
                .iter()
                .filter(|e| e.0 == from && e.2 == kind)
                .map(|e| &e.1);
            let back = self
                .edges
                .iter()
                .filter(|e| e.1 == from && e.2 == reversed)
                .map(|e| &e.0);
            forward.chain(back).cloned().collect()
        }

        /// The anchors of `name` as the report says the migration rewrote
        /// and dropped its code refs.
        fn anchors(&self, name: &str, report: &StepReport) -> BTreeSet<(String, Option<String>)> {
            let file = File::Decision(name.into()).to_string();
            let respelled: BTreeMap<&RefText, &RefText> = report
                .facts
                .iter()
                .filter_map(|fact| match fact {
                    Fact::Normalized { file: of, old, new } if *of == file => Some((old, new)),
                    _ => None,
                })
                .collect();
            let lost: BTreeSet<&str> = report
                .losses
                .iter()
                .filter(|loss| loss.file == file)
                .map(|loss| loss.field.as_str())
                .collect();
            let refs = self.decisions[name]
                .get("code_refs")
                .and_then(toml::Value::as_array);
            let mut anchors = BTreeSet::new();
            for (at, code_ref) in refs.into_iter().flatten().enumerate() {
                let field = format!("decision.code_refs[{at}]");
                if lost.contains(field.as_str()) {
                    continue;
                }
                let old = RefText {
                    file: code_ref["file"].as_str().unwrap().to_owned(),
                    symbol: code_ref
                        .get("symbol")
                        .and_then(toml::Value::as_str)
                        .map(str::to_owned),
                };
                let mut new = respelled
                    .get(&old)
                    .map_or(old.clone(), |new| (*new).clone());
                if lost.contains(format!("{field}.symbol").as_str()) {
                    new.symbol = None;
                }
                anchors.insert((new.file, new.symbol));
            }
            anchors
        }
    }

    /// What the migration must keep of `old` in `state`, and what its
    /// report must hold of what it took out of the graph.
    fn assert_migrated(old: &Old, report: &MigrationReport, state: &ProjectState) {
        let step = report.steps.last().unwrap();
        assert_eq!(
            state.decisions.keys().collect::<Vec<_>>(),
            old.decisions.keys().collect::<Vec<_>>()
        );
        for (name, decision) in &state.decisions {
            let was = &old.decisions[name];
            let text = |key: &str| was[key].as_str().unwrap();
            assert_eq!(
                (
                    decision.component.as_str(),
                    decision.choice.as_str(),
                    decision.reason.as_str()
                ),
                (text("component"), text("choice"), text("reason")),
                "{name}"
            );
            let list = |key: &str| -> Vec<String> {
                was.get(key)
                    .and_then(toml::Value::as_array)
                    .map_or_else(Vec::new, |list| {
                        list.iter()
                            .map(|item| item.as_str().unwrap().to_owned())
                            .collect()
                    })
            };
            assert_eq!(decision.alternatives, list("alternatives"), "{name}");
            assert_eq!(decision.tags, list("tags").into_iter().collect(), "{name}");
            assert_eq!(
                decision.depends_on,
                old.targets(name, "depends_on", "constrains"),
                "{name}"
            );
            let anchors: BTreeSet<(String, Option<String>)> = decision
                .anchors
                .iter()
                .map(|anchor| (anchor.file().to_owned(), anchor.symbol().map(str::to_owned)))
                .collect();
            assert_eq!(anchors, old.anchors(name, step), "{name}");
        }
        assert_eq!(
            state.components.keys().cloned().collect::<BTreeSet<_>>(),
            old.components
        );
        for (name, component) in &state.components {
            assert_eq!(
                component.connects_to,
                old.targets(name, "connects_to", ""),
                "{name}"
            );
        }
        for (name, pattern) in &old.patterns {
            let text = |key: &str| {
                pattern
                    .get(key)
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            };
            let edges = |kind: &str| old.targets(name, kind, "").into_iter().collect();
            let record = Fact::Pattern(PatternRecord {
                name: name.clone(),
                title: text("name"),
                description: text("description"),
                tags: old.tags.get(name).cloned().unwrap_or_default(),
                member_of: edges("member_of"),
                applies_to: edges("applies_to"),
            });
            assert!(step.facts.contains(&record), "{record:?}");
        }
        let components = old
            .components
            .iter()
            .map(String::as_str)
            .chain([PROJECT_COMPONENT]);
        for node in components {
            let tags = old.tags.get(node).cloned().unwrap_or_default();
            let reported = step.facts.contains(&Fact::Tags {
                node: node.into(),
                tags: tags.clone(),
            });
            assert!(reported || tags.is_empty(), "{node}: {tags:?}");
        }
    }

    #[test]
    fn the_0_4_1_fixture_keeps_its_decisions_edges_tags_and_anchors_and_reports_the_rest() {
        let (_tmp, store) = fixture_store("0.4.1");
        let old = read_old(store.root());

        let report = migrate(&store);

        assert_migrated(&old, &report, &store.load_state().unwrap());
    }

    /// `state`, the seeded corpus, written over its store in format 0.4.1:
    /// one `[decision]` table per file with `attribution` and `code_refs`,
    /// and every edge in `graph.toml`, each second `depends_on` as the
    /// `constrains` it reverses.
    fn write_0_4_1(store: &Store, state: &ProjectState) {
        let write = |path: PathBuf, table: toml::Table| {
            fs::write(path, toml::to_string(&table).unwrap()).unwrap();
        };
        let wrap = |key: &str, fields: toml::Table| {
            let mut table = toml::Table::new();
            table.insert(key.into(), fields.into());
            table
        };
        let mut project = toml::Table::new();
        project.insert("name".into(), state.project.name.as_str().into());
        project.insert("description".into(), "".into());
        let mut file = wrap("project", project);
        file.insert("format_version".into(), "0.4.1".into());
        write(store.project_path(), file);
        let mut edges = Vec::new();
        let mut edge = |from: &str, to: &str, kind: &str| {
            let fields: toml::Table = [("from", from), ("to", to), ("kind", kind)]
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.into()))
                .collect();
            edges.push(toml::Value::Table(fields));
        };
        for (name, component) in &state.components {
            let mut fields = toml::Table::new();
            fields.insert("name".into(), name.as_str().into());
            fields.insert("description".into(), component.description.as_str().into());
            write(store.component_path(name), wrap("component", fields));
            for to in &component.connects_to {
                edge(name, to, "connects_to");
            }
        }
        for (at, (name, decision)) in state.decisions.iter().enumerate() {
            for to in &decision.depends_on {
                match at % 2 {
                    0 => edge(name, to, "depends_on"),
                    _ => edge(to, name, "constrains"),
                }
            }
            edge(name, &decision.component, "belongs_to");
            let mut fields = toml::Table::new();
            for (key, text) in [
                ("component", &decision.component),
                ("choice", &decision.choice),
                ("reason", &decision.reason),
            ] {
                fields.insert(key.into(), text.as_str().into());
            }
            let strings = |list: &mut dyn Iterator<Item = &String>| -> toml::Value {
                toml::Value::Array(list.map(|item| item.as_str().into()).collect())
            };
            fields.insert(
                "alternatives".into(),
                strings(&mut decision.alternatives.iter()),
            );
            fields.insert("tags".into(), strings(&mut decision.tags.iter()));
            let attribution = match decision.status {
                Status::Proposed { .. } => "agent",
                Status::Accepted { .. } => "user",
            };
            fields.insert("attribution".into(), attribution.into());
            fields.insert("created".into(), decision.created.to_string().into());
            let refs = decision.anchors.iter().map(|anchor| {
                let text = RefText {
                    file: anchor.file().to_owned(),
                    symbol: anchor.symbol().map(str::to_owned),
                };
                text.to_value()
            });
            fields.insert("code_refs".into(), toml::Value::Array(refs.collect()));
            write(store.decision_path(name), wrap("decision", fields));
        }
        let mut index = toml::Table::new();
        index.insert("version".into(), 1.into());
        index.insert("nodes".into(), toml::Value::Array(Vec::new()));
        index.insert("edges".into(), toml::Value::Array(edges));
        write(store.root().join(GRAPH_FILE), index);
    }

    /// The seeded corpus at the smallest bench size, written as 0.4.1,
    /// migrates back to itself: only what 0.4.1 cannot state differs, the
    /// status's `accepted_via` and `accepted_text`, and `origin`.
    #[test]
    fn the_seeded_corpus_written_as_0_4_1_migrates_back_to_itself() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(Corpus::SIZES[0]).write(tmp.path()).unwrap();
        let mut corpus = store.load_state().unwrap();
        write_0_4_1(&store, &corpus);
        let old = read_old(store.root());

        let report = migrate(&store);

        let state = store.load_state().unwrap();
        assert_migrated(&old, &report, &state);
        assert_eq!(report.steps.last().unwrap().losses, []);
        for decision in corpus.decisions.values_mut() {
            decision.origin = None;
            decision.status = match decision.status {
                Status::Proposed { .. } => Status::Proposed {
                    accepted_text: None,
                },
                Status::Accepted { .. } => Status::Accepted { via: None },
            };
        }
        assert_eq!(state.decisions, corpus.decisions);
        assert_eq!(state.components, corpus.components);
        assert_eq!(state.validate(), []);
        assert_eq!(state.non_canonical, []);
    }
}
