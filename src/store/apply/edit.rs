//! The graph a batch changes, in place, with the value each node it
//! touched had when the batch began: what undoes a refused batch, what the
//! commit writes, and the graph "no new errors" compares with.

use std::collections::{BTreeMap, BTreeSet};
use std::mem;

use crate::store::graph::cycles;
use crate::store::graph::{Issue, carry_members, introduced};
use crate::store::model::ProjectState;
use crate::store::model::{Component, Decision};

/// The writer's graph while a batch applies to it.
pub(super) struct Edit<'s> {
    state: &'s mut ProjectState,
    before: Before,
    /// The index of the op running now.
    op: usize,
    /// The op that last changed each node, by name.
    last_op: BTreeMap<String, usize>,
    /// Each node of the starting graph the batch renamed or merged into
    /// another and did not remove, by its starting name, and the name it
    /// has now.
    carried: BTreeMap<String, String>,
    /// The starting names whose starting node has left them: renamed,
    /// merged or removed.
    left: BTreeSet<String>,
    /// The cycles of the graph the batch began with, by sorted members;
    /// found once, when an op first leaves a node in a cycle.
    start_cycles: Option<Vec<Vec<String>>>,
}

/// The value each touched node had when the batch began, `None` for a
/// node the batch made.
#[derive(Debug, Default)]
pub(super) struct Before {
    pub(super) decisions: BTreeMap<String, Option<Decision>>,
    pub(super) components: BTreeMap<String, Option<Component>>,
}

impl<'s> Edit<'s> {
    pub(super) fn new(state: &'s mut ProjectState) -> Self {
        Self {
            state,
            before: Before::default(),
            op: 0,
            last_op: BTreeMap::new(),
            carried: BTreeMap::new(),
            left: BTreeSet::new(),
            start_cycles: None,
        }
    }

    /// Run the next ops as the op at `index`.
    pub(super) const fn begin(&mut self, index: usize) {
        self.op = index;
    }

    /// The graph as the ops so far left it.
    pub(super) fn state(&self) -> &ProjectState {
        self.state
    }

    pub(super) fn decision_mut(&mut self, name: &str) -> Option<&mut Decision> {
        self.touch_decision(name);
        self.state.decisions.get_mut(name)
    }

    pub(super) fn put_decision(&mut self, name: String, decision: Decision) {
        self.touch_decision(&name);
        self.state.decisions.insert(name, decision);
    }

    pub(super) fn take_decision(&mut self, name: &str) -> Option<Decision> {
        self.touch_decision(name);
        self.state.decisions.remove(name)
    }

    pub(super) fn component_mut(&mut self, name: &str) -> Option<&mut Component> {
        self.touch_component(name);
        self.state.components.get_mut(name)
    }

    pub(super) fn put_component(&mut self, name: String, component: Component) {
        self.touch_component(&name);
        self.state.components.insert(name, component);
    }

    pub(super) fn take_component(&mut self, name: &str) -> Option<Component> {
        self.touch_component(name);
        self.state.components.remove(name)
    }

    fn touch_decision(&mut self, name: &str) {
        if !self.before.decisions.contains_key(name) {
            // clone: the batch's first change of a node keeps its old value.
            let old = self.state.decisions.get(name).cloned();
            self.before.decisions.insert(name.to_owned(), old);
        }
        self.changed_by_this_op(name);
    }

    fn touch_component(&mut self, name: &str) {
        if !self.before.components.contains_key(name) {
            // clone: the batch's first change of a node keeps its old value.
            let old = self.state.components.get(name).cloned();
            self.before.components.insert(name.to_owned(), old);
        }
        self.changed_by_this_op(name);
    }

    fn changed_by_this_op(&mut self, name: &str) {
        match self.last_op.get_mut(name) {
            Some(op) => *op = self.op,
            None => {
                self.last_op.insert(name.to_owned(), self.op);
            }
        }
    }

    /// Record that the node `from` goes on as `to`: renamed, or merged
    /// into it. Called once the op has taken `from`.
    pub(super) fn carry(&mut self, from: &str, to: &str) {
        for name in self.carried.values_mut() {
            if name == from {
                to.clone_into(name);
            }
        }
        if self.leaves_start(from) {
            self.carried.insert(from.to_owned(), to.to_owned());
        }
    }

    /// Record that the node `name` goes on as no node: removed. Called
    /// once the op has taken it.
    pub(super) fn end(&mut self, name: &str) {
        self.carried.retain(|_, now| now != name);
        self.leaves_start(name);
    }

    /// Record that the node at `name` leaves it; whether that node is the
    /// one `name` held when the batch began. A node the batch made under a
    /// name a starting node left is another node.
    fn leaves_start(&mut self, name: &str) -> bool {
        let in_start = matches!(self.before.decisions.get(name), Some(Some(_)))
            || matches!(self.before.components.get(name), Some(Some(_)));
        in_start && self.left.insert(name.to_owned())
    }

    /// The members of the `depends_on` cycle through `pivot`, when the
    /// graph has one there that the graph the batch began with lacks under
    /// the names the batch carried its nodes to: the cycle an op closed by
    /// adding edges at `pivot`.
    pub(super) fn new_cycle(&mut self, pivot: &str) -> Option<Vec<String>> {
        let decisions = &self.state.decisions;
        let found = cycles([pivot], |name| {
            decisions.get(name).map(|decision| &decision.depends_on)
        });
        let members: Vec<String> = found
            .into_iter()
            .find(|members| members.contains(&pivot))?
            .into_iter()
            .map(str::to_owned)
            .collect();
        let start = match self.start_cycles.take() {
            Some(start) => start,
            None => self.cycles_at_start(),
        };
        let carried = &self.carried;
        let new = !start
            .iter()
            .any(|cycle| carry_members(cycle.iter().map(String::as_str), carried) == members);
        self.start_cycles = Some(start);
        new.then_some(members)
    }

    fn cycles_at_start(&self) -> Vec<Vec<String>> {
        let (before, now) = (&self.before.decisions, &self.state.decisions);
        let at_start = |name: &str| match before.get(name) {
            Some(old) => old.as_ref(),
            None => now.get(name),
        };
        let roots = now.keys().chain(before.keys()).map(String::as_str);
        cycles(roots, |name| {
            at_start(name).map(|decision| &decision.depends_on)
        })
        .into_iter()
        .map(|members| members.into_iter().map(str::to_owned).collect())
        .collect()
    }

    /// The first issue the graph has that the graph the batch began with
    /// lacks, matched on kind and subject, with the op that last changed a
    /// node it concerns. The graph is validated once, and the graph the
    /// batch began with only when it has an issue.
    pub(super) fn first_introduced(&mut self) -> Option<(usize, Issue)> {
        let after = self.state.validate();
        if after.is_empty() {
            return None;
        }
        self.before.swap(self.state);
        let at_start = self.state.validate();
        self.before.swap(self.state);
        let issue = introduced(after, &at_start, &self.carried)
            .into_iter()
            .next()?;
        let op = self
            .last_op
            .iter()
            .filter(|(name, _)| issue.concerns(name))
            .map(|(_, &op)| op)
            .max()
            .unwrap_or(self.op);
        Some((op, issue))
    }

    /// Put back every node the batch touched.
    pub(super) fn undo(self) {
        self.before.restore(self.state);
    }

    /// End the batch, keeping what it changed and the op that last
    /// changed each node.
    pub(super) fn finish(self) -> (Before, BTreeMap<String, usize>) {
        (self.before, self.last_op)
    }
}

impl Before {
    /// Put the batch's starting values back into `state`.
    pub(super) fn restore(mut self, state: &mut ProjectState) {
        self.swap(state);
    }

    /// Exchange the values held here with the ones in `state`: twice is
    /// no change.
    fn swap(&mut self, state: &mut ProjectState) {
        swap_nodes(&mut self.decisions, &mut state.decisions);
        swap_nodes(&mut self.components, &mut state.components);
    }
}

fn swap_nodes<T>(held: &mut BTreeMap<String, Option<T>>, nodes: &mut BTreeMap<String, T>) {
    for (name, value) in held {
        let current = nodes.remove(name);
        if let Some(old) = mem::replace(value, current) {
            // clone: the map takes its own key back.
            nodes.insert(name.clone(), old);
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::store::apply::*;
    use crate::store::model::NodeFile;
    use crate::store::testing::{
        add, apply, decision, graph, record, refused, rename, revise, sample_component, write_node,
    };
    use crate::store::{IssueKind, Store};

    #[test]
    fn an_edge_that_closes_a_cycle_is_refused_naming_its_members() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        let mut op = revise("sign");
        op.depends_on = add(&["audit"]);

        let refusal = refused(&store, vec![Op::Revise(op)]);

        assert_eq!(
            refusal.rule,
            Rule::Cycle {
                members: ["audit", "rotate", "sign"].map(String::from).into()
            }
        );
        assert_eq!(refusal.path(), "ops[0].depends_on");
        assert!(
            refusal
                .to_string()
                .ends_with("closes a depends_on cycle among `audit`, `rotate`, `sign`"),
            "{refusal}"
        );
    }

    /// `audit -> rotate -> sign -> audit`, a cycle a hand edit left.
    fn with_cycle(store: &Store) {
        let mut sign = decision(store, "sign");
        sign.depends_on.insert("audit".into());
        write_node(store, &NodeFile::Decision("sign".into()), &sign);
    }

    /// A cycle a hand edit left is no new issue: an edge inside it does not
    /// change its members, and a write elsewhere lands beside it.
    #[test]
    fn a_cycle_the_graph_began_with_blocks_no_write() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_cycle(&store);
        let mut inside = revise("sign");
        inside.depends_on = add(&["rotate"]);

        apply(
            &store,
            vec![Op::Revise(inside), Op::Record(record("new", "auth"))],
        )
        .unwrap();

        let state = store.load_state().unwrap();
        let kinds: Vec<IssueKind> = state.validate().iter().map(|issue| issue.kind).collect();
        assert_eq!(kinds, [IssueKind::DependsOnCycle]);
    }

    /// `sign` with a dangling `depends_on` and a reason below the text rule:
    /// two issues a hand edit left.
    fn with_issues(store: &Store) {
        let mut sign = decision(store, "sign");
        sign.depends_on.insert("gone".into());
        sign.reason = "Too short".into();
        write_node(store, &NodeFile::Decision("sign".into()), &sign);
    }

    /// An issue a node had is not new when the batch's own rename or merge
    /// carries it to the node's new name, renamed twice included.
    #[test]
    fn a_rename_or_a_merge_carries_the_issues_a_node_had() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_issues(&store);
        let issues = |store: &Store| -> Vec<(IssueKind, String)> {
            let state = store.load_state().unwrap();
            let issues = state.validate().into_iter();
            issues.map(|issue| (issue.kind, issue.subject)).collect()
        };

        apply(
            &store,
            vec![
                Op::Rename(rename("sign", "signing")),
                Op::Rename(rename("signing", "sign-tokens")),
            ],
        )
        .unwrap();
        let merge = Merge {
            survivor: "rotate".into(),
            absorbed: "sign-tokens".into(),
        };
        apply(&store, vec![Op::Merge(merge)]).unwrap();

        assert_eq!(
            issues(&store),
            [(
                IssueKind::DependencyMissing,
                "rotate -> gone (depends_on)".to_owned()
            )]
        );
    }

    /// A rename carries the node's issues, not the license to add one: `sign`
    /// depends on a `sign-tokens` that does not exist, and renaming it to that
    /// name makes the edge a self-edge, which is new.
    #[test]
    fn a_new_issue_under_a_rename_is_refused_naming_its_op() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_issues(&store);
        let mut sign = decision(&store, "sign");
        sign.depends_on.insert("sign-tokens".into());
        write_node(&store, &NodeFile::Decision("sign".into()), &sign);
        let ops = vec![
            Op::Record(record("new", "auth")),
            Op::Rename(rename("sign", "sign-tokens")),
        ];

        let refusal = refused(&store, ops);

        assert_eq!(refusal.op.as_ref().unwrap().index, 1);
        assert_eq!(refusal.rule.as_str(), "self_edge");
        assert!(
            refusal
                .to_string()
                .contains("the graph would gain an issue: decisions/sign-tokens.toml"),
            "{refusal}"
        );
        let mut unrelated = revise("rotate");
        unrelated.tags = add(&["keys"]);
        apply(&store, vec![Op::Revise(unrelated)]).unwrap();
    }

    /// A node the batch made under a starting node's old name is another
    /// node: renaming it moves nothing the starting node had.
    #[test]
    fn a_rename_of_a_node_made_under_an_old_name_carries_no_issue() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_issues(&store);

        apply(
            &store,
            vec![
                Op::Rename(rename("sign", "signing")),
                Op::Record(record("sign", "auth")),
                Op::Rename(rename("sign", "sign-tokens")),
            ],
        )
        .unwrap();
    }

    /// The cycle a graph began with is the one its members form under the
    /// names the batch's renames and merges gave them: an edge added at a
    /// member after another is renamed, and a merge of two members, close no
    /// new cycle.
    #[test]
    fn a_cycle_the_graph_began_with_is_known_under_its_members_new_names() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_cycle(&store);
        let mut rotate = revise("rotate");
        rotate.depends_on = add(&["expire"]);

        apply(
            &store,
            vec![
                Op::Record(record("expire", "auth")),
                Op::Rename(rename("sign", "signing")),
                Op::Revise(rotate),
            ],
        )
        .unwrap();
        let merge = Merge {
            survivor: "audit".into(),
            absorbed: "rotate".into(),
        };
        apply(&store, vec![Op::Merge(merge)]).unwrap();

        let state = store.load_state().unwrap();
        let issues: Vec<(IssueKind, String)> = state
            .validate()
            .into_iter()
            .map(|issue| (issue.kind, issue.subject))
            .collect();
        assert_eq!(
            issues,
            [(IssueKind::DependsOnCycle, "audit, signing".to_owned())]
        );
    }

    /// A removed node goes on as no node: one the batch makes under its name
    /// and renames is new, and so is the cycle it closes with the removed
    /// node's old cycle mates.
    #[test]
    fn a_cycle_through_a_node_made_under_a_removed_members_name_is_new() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        with_cycle(&store);
        let mut again = record("sign", "auth");
        again.depends_on = vec!["audit".into()];
        let mut rotate = revise("rotate");
        rotate.depends_on = add(&["signing"]);
        let ops = vec![
            Op::Remove(Remove {
                name: "sign".into(),
                repoint_to: Some("rotate".into()),
            }),
            Op::Record(again),
            Op::Rename(rename("sign", "signing")),
            Op::Revise(rotate),
        ];

        let refusal = refused(&store, ops);

        assert_eq!(refusal.op.as_ref().unwrap().index, 3);
        assert_eq!(
            refusal.rule,
            Rule::Cycle {
                members: ["audit", "rotate", "signing"].map(String::from).into()
            }
        );
    }

    /// An edge that names a node of the other kind names no node of its own,
    /// whatever that node is renamed to: a `connects_to` naming a decision and
    /// a `depends_on` naming a component keep their issues through a rename of
    /// that decision and that component.
    #[test]
    fn a_rename_leaves_an_edge_that_names_a_node_of_the_other_kind_as_it_was() {
        let tmp = TempDir::new().unwrap();
        let store = graph(tmp.path());
        let mut billing = sample_component();
        billing.connects_to.insert("sign".into());
        write_node(&store, &NodeFile::Component("billing".into()), &billing);
        let mut audit = decision(&store, "audit");
        audit.depends_on.insert("auth".into());
        write_node(&store, &NodeFile::Decision("audit".into()), &audit);

        apply(
            &store,
            vec![
                Op::Rename(rename("sign", "signing")),
                Op::RenameComponent(rename("auth", "identity")),
            ],
        )
        .unwrap();

        let state = store.load_state().unwrap();
        let subjects: Vec<String> = state
            .validate()
            .into_iter()
            .map(|issue| issue.subject)
            .collect();
        assert_eq!(
            subjects,
            [
                "billing -> sign (connects_to)",
                "audit -> auth (depends_on)"
            ]
        );
    }
}
