//! The graph a batch changes, in place, with the value each node it
//! touched had when the batch began: what undoes a refused batch, what the
//! commit writes, and the graph "no new errors" compares with.

use std::collections::{BTreeMap, BTreeSet};
use std::mem;

use crate::store::cycles::cycles;
use crate::store::schema::{Component, Decision};
use crate::store::state::ProjectState;
use crate::store::validate::{Issue, introduced};

/// The writer's graph while a batch applies to it.
pub(super) struct Edit<'s> {
    state: &'s mut ProjectState,
    before: Before,
    /// The index of the op running now.
    op: usize,
    /// The op that last changed each node, by name.
    last_op: BTreeMap<String, usize>,
    /// Each node of the starting graph the batch renamed or merged into
    /// another, by its starting name, and the name it has now.
    carried: BTreeMap<String, String>,
    /// The cycles of the graph the batch began with, by sorted members;
    /// found once, when an op first leaves a node in a cycle.
    start_cycles: Option<BTreeSet<Vec<String>>>,
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
    /// into it. Called once the op has touched `from`, so `before` tells
    /// whether it is a node of the starting graph or one the batch made.
    pub(super) fn carry(&mut self, from: &str, to: &str) {
        for name in self.carried.values_mut() {
            if name == from {
                to.clone_into(name);
            }
        }
        let started = |held: Option<bool>| held == Some(true);
        let in_start = started(self.before.decisions.get(from).map(Option::is_some))
            || started(self.before.components.get(from).map(Option::is_some));
        if in_start {
            self.carried.insert(from.to_owned(), to.to_owned());
        }
    }

    /// The members of the `depends_on` cycle through `pivot`, when the
    /// graph has one there that the graph the batch began with lacks:
    /// the cycle an op closed by adding edges at `pivot`.
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
        let new = !start.contains(&members);
        self.start_cycles = Some(start);
        new.then_some(members)
    }

    fn cycles_at_start(&self) -> BTreeSet<Vec<String>> {
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
