//! The step from format 0.4.1 to 0.5: node files only, each edge in the
//! file of its source.
//!
//! `graph.toml` leaves the store. Its `connects_to` and `depends_on` edges
//! move into the files of their sources, each `A constrains B` becomes
//! `B depends_on A`, and a `belongs_to` edge is what the decision's
//! `component` already states. Patterns, with their `member_of` and
//! `applies_to` edges and tags, and the tags of components leave the graph
//! for the report. A decision's `attribution` becomes its `status`, its
//! `code_refs` its `anchors`, and its history is dropped and counted. The
//! `name` field of a node file goes: the stem is the name. Every other key
//! a 0.4.1 file holds, and every edge or node entry that names no node of
//! its kind, is a [`Loss`]: a field this step does not map is reported,
//! never refused.

use std::mem;

use toml::{Table, Value};

use crate::Result;

mod anchors;
mod graph;

use super::super::schema::Placement;
use super::{Fact, File, Loss, PatternRecord, Tree, loss};

use anchors::anchors;
use graph::Graph;

pub(super) fn reshape(tree: &mut Tree) -> Result<()> {
    let Tree {
        files,
        losses,
        facts,
        placement,
    } = tree;
    let mut notes = Notes { facts, losses };
    let (placement, basis) = *placement;
    notes.facts.push(Fact::Placement { placement, basis });
    let index = files.remove(&File::Index);
    let mut graph = Graph::default();
    for (file, table) in mem::take(files) {
        if let Some(table) = graph.read_file(&file, table, placement, &mut notes) {
            files.insert(file, table);
        }
    }
    if let Some(index) = index {
        graph.read_index(index, &mut notes);
    }
    graph.refuse_new_cycles()?;
    graph.write(files, &mut notes);
    Ok(())
}

/// Where the step reports what it drops and what else it did.
struct Notes<'a> {
    facts: &'a mut Vec<Fact>,
    losses: &'a mut Vec<Loss>,
}

impl Notes<'_> {
    fn loss(&mut self, file: &File, field: &str, value: &Value) {
        self.losses.push(loss(file, field, value));
    }

    /// Each entry of `fields`, which nothing maps, at `section.<key>`.
    fn rest(&mut self, file: &File, section: &str, fields: Table) {
        for (key, value) in fields {
            self.loss(file, &format!("{section}.{key}"), &value);
        }
    }

    /// The top-level keys of a node file besides its section.
    fn rest_top(&mut self, file: &File, table: Table) {
        for (key, value) in table {
            self.loss(file, &key, &value);
        }
    }
}

/// `project.toml` with its `[project]` fields at the top level and the
/// placement the tree carries.
fn project(mut table: Table, placement: Placement, notes: &mut Notes<'_>) -> Table {
    let mut project = Table::new();
    if let Some(version) = table.remove("format_version") {
        project.insert("format_version".into(), version);
    }
    let fields = section(&File::Project, "project", &mut table, notes);
    move_keys(fields, &mut project, &["name", "description"], |fields| {
        notes.rest(&File::Project, "project", fields);
    });
    notes.rest_top(&File::Project, table);
    project.insert("placement".into(), placement.as_str().into());
    project
}

fn component(file: &File, name: &str, mut table: Table, notes: &mut Notes<'_>) -> Table {
    let mut fields = section(file, "component", &mut table, notes);
    notes.rest_top(file, table);
    stem_is_name(file, name, fields.remove("name"), "component", notes);
    let mut component = Table::new();
    move_keys(fields, &mut component, &["description"], |fields| {
        notes.rest(file, "component", fields);
    });
    component
}

/// The keys a 0.5 decision takes from 0.4.1 as they are.
const DECISION_KEYS: [&str; 6] = [
    "component",
    "choice",
    "reason",
    "alternatives",
    "tags",
    "created",
];

fn decision(file: &File, name: &str, mut table: Table, notes: &mut Notes<'_>) -> Table {
    let mut fields = section(file, "decision", &mut table, notes);
    notes.rest_top(file, table);
    stem_is_name(file, name, fields.remove("name"), "decision", notes);
    let mut decision = Table::new();
    decision.insert(
        "status".into(),
        status(file, fields.remove("attribution"), notes).into(),
    );
    if let Some(code_refs) = fields.remove("code_refs") {
        let anchors = anchors(file, code_refs, notes);
        if !anchors.is_empty() {
            decision.insert("anchors".into(), Value::Array(anchors));
        }
    }
    match fields.remove("history") {
        Some(Value::Array(entries)) if !entries.is_empty() => notes.facts.push(Fact::History {
            file: file.to_string(),
            entries: entries.len(),
        }),
        Some(Value::Array(_)) | None => {}
        Some(other) => notes.loss(file, "decision.history", &other),
    }
    move_keys(fields, &mut decision, &DECISION_KEYS, |fields| {
        notes.rest(file, "decision", fields);
    });
    decision
}

/// `agent` is a proposal the owner has not accepted; `user`, and no
/// attribution, the owner's own decision. Any other value 0.4.1 refused to
/// read, so no acceptance is known: the decision is proposed.
fn status(file: &File, attribution: Option<Value>, notes: &mut Notes<'_>) -> &'static str {
    let Some(attribution) = attribution else {
        return "accepted";
    };
    match attribution.as_str() {
        Some("user") => "accepted",
        Some("agent") => "proposed",
        Some(_) | None => {
            notes.loss(file, "decision.attribution", &attribution);
            "proposed"
        }
    }
}

fn pattern(file: &File, name: &str, mut table: Table, notes: &mut Notes<'_>) -> PatternRecord {
    let mut fields = section(file, "pattern", &mut table, notes);
    notes.rest_top(file, table);
    let mut text = |key: &str| match fields.remove(key) {
        Some(Value::String(text)) => Some(text),
        Some(other) => {
            notes.loss(file, &format!("pattern.{key}"), &other);
            None
        }
        None => None,
    };
    let record = PatternRecord {
        name: name.to_owned(),
        title: text("name"),
        description: text("description"),
        ..PatternRecord::default()
    };
    notes.rest(file, "pattern", fields);
    record
}

/// The table `[section]` of `table`, taken out of it; empty when there is
/// none, and the value reported when it is no table.
fn section(file: &File, section: &str, table: &mut Table, notes: &mut Notes<'_>) -> Table {
    match table.remove(section) {
        Some(Value::Table(fields)) => fields,
        Some(other) => {
            notes.loss(file, section, &other);
            Table::new()
        }
        None => Table::new(),
    }
}

/// Move each of `keys` from `fields` to `to`, then hand what is left to
/// `rest`.
fn move_keys(mut fields: Table, to: &mut Table, keys: &[&str], rest: impl FnOnce(Table)) {
    for &key in keys {
        if let Some(value) = fields.remove(key) {
            to.insert(key.to_owned(), value);
        }
    }
    rest(fields);
}

/// The `name` field of the node file `file`, whose stem is `stem`.
fn stem_is_name(
    file: &File,
    stem: &str,
    name: Option<Value>,
    section: &str,
    notes: &mut Notes<'_>,
) {
    match name {
        Some(Value::String(name)) if name != stem => notes.facts.push(Fact::Renamed {
            file: file.to_string(),
            name,
        }),
        Some(Value::String(_)) | None => {}
        Some(other) => notes.loss(file, &format!("{section}.name"), &other),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::RefText;
    use super::super::testing::fixture_tree;
    use super::*;
    use crate::Error;
    use crate::store::PROJECT_COMPONENT;

    const API: &str = "api";
    const JSON_RPC: &str = "json-rpc-over-stdio";
    const LOCK: &str = "lock-before-every-write";
    const ONE_FILE: &str = "one-toml-file-per-node";
    const TEMP_FILE: &str = "write-through-a-temp-file-and-rename";

    fn decision_file(name: &str) -> File {
        File::Decision(name.into())
    }

    /// The 0.4.1 fixture, `edit` applied to its tables, after the step.
    fn reshaped(edit: impl FnOnce(&mut BTreeMap<File, Table>)) -> Tree {
        let (_tmp, mut tree) = fixture_tree("0.4.1");
        edit(&mut tree.files);
        reshape(&mut tree).unwrap();
        tree
    }

    fn strings(table: &Table, key: &str) -> Vec<String> {
        table
            .get(key)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |list| {
                list.iter()
                    .map(|item| item.as_str().unwrap().to_owned())
                    .collect()
            })
    }

    fn losses(tree: &Tree) -> Vec<(String, String, String)> {
        tree.losses
            .iter()
            .map(|loss| (loss.file.clone(), loss.field.clone(), loss.value.clone()))
            .collect()
    }

    /// Append the edge `from -> to` of `kind` to `graph.toml`.
    fn add_edge(files: &mut BTreeMap<File, Table>, from: &str, to: &str, kind: &str) {
        let mut edge = Table::new();
        for (key, value) in [("from", from), ("to", to), ("kind", kind)] {
            edge.insert(key.into(), value.into());
        }
        let edges = files.get_mut(&File::Index).unwrap()["edges"]
            .as_array_mut()
            .unwrap();
        edges.push(Value::Table(edge));
    }

    #[test]
    fn edges_move_into_their_sources_and_constrains_turns_around() {
        let tree = reshaped(|_| {});

        assert!(!tree.files.contains_key(&File::Index));
        assert_eq!(
            strings(&tree.files[&File::Component(API.into())], "connects_to"),
            ["store"]
        );
        let temp_file = &tree.files[&decision_file(TEMP_FILE)];
        assert_eq!(strings(temp_file, "depends_on"), [JSON_RPC, ONE_FILE]);
        assert_eq!(
            strings(&tree.files[&decision_file(LOCK)], "depends_on"),
            [TEMP_FILE]
        );
        assert!(tree.facts.contains(&Fact::Reversed {
            from: JSON_RPC.into(),
            to: TEMP_FILE.into(),
        }));
        // `belongs_to` restates each decision's component.
        assert!(
            losses(&tree).iter().all(|(file, ..)| file != "graph.toml"),
            "{:?}",
            tree.losses
        );
    }

    #[test]
    fn agent_is_proposed_and_user_or_none_accepted_with_no_accepted_via() {
        let tree = reshaped(|files| {
            let decision = files.get_mut(&decision_file(ONE_FILE)).unwrap()["decision"]
                .as_table_mut()
                .unwrap();
            decision.insert("attribution".into(), "robot".into());
        });

        let status = |name: &str| tree.files[&decision_file(name)]["status"].as_str().unwrap();
        assert_eq!(status(JSON_RPC), "proposed", "agent");
        assert_eq!(status(TEMP_FILE), "accepted", "user");
        assert_eq!(status(LOCK), "accepted", "no attribution");
        assert_eq!(status(ONE_FILE), "proposed", "an attribution 0.4.1 refused");
        assert!(losses(&tree).contains(&(
            "decisions/one-toml-file-per-node.toml".into(),
            "decision.attribution".into(),
            "\"robot\"".into()
        )));
        for table in tree.files.values() {
            assert!(!table.contains_key("accepted_via") && !table.contains_key("attribution"));
        }
    }

    #[test]
    fn history_is_dropped_and_counted() {
        let tree = reshaped(|_| {});

        let history: Vec<&Fact> = tree
            .facts
            .iter()
            .filter(|fact| matches!(fact, Fact::History { .. }))
            .collect();
        assert_eq!(
            history,
            [
                &Fact::History {
                    file: decision_file(JSON_RPC).to_string(),
                    entries: 1
                },
                &Fact::History {
                    file: decision_file(LOCK).to_string(),
                    entries: 2
                },
            ]
        );
        assert!(
            tree.files
                .values()
                .all(|table| !table.contains_key("history"))
        );
    }

    #[test]
    fn patterns_and_component_tags_leave_the_graph_whole_and_decision_tags_stay() {
        let tree = reshaped(|_| {});

        assert!(
            tree.files
                .keys()
                .all(|file| !matches!(file, File::Pattern(_)))
        );
        assert!(tree.facts.contains(&Fact::Pattern(PatternRecord {
            name: "durable-storage".into(),
            title: Some("Durable storage".into()),
            description: Some("Node files are written whole or not at all".into()),
            tags: vec!["storage".into()],
            member_of: vec![LOCK.into(), ONE_FILE.into(), TEMP_FILE.into()],
            applies_to: vec!["store".into()],
        })));
        let tags = |node: &str, tags: &[&str]| Fact::Tags {
            node: node.into(),
            tags: tags.iter().map(|&tag| tag.to_owned()).collect(),
        };
        for fact in [
            tags(API, &["protocol", "server"]),
            tags(PROJECT_COMPONENT, &["rules"]),
            tags("store", &["persistence"]),
        ] {
            assert!(tree.facts.contains(&fact), "{fact:?}");
        }
        let temp_file = &tree.files[&decision_file(TEMP_FILE)];
        assert_eq!(strings(temp_file, "tags"), ["storage", "reliability"]);
    }

    #[test]
    fn the_project_fields_move_to_the_top_level_beside_the_placement() {
        let tree = reshaped(|_| {});

        let project = &tree.files[&File::Project];
        let keys: Vec<&str> = project.keys().map(String::as_str).collect();
        assert_eq!(keys, ["description", "format_version", "name", "placement"]);
        assert_eq!(project["name"].as_str(), Some("demo"));
        assert_eq!(project["placement"].as_str(), Some("tracked"));
        assert!(tree.facts.contains(&Fact::Placement {
            placement: Placement::Tracked,
            basis: super::super::PlacementBasis::Stated,
        }));
    }

    #[test]
    fn a_name_field_goes_and_one_that_differs_from_the_stem_is_reported() {
        let tree = reshaped(|files| {
            let api = files.get_mut(&File::Component(API.into())).unwrap()["component"]
                .as_table_mut()
                .unwrap();
            api.insert("name".into(), "API".into());
        });

        assert!(
            tree.files
                .values()
                .all(|table| !table.contains_key("name") || table.contains_key("format_version"))
        );
        let renamed: Vec<&Fact> = tree
            .facts
            .iter()
            .filter(|fact| matches!(fact, Fact::Renamed { .. }))
            .collect();
        assert_eq!(
            renamed,
            [&Fact::Renamed {
                file: "components/api.toml".into(),
                name: "API".into()
            }]
        );
    }

    #[test]
    fn code_refs_become_anchors_in_the_0_5_spelling_and_the_rest_is_reported() {
        let tree = reshaped(|_| {});

        let anchors: Vec<String> = tree.files[&decision_file(LOCK)]["anchors"]
            .as_array()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            anchors,
            [
                "{ file = \"src/store/lock.rs\", symbol = \"StoreLock\" }",
                "{ file = \"src/store/lock.rs\", symbol = \"lock\" }",
                "{ file = \"src/store/commit.rs\", symbol = \"commit_batch\" }",
                "{ file = \"Cargo.toml\", symbol = \"profile::release::panic\" }",
                "{ file = \"tools/check.py\", symbol = \"Checker::run\" }",
                "{ file = \"README.md\", symbol = \"Install\" }",
            ]
        );
        let file = decision_file(LOCK).to_string();
        let anchor = |path: &str, symbol: &str| RefText {
            file: path.into(),
            symbol: Some(symbol.into()),
        };
        let normalized: Vec<(RefText, RefText)> = tree
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Normalized { old, new, .. } => Some((old.clone(), new.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            normalized,
            [
                (
                    anchor("src/store/lock.rs", "lock()"),
                    anchor("src/store/lock.rs", "lock")
                ),
                (
                    anchor("./src/store/commit.rs", "commit_batch"),
                    anchor("src/store/commit.rs", "commit_batch")
                ),
                (
                    anchor("Cargo.toml", "profile.release.panic"),
                    anchor("Cargo.toml", "profile::release::panic")
                ),
                (
                    anchor("tools/check.py", "Checker.run"),
                    anchor("tools/check.py", "Checker::run")
                ),
            ]
        );
        assert!(tree.facts.iter().any(|fact| matches!(fact,
            Fact::Unnormalized { anchor: kept, .. } if *kept == anchor("README.md", "Install"))));
        assert_eq!(
            losses(&tree),
            [
                (
                    file.clone(),
                    "decision.code_refs[0].line".into(),
                    "12".into()
                ),
                (
                    file,
                    "decision.code_refs[3]".into(),
                    "{ file = \"../shared/lock.rs\" }".into()
                ),
            ]
        );
    }

    #[test]
    fn a_constrains_edge_that_closes_a_cycle_is_refused_naming_its_members() {
        let (_tmp, mut tree) = fixture_tree("0.4.1");
        add_edge(&mut tree.files, TEMP_FILE, ONE_FILE, "constrains");

        let err = reshape(&mut tree).unwrap_err();

        assert!(
            matches!(&err, Error::ConstrainsCycle { members } if *members == [ONE_FILE, TEMP_FILE]),
            "{err:?}"
        );
    }

    /// 0.4.1 refused such a cycle on every write, so only a hand edit made
    /// it; `check` names it once the store is migrated.
    #[test]
    fn a_cycle_of_depends_on_edges_alone_is_migrated_for_check_to_report() {
        let tree = reshaped(|files| add_edge(files, ONE_FILE, TEMP_FILE, "depends_on"));

        assert_eq!(
            strings(&tree.files[&decision_file(ONE_FILE)], "depends_on"),
            [TEMP_FILE]
        );
    }

    #[test]
    fn an_edge_or_a_node_entry_that_names_no_node_of_its_kind_is_reported() {
        let tree = reshaped(|files| {
            add_edge(files, API, "gone", "connects_to");
            add_edge(files, JSON_RPC, API, "depends_on");
            add_edge(files, JSON_RPC, ONE_FILE, "supersedes");
            add_edge(files, "durable-storage", API, "member_of");
            let index = files.get_mut(&File::Index).unwrap();
            let mut node = Table::new();
            node.insert("name".into(), "gone".into());
            index["nodes"]
                .as_array_mut()
                .unwrap()
                .push(Value::Table(node));
        });

        let reported: Vec<String> = losses(&tree)
            .into_iter()
            .filter(|(file, ..)| file == "graph.toml")
            .map(|(_, field, value)| format!("{field} = {value}"))
            .collect();
        assert_eq!(
            reported,
            [
                "edges = { from = \"api\", kind = \"connects_to\", to = \"gone\" }",
                "edges = { from = \"json-rpc-over-stdio\", kind = \"depends_on\", to = \"api\" }",
                "edges = { from = \"json-rpc-over-stdio\", kind = \"supersedes\", to = \"one-toml-file-per-node\" }",
                "edges = { from = \"durable-storage\", kind = \"member_of\", to = \"api\" }",
                "nodes[9] = { name = \"gone\" }",
            ]
        );
    }

    #[test]
    fn a_key_of_any_0_4_1_file_the_step_does_not_map_is_reported() {
        let tree = reshaped(|files| {
            let mut put = |file: File, path: &[&str], key: &str| {
                let mut table = files.get_mut(&file).unwrap();
                for section in path {
                    table = table.get_mut(*section).unwrap().as_table_mut().unwrap();
                }
                table.insert(key.into(), 1.into());
            };
            put(File::Project, &[], "owner");
            put(File::Project, &["project"], "owner");
            put(File::Component(API.into()), &["component"], "owner");
            put(decision_file(JSON_RPC), &["decision"], "status");
            put(
                File::Pattern("durable-storage".into()),
                &["pattern"],
                "owner",
            );
            put(File::Index, &[], "owner");
        });

        let reported: Vec<String> = losses(&tree)
            .into_iter()
            .map(|(file, field, _)| format!("{file}: {field}"))
            .collect();
        for expected in [
            "project.toml: owner",
            "project.toml: project.owner",
            "components/api.toml: component.owner",
            "decisions/json-rpc-over-stdio.toml: decision.status",
            "patterns/durable-storage.toml: pattern.owner",
            "graph.toml: owner",
        ] {
            assert!(
                reported.contains(&expected.to_owned()),
                "{expected}: {reported:?}"
            );
        }
    }
}
