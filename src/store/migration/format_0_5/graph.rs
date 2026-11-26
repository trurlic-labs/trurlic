//! The graph `graph.toml` and the node files of format 0.4.1 hold: the
//! nodes by kind, and each edge and node entry of the index moved to where
//! format 0.5 holds it or reported.

use std::collections::{BTreeMap, BTreeSet};
use std::mem;

use toml::{Table, Value};

use crate::{Error, Result};

use super::super::super::cycles::cycles;
use super::super::{Fact, File, PatternRecord};
use super::{Notes, component, decision, pattern, project};
use crate::store::model::{PROJECT_COMPONENT, Placement};

/// The nodes of the 0.4.1 graph by kind, and the edges `graph.toml` gives
/// them.
#[derive(Default)]
pub(super) struct Graph {
    /// Each component's `connects_to`.
    components: BTreeMap<String, BTreeSet<String>>,
    /// Each decision's component, as its file states it.
    decisions: BTreeMap<String, Option<String>>,
    patterns: BTreeMap<String, PatternRecord>,
    /// The `depends_on` edges of `graph.toml`, by source.
    depends_on: BTreeMap<String, BTreeSet<String>>,
    /// `B -> {A}` for each `A constrains B`.
    constrained: BTreeMap<String, BTreeSet<String>>,
}

/// The keys of an edge of `graph.toml`.
const EDGE_KEYS: [&str; 3] = ["from", "to", "kind"];

impl Graph {
    fn is_component(&self, name: &str) -> bool {
        name == PROJECT_COMPONENT || self.components.contains_key(name)
    }

    /// `table`, the file `file` of format 0.4.1, as format 0.5 holds it, the
    /// node noted; `None` for a pattern, which 0.5 does not hold.
    pub(super) fn read_file(
        &mut self,
        file: &File,
        table: Table,
        placement: Placement,
        notes: &mut Notes<'_>,
    ) -> Option<Table> {
        match file {
            File::Project => Some(project(table, placement, notes)),
            File::Component(name) => {
                // clone: the graph and the tree each key the node by name.
                self.components.insert(name.clone(), BTreeSet::new());
                Some(component(file, name, table, notes))
            }
            File::Decision(name) => {
                let decision = decision(file, name, table, notes);
                let component = decision.get("component").and_then(Value::as_str);
                // clone: the graph and the tree each key the node by name.
                self.decisions
                    .insert(name.clone(), component.map(str::to_owned));
                Some(decision)
            }
            File::Pattern(name) => {
                let pattern = pattern(file, name, table, notes);
                // clone: the report and the tree each key the pattern by name.
                self.patterns.insert(name.clone(), pattern);
                None
            }
            // Taken out of the tree before the node files are read.
            File::Index => None,
        }
    }

    pub(super) fn read_index(&mut self, mut index: Table, notes: &mut Notes<'_>) {
        // Rebuilt from the node files on every 0.4.1 write.
        index.remove("version");
        index.remove("rebuilt");
        match index.remove("edges") {
            Some(Value::Array(edges)) => {
                for edge in &edges {
                    self.edge(edge, notes);
                }
            }
            Some(other) => notes.loss(&File::Index, "edges", &other),
            None => {}
        }
        match index.remove("nodes") {
            Some(Value::Array(nodes)) => {
                for (at, node) in nodes.into_iter().enumerate() {
                    self.node(at, node, notes);
                }
            }
            Some(other) => notes.loss(&File::Index, "nodes", &other),
            None => {}
        }
        notes.rest_top(&File::Index, index);
    }

    /// One edge of `graph.toml`, moved to where 0.5 holds it, or reported.
    fn edge(&mut self, edge: &Value, notes: &mut Notes<'_>) {
        let field = |key: &str| edge.get(key).and_then(Value::as_str);
        let shaped = edge
            .as_table()
            .is_some_and(|table| table.keys().all(|key| EDGE_KEYS.contains(&key.as_str())));
        let (Some(from), Some(to), Some(kind), true) =
            (field("from"), field("to"), field("kind"), shaped)
        else {
            notes.loss(&File::Index, "edges", edge);
            return;
        };
        let decision = |name: &str| self.decisions.contains_key(name);
        let placed = match kind {
            "belongs_to" => self
                .decisions
                .get(from)
                .is_some_and(|component| component.as_deref() == Some(to)),
            "connects_to" if self.components.contains_key(from) && self.is_component(to) => {
                insert(&mut self.components, from, to)
            }
            "depends_on" if decision(from) && decision(to) => {
                insert(&mut self.depends_on, from, to)
            }
            "constrains" if decision(from) && decision(to) => {
                notes.facts.push(Fact::Reversed {
                    from: from.to_owned(),
                    to: to.to_owned(),
                });
                insert(&mut self.constrained, to, from)
            }
            "member_of" if decision(to) => {
                self.pattern_edge(from, to, |pattern| &mut pattern.member_of)
            }
            "applies_to" if self.is_component(to) => {
                self.pattern_edge(from, to, |pattern| &mut pattern.applies_to)
            }
            // An unknown kind, or one whose ends are not of its node kinds.
            _ => false,
        };
        if !placed {
            notes.loss(&File::Index, "edges", edge);
        }
    }

    /// Add `to` to the list `list` picks of the pattern `from`; `false`
    /// when there is no such pattern.
    fn pattern_edge(
        &mut self,
        from: &str,
        to: &str,
        list: impl FnOnce(&mut PatternRecord) -> &mut Vec<String>,
    ) -> bool {
        self.patterns
            .get_mut(from)
            .map(|pattern| list(pattern).push(to.to_owned()))
            .is_some()
    }

    /// The entry `at` of the `nodes` of `graph.toml`. Its tags are what
    /// 0.4.1 held of a component's or a pattern's; a decision's file holds
    /// its own. An entry for no node is reported.
    fn node(&mut self, at: usize, node: Value, notes: &mut Notes<'_>) {
        let field = format!("nodes[{at}]");
        let Value::Table(mut entry) = node else {
            notes.loss(&File::Index, &field, &node);
            return;
        };
        let name = match entry.get("name").and_then(Value::as_str) {
            Some(name) if self.decisions.contains_key(name) => return,
            Some(name) if self.is_component(name) || self.patterns.contains_key(name) => {
                name.to_owned()
            }
            _ => {
                notes.loss(&File::Index, &field, &Value::Table(entry));
                return;
            }
        };
        let tags = match entry.remove("tags").map(tags) {
            Some(Ok(tags)) => tags,
            Some(Err(other)) => {
                notes.loss(&File::Index, &format!("{field}.tags"), &other);
                Vec::new()
            }
            None => Vec::new(),
        };
        for key in ["name", "kind", "hash"] {
            entry.remove(key);
        }
        notes.rest(&File::Index, &field, entry);
        match self.patterns.get_mut(&name) {
            Some(pattern) => pattern.tags = tags,
            None if !tags.is_empty() => notes.facts.push(Fact::Tags { node: name, tags }),
            None => {}
        }
    }

    /// Refuse a `depends_on` cycle that a reversed `constrains` edge
    /// closes. A cycle of `depends_on` edges alone stays, for `check` to
    /// report.
    pub(super) fn refuse_new_cycles(&self) -> Result<()> {
        if self.constrained.is_empty() {
            return Ok(());
        }
        let merged = merge(&self.depends_on, &self.constrained);
        let after = cycles(merged.keys().map(String::as_str), |name| merged.get(name));
        if after.is_empty() {
            return Ok(());
        }
        let before: BTreeSet<Vec<&str>> =
            cycles(self.depends_on.keys().map(String::as_str), |name| {
                self.depends_on.get(name)
            })
            .into_iter()
            .collect();
        match after.into_iter().find(|cycle| !before.contains(cycle)) {
            Some(members) => Err(Error::ConstrainsCycle {
                members: members.into_iter().map(str::to_owned).collect(),
            }),
            None => Ok(()),
        }
    }

    /// Write each node's edges into its table, and the patterns into the
    /// report.
    pub(super) fn write(mut self, files: &mut BTreeMap<File, Table>, notes: &mut Notes<'_>) {
        for (to, from) in mem::take(&mut self.constrained) {
            self.depends_on.entry(to).or_default().extend(from);
        }
        for (file, table) in files.iter_mut() {
            let (key, targets) = match file {
                File::Component(name) => ("connects_to", self.components.get(name)),
                File::Decision(name) => ("depends_on", self.depends_on.get(name)),
                File::Project | File::Index | File::Pattern(_) => continue,
            };
            if let Some(targets) = targets.filter(|targets| !targets.is_empty()) {
                let list = targets
                    .iter()
                    .map(|target| target.as_str().into())
                    .collect();
                table.insert(key.into(), Value::Array(list));
            }
        }
        for mut pattern in self.patterns.into_values() {
            for list in [&mut pattern.member_of, &mut pattern.applies_to] {
                list.sort_unstable();
                list.dedup();
            }
            notes.facts.push(Fact::Pattern(pattern));
        }
    }
}

/// The tags of a node entry, sorted and each once; the value back when it
/// is no list of strings.
fn tags(value: Value) -> std::result::Result<Vec<String>, Value> {
    let list = match value {
        Value::Array(list) if list.iter().all(Value::is_str) => list,
        other => return Err(other),
    };
    let mut tags: Vec<String> = list
        .into_iter()
        .filter_map(|tag| match tag {
            Value::String(tag) => Some(tag),
            // Every element is a string: checked above.
            _ => None,
        })
        .collect();
    tags.sort_unstable();
    tags.dedup();
    Ok(tags)
}

/// Add `to` to the set of `from` in `edges`; always placed.
fn insert(edges: &mut BTreeMap<String, BTreeSet<String>>, from: &str, to: &str) -> bool {
    edges
        .entry(from.to_owned())
        .or_default()
        .insert(to.to_owned());
    true
}

/// The union of two edge maps.
fn merge(
    left: &BTreeMap<String, BTreeSet<String>>,
    right: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    // clone: the union is a new map; the cycle check compares it with `left`.
    let mut merged = left.clone();
    for (from, targets) in right {
        // clone: as above.
        let union = merged.entry(from.clone()).or_default();
        // clone: as above.
        union.extend(targets.iter().cloned());
    }
    merged
}
