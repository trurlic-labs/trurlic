//! The migration registry: one step per format change, oldest first, and
//! the walk of a chain of them over a [`Tree`].
//!
//! A step rewrites a [`Tree`] in one format into the next: it changes the
//! tables of the files the change touches and records in [`Tree::losses`]
//! every value it drops. The walk stamps the step's `to` version after it,
//! so a step changes the version key only when the format renames it. A
//! new format appends one step here and raises `FORMAT_VERSION` to its
//! `to`; a test holds the chain unbroken.

use std::mem;

use toml::Value;

use crate::Result;

use super::format_0_5;
use super::report::StepReport;
use super::tree::{File, Tree, loss};
use crate::store::model::{FORMAT_VERSION, FormatVersion};

/// One format change.
pub(in crate::store) struct Step {
    pub(in crate::store) from: FormatVersion,
    pub(in crate::store) to: FormatVersion,
    /// The change, in a few words, as the report names it.
    pub(in crate::store) change: &'static str,
    pub(in crate::store) apply: fn(&mut Tree) -> Result<()>,
}

pub(in crate::store) const STEPS: [Step; 5] = [
    Step {
        from: FormatVersion::new(0, 1, 0),
        to: FormatVersion::new(0, 2, 0),
        change: "connections and supersedes links move into graph.toml",
        apply: move_edges_into_index,
    },
    Step {
        from: FormatVersion::new(0, 2, 0),
        to: FormatVersion::new(0, 3, 0),
        change: "decisions record who made them",
        apply: attribute_to_user,
    },
    Step {
        from: FormatVersion::new(0, 3, 0),
        to: FormatVersion::new(0, 4, 0),
        change: "the supersedes edge is retired",
        apply: drop_supersedes_edges,
    },
    Step {
        from: FormatVersion::new(0, 4, 0),
        to: FormatVersion::new(0, 4, 1),
        change: "the version key becomes format_version",
        apply: rename_version_key,
    },
    Step {
        from: FormatVersion::new(0, 4, 1),
        to: FormatVersion::new(0, 5, 0),
        change: "node files only: edges in their sources, no patterns, status for attribution",
        apply: format_0_5::reshape,
    },
];

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

/// In 0.1.0 a component listed the components it connects to and a
/// decision named the decision it supersedes; 0.2.0 holds both as edges
/// of `graph.toml`, which it introduced. Its node entries and `belongs_to`
/// edges are derived from the node files when the migration is written.
/// The pre-release `trurl` builds named the version key `trurl_version`.
fn move_edges_into_index(tree: &mut Tree) -> Result<()> {
    rename_key(tree, "trurl_version", "trurlic_version");
    let Tree { files, losses, .. } = tree;
    let mut edges = Vec::new();
    for (file, table) in files.iter_mut() {
        // Each field becomes edges of the kind named like it.
        let (name, section, field) = match file {
            File::Component(name) => (name, "component", "connects_to"),
            File::Decision(name) => (name, "decision", "supersedes"),
            File::Project | File::Index | File::Pattern(_) => continue,
        };
        let removed = table
            .get_mut(section)
            .and_then(Value::as_table_mut)
            .and_then(|node| node.remove(field));
        let targets = match removed {
            None => continue,
            Some(Value::Array(targets)) => targets,
            Some(target) => vec![target],
        };
        for target in targets {
            match target {
                Value::String(target) => edges.push(edge(name, target, field)),
                other => losses.push(loss(file, &format!("{section}.{field}"), &other)),
            }
        }
    }
    append_edges(tree, edges);
    Ok(())
}

/// Append `edges` to those of `graph.toml`, which a 0.1.0 store lacks.
fn append_edges(tree: &mut Tree, edges: Vec<Value>) {
    let index = tree.files.entry(File::Index).or_insert_with(|| {
        let mut index = toml::Table::new();
        index.insert("version".into(), 1.into());
        index
    });
    match index
        .entry("edges")
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(list) => list.extend(edges),
        other => {
            let replaced = mem::replace(other, Value::Array(edges));
            tree.losses.push(loss(&File::Index, "edges", &replaced));
        }
    }
}

/// An edge table of `graph.toml`.
fn edge(from: &str, to: String, kind: &str) -> Value {
    let mut edge = toml::Table::new();
    edge.insert("from".into(), from.into());
    edge.insert("to".into(), to.into());
    edge.insert("kind".into(), kind.into());
    Value::Table(edge)
}

/// 0.3.0 records whether the user or an agent made a decision, and reads a
/// decision without the field as the user's, which every 0.2.0 decision
/// was; the step writes that out.
fn attribute_to_user(tree: &mut Tree) -> Result<()> {
    for (file, table) in &mut tree.files {
        if let File::Decision(_) = file
            && let Some(decision) = table.get_mut("decision").and_then(Value::as_table_mut)
        {
            decision
                .entry("attribution")
                .or_insert_with(|| "user".into());
        }
    }
    Ok(())
}

/// 0.4.0 has no `supersedes` edge kind: a superseded decision is revised
/// in place instead. The edges are dropped.
fn drop_supersedes_edges(tree: &mut Tree) -> Result<()> {
    let Tree { files, losses, .. } = tree;
    let edges = files
        .get_mut(&File::Index)
        .and_then(|index| index.get_mut("edges"))
        .and_then(Value::as_array_mut);
    if let Some(edges) = edges {
        edges.retain(|edge| {
            let retired = edge.get("kind").and_then(Value::as_str) == Some("supersedes");
            if retired {
                losses.push(loss(&File::Index, "edges", edge));
            }
            !retired
        });
    }
    Ok(())
}

/// 0.4.1 names the version key `format_version`: the version is the
/// format's, not the release's.
fn rename_version_key(tree: &mut Tree) -> Result<()> {
    rename_key(tree, "trurlic_version", "format_version");
    Ok(())
}

/// Move the value of `old` in `project.toml` to `new`. When `new` is there
/// already, it is what the version was read from, and `old` is dropped.
fn rename_key(tree: &mut Tree, old: &str, new: &str) {
    let project = tree.project();
    let Some(value) = project.remove(old) else {
        return;
    };
    if !project.contains_key(new) {
        project.insert(new.to_owned(), value);
        return;
    }
    tree.losses.push(loss(&File::Project, old, &value));
}

#[cfg(test)]
mod tests {
    use super::super::testing::{fixture_tree, step};
    use super::*;
    use crate::store::FORMAT_VERSION;

    fn apply(step: &Step, tree: &mut Tree) {
        (step.apply)(tree).unwrap();
    }

    fn table<'a>(tree: &'a Tree, file: &File, section: &str) -> &'a toml::Table {
        tree.files[file][section].as_table().unwrap()
    }

    fn edges(tree: &Tree) -> Vec<(String, String, String)> {
        let field = |edge: &Value, key: &str| edge[key].as_str().unwrap().to_owned();
        tree.files[&File::Index]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|edge| (field(edge, "from"), field(edge, "to"), field(edge, "kind")))
            .collect()
    }

    #[test]
    fn the_steps_chain_from_the_first_format_on() {
        assert_eq!(STEPS[0].from, FormatVersion::new(0, 1, 0));
        for pair in STEPS.windows(2) {
            assert_eq!(pair[0].to, pair[1].from);
        }
        for step in &STEPS {
            assert!(step.from < step.to, "{} -> {}", step.from, step.to);
        }
    }

    #[test]
    fn the_last_step_reaches_the_current_format() {
        assert_eq!(STEPS[STEPS.len() - 1].to, FORMAT_VERSION);
    }

    #[test]
    fn from_0_1_0_connections_and_supersedes_links_become_edges() {
        let (_tmp, mut tree) = fixture_tree("0.1.0");
        assert!(!tree.files.contains_key(&File::Index));

        apply(&STEPS[0], &mut tree);

        let triple = |from: &str, to: &str, kind: &str| (from.into(), to.into(), kind.into());
        assert_eq!(
            edges(&tree),
            [
                triple("api", "store", "connects_to"),
                triple(
                    "newline-delimited-json-rpc-over-stdio",
                    "json-rpc-over-stdio",
                    "supersedes"
                ),
            ]
        );
        let api = table(&tree, &File::Component("api".into()), "component");
        assert!(!api.contains_key("connects_to"));
        let newline = File::Decision("newline-delimited-json-rpc-over-stdio".into());
        assert!(!table(&tree, &newline, "decision").contains_key("supersedes"));
        assert_eq!(tree.project()["trurlic_version"].as_str(), Some("0.1.0"));
        assert!(!tree.project().contains_key("trurl_version"));
        assert!(tree.losses.is_empty(), "{:?}", tree.losses);
    }

    #[test]
    fn from_0_1_0_a_target_that_is_not_a_name_is_dropped() {
        let (_tmp, mut tree) = fixture_tree("0.1.0");
        let api = File::Component("api".into());
        let component = tree.files.get_mut(&api).unwrap()["component"]
            .as_table_mut()
            .unwrap();
        component.insert(
            "connects_to".into(),
            Value::Array(vec!["store".into(), 7.into()]),
        );

        apply(&STEPS[0], &mut tree);

        assert_eq!(
            tree.losses,
            [loss(&api, "component.connects_to", &7.into())]
        );
        assert!(edges(&tree).contains(&("api".into(), "store".into(), "connects_to".into())));
    }

    #[test]
    fn from_0_2_0_every_decision_is_the_users() {
        let (_tmp, mut tree) = fixture_tree("0.2.0");

        apply(&STEPS[1], &mut tree);

        let decisions: Vec<&File> = tree
            .files
            .keys()
            .filter(|file| matches!(file, File::Decision(_)))
            .collect();
        assert_eq!(decisions.len(), 5);
        for file in decisions {
            let decision = table(&tree, file, "decision");
            assert_eq!(decision["attribution"].as_str(), Some("user"), "{file}");
        }
        assert!(tree.losses.is_empty());
    }

    #[test]
    fn from_0_3_0_supersedes_edges_are_dropped_and_reported() {
        let (_tmp, mut tree) = fixture_tree("0.3.0");
        let before = edges(&tree);

        apply(&STEPS[2], &mut tree);

        let superseded = (
            "newline-delimited-json-rpc-over-stdio".to_owned(),
            "json-rpc-over-stdio".to_owned(),
            "supersedes".to_owned(),
        );
        let kept: Vec<_> = before.into_iter().filter(|e| *e != superseded).collect();
        assert_eq!(edges(&tree), kept);
        assert_eq!(tree.losses.len(), 1);
        assert_eq!(tree.losses[0].file, "graph.toml");
        assert_eq!(tree.losses[0].field, "edges");
        assert!(tree.losses[0].value.contains("supersedes"));
    }

    #[test]
    fn from_0_4_0_the_version_key_is_renamed() {
        let (_tmp, mut tree) = fixture_tree("0.4.0");

        apply(&STEPS[3], &mut tree);

        assert_eq!(tree.project()["format_version"].as_str(), Some("0.4.0"));
        assert!(!tree.project().contains_key("trurlic_version"));
        assert!(tree.losses.is_empty());
    }

    #[test]
    fn a_renamed_version_key_never_overwrites_the_new_one() {
        let (_tmp, mut tree) = fixture_tree("0.4.0");
        tree.project()
            .insert("format_version".into(), "0.4.0".into());

        apply(&STEPS[3], &mut tree);

        assert_eq!(
            tree.losses,
            [loss(&File::Project, "trurlic_version", &"0.4.0".into())]
        );
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
}
