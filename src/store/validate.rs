//! What a loaded graph holds but should not: the relations between its
//! files, the names their stems give, and the text rules of `text.rs`.
//!
//! [`ProjectState::validate`] runs every check and sorts what they find, so
//! the order of the checks never reaches a caller. [`introduced`] is the
//! rule every write commits under: a write may keep an issue the graph
//! already had, never add one.

use std::collections::BTreeSet;
use std::fmt;

use super::schema::{Decision, PROJECT_COMPONENT};
use super::state::{NameFault, NodeFile, ProjectState, name_fault};
use super::text::{TextField, fault};

/// What an [`Issue`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueKind {
    /// A stem that is not kebab-case, or longer than a name may be.
    NameInvalid,
    /// A node named like the project-wide component, which has no file.
    NameReserved,
    /// A component and a decision with one stem.
    NameShared,
    /// A decision whose component does not exist.
    ComponentMissing,
    /// A `connects_to` that names no component.
    ConnectionMissing,
    /// A `depends_on` that names no decision.
    DependencyMissing,
    /// A node that connects to or depends on itself.
    SelfEdge,
    DependsOnCycle,
    /// A choice that breaks a rule of `store::text`.
    InvalidChoice,
    /// A reason that breaks a rule of `store::text`.
    InvalidReason,
    /// `tracked`, and no work tree holds the graph or git tracks none of
    /// its files.
    TrackedUnversioned,
    /// `tracked`, and an ignore rule keeps some of its files out of git.
    TrackedIgnored,
    /// `shared`, and git tracks some of its files.
    SharedVersioned,
    /// `shared`, and the graph is not the one at the top of the main
    /// worktree, where linked worktrees find it.
    SharedOffMain,
    /// A graph in a worktree beside the main worktree's shared graph.
    SecondGraph,
}

impl IssueKind {
    pub const ALL: [Self; 15] = [
        Self::NameInvalid,
        Self::NameReserved,
        Self::NameShared,
        Self::ComponentMissing,
        Self::ConnectionMissing,
        Self::DependencyMissing,
        Self::SelfEdge,
        Self::DependsOnCycle,
        Self::InvalidChoice,
        Self::InvalidReason,
        Self::TrackedUnversioned,
        Self::TrackedIgnored,
        Self::SharedVersioned,
        Self::SharedOffMain,
        Self::SecondGraph,
    ];

    /// The kind as reports name it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NameInvalid => "name_invalid",
            Self::NameReserved => "name_reserved",
            Self::NameShared => "name_shared",
            Self::ComponentMissing => "component_missing",
            Self::ConnectionMissing => "connection_missing",
            Self::DependencyMissing => "dependency_missing",
            Self::SelfEdge => "self_edge",
            Self::DependsOnCycle => "depends_on_cycle",
            Self::InvalidChoice => "invalid_choice",
            Self::InvalidReason => "invalid_reason",
            Self::TrackedUnversioned => "tracked_unversioned",
            Self::TrackedIgnored => "tracked_ignored",
            Self::SharedVersioned => "shared_versioned",
            Self::SharedOffMain => "shared_off_main",
            Self::SecondGraph => "second_graph",
        }
    }
}

/// One thing wrong with a loaded graph, or with where it sits: the
/// placement issues are found from git's answers by
/// [`ProjectState::placement_issues`], every other kind by
/// [`ProjectState::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    /// What the issue is about: a node name, an edge as `from -> to (kind)`,
    /// or a cycle's members, sorted and comma-separated. Stable across
    /// validations of the same graph, unlike `message`.
    pub subject: String,
    /// The file at fault, then what is wrong with it.
    pub message: String,
}

impl Issue {
    /// Whether the issue is about the node `name`: the node itself, either
    /// end of an edge, or a member of a cycle. Read from the subject, so it
    /// is exact for a kebab-case `name`.
    pub(super) fn concerns(&self, name: &str) -> bool {
        let subject = self.subject.as_str();
        match self.kind {
            IssueKind::NameInvalid
            | IssueKind::NameReserved
            | IssueKind::NameShared
            | IssueKind::ComponentMissing
            | IssueKind::InvalidChoice
            | IssueKind::InvalidReason => subject == name,
            IssueKind::ConnectionMissing | IssueKind::DependencyMissing | IssueKind::SelfEdge => {
                let from = subject
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with(" -> "));
                let to = [EdgeField::ConnectsTo, EdgeField::DependsOn]
                    .iter()
                    .any(|field| subject.ends_with(&format!(" -> {name} ({field})")));
                from || to
            }
            IssueKind::DependsOnCycle => subject.split(", ").any(|member| member == name),
            // Placement issues are about the graph directory, not a node.
            IssueKind::TrackedUnversioned
            | IssueKind::TrackedIgnored
            | IssueKind::SharedVersioned
            | IssueKind::SharedOffMain
            | IssueKind::SecondGraph => false,
        }
    }
}

/// The two edge fields, as an edge subject names them.
#[derive(Debug, Clone, Copy)]
enum EdgeField {
    ConnectsTo,
    DependsOn,
}

impl fmt::Display for EdgeField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ConnectsTo => "connects_to",
            Self::DependsOn => "depends_on",
        })
    }
}

impl ProjectState {
    /// Every issue in the graph, sorted by kind, subject and message. Empty
    /// when the graph is valid.
    #[must_use]
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        self.check_names(&mut issues);
        self.check_components(&mut issues);
        self.check_decisions(&mut issues);
        self.check_cycles(&mut issues);
        sort(&mut issues);
        issues
    }

    /// Stems that are no name, the reserved name, and a stem two kinds share.
    fn check_names(&self, issues: &mut Vec<Issue>) {
        let components = self
            .components
            .keys()
            .map(|name| (name, component_file as FileOf));
        let decisions = self
            .decisions
            .keys()
            .map(|name| (name, decision_file as FileOf));
        for (name, file) in components.chain(decisions) {
            if let Some(fault) = name_fault(name) {
                let kind = match fault {
                    NameFault::NotKebab | NameFault::TooLong { .. } => IssueKind::NameInvalid,
                    NameFault::Reserved => IssueKind::NameReserved,
                };
                let message = format!("{}: the name `{name}` {fault}", file(name));
                issues.push(issue(kind, name, message));
            }
        }
        for name in self.components.keys() {
            if self.decisions.contains_key(name) {
                issues.push(issue(
                    IssueKind::NameShared,
                    name,
                    format!(
                        "{} and {} share the name `{name}`; a name is unique across node kinds",
                        component_file(name),
                        decision_file(name)
                    ),
                ));
            }
        }
    }

    /// Each `connects_to` names a component, and not its own.
    fn check_components(&self, issues: &mut Vec<Issue>) {
        for (name, component) in &self.components {
            for target in &component.connects_to {
                let exists = target == PROJECT_COMPONENT || self.components.contains_key(target);
                check_edge(issues, name, EdgeField::ConnectsTo, target, exists);
            }
        }
    }

    /// Each decision's component exists, each `depends_on` names a
    /// decision, not its own, and its choice and reason meet `store::text`.
    fn check_decisions(&self, issues: &mut Vec<Issue>) {
        for (name, decision) in &self.decisions {
            let component = &decision.component;
            if component != PROJECT_COMPONENT && !self.components.contains_key(component) {
                issues.push(issue(
                    IssueKind::ComponentMissing,
                    name,
                    format!(
                        "{}: component `{component}` does not exist",
                        decision_file(name)
                    ),
                ));
            }
            for target in &decision.depends_on {
                let exists = self.decisions.contains_key(target);
                check_edge(issues, name, EdgeField::DependsOn, target, exists);
            }
            check_text(issues, name, decision);
        }
    }

    fn check_cycles(&self, issues: &mut Vec<Issue>) {
        for members in self.depends_on_cycles() {
            let listed: Vec<String> = members.iter().map(|name| format!("`{name}`")).collect();
            issues.push(issue(
                IssueKind::DependsOnCycle,
                &members.join(", "),
                format!("depends_on cycle among {}", listed.join(", ")),
            ));
        }
    }
}

/// The edge `from -> to` of `field`; `exists` is whether `to` is a node
/// the field may name.
fn check_edge(issues: &mut Vec<Issue>, from: &str, field: EdgeField, to: &str, exists: bool) {
    let file = match field {
        EdgeField::ConnectsTo => component_file(from),
        EdgeField::DependsOn => decision_file(from),
    };
    let subject = format!("{from} -> {to} ({field})");
    if from == to {
        issues.push(issue(
            IssueKind::SelfEdge,
            &subject,
            format!("{file}: `{field}` names the node itself"),
        ));
    } else if !exists {
        let (kind, node) = match field {
            EdgeField::ConnectsTo => (IssueKind::ConnectionMissing, "component"),
            EdgeField::DependsOn => (IssueKind::DependencyMissing, "decision"),
        };
        issues.push(issue(
            kind,
            &subject,
            format!("{file}: `{field}` names `{to}`, which is no {node}"),
        ));
    }
}

/// The choice and the reason of the decision `name` against the rules every
/// surface holds them to.
fn check_text(issues: &mut Vec<Issue>, name: &str, decision: &Decision) {
    for (field, text, kind) in [
        (
            TextField::Choice,
            &decision.choice,
            IssueKind::InvalidChoice,
        ),
        (
            TextField::Reason,
            &decision.reason,
            IssueKind::InvalidReason,
        ),
    ] {
        if let Some(fault) = fault(field, text) {
            issues.push(issue(
                kind,
                name,
                format!("{}: `{field}` {fault}", decision_file(name)),
            ));
        }
    }
}

/// The file of a node of one kind, by its name; built only for a message.
type FileOf = fn(&str) -> NodeFile;

fn component_file(name: &str) -> NodeFile {
    NodeFile::Component(name.to_owned())
}

fn decision_file(name: &str) -> NodeFile {
    NodeFile::Decision(name.to_owned())
}

/// The issues of `after` that `before` does not have, matched on kind and
/// subject: a message may change while the issue stays the same. This is
/// the rule every write commits under: it may keep an issue the graph had,
/// never add one.
pub(super) fn introduced(mut after: Vec<Issue>, before: &[Issue]) -> Vec<Issue> {
    let known: BTreeSet<(IssueKind, &str)> = before
        .iter()
        .map(|issue| (issue.kind, issue.subject.as_str()))
        .collect();
    after.retain(|issue| !known.contains(&(issue.kind, issue.subject.as_str())));
    after
}

/// `issues` in the order every report gives them: by kind, subject and
/// message, so the order of the checks never reaches a caller.
pub(super) fn sort(issues: &mut [Issue]) {
    issues.sort_unstable_by(|a, b| {
        (a.kind, &a.subject, &a.message).cmp(&(b.kind, &b.subject, &b.message))
    });
}

pub(super) fn issue(kind: IssueKind, subject: &str, message: String) -> Issue {
    Issue {
        kind,
        subject: subject.to_owned(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{depends_on_state, empty_state, sample_component, sample_decision};

    fn kinds(state: &ProjectState) -> Vec<(IssueKind, String)> {
        state
            .validate()
            .into_iter()
            .map(|issue| (issue.kind, issue.subject))
            .collect()
    }

    fn with_component(state: &mut ProjectState, name: &str, connects_to: &[&str]) {
        let mut component = sample_component();
        component.connects_to = connects_to.iter().map(|&to| to.to_owned()).collect();
        state.components.insert(name.into(), component);
    }

    #[test]
    fn a_graph_whose_edges_all_land_is_valid() {
        let mut state = depends_on_state(&[("a", "b"), ("b", "c")]);
        with_component(&mut state, "auth", &["store", PROJECT_COMPONENT]);
        with_component(&mut state, "store", &[]);
        state.decisions.insert("d".into(), sample_decision("auth"));
        assert_eq!(state.validate(), []);
    }

    #[test]
    fn each_edge_to_no_node_is_one_issue_naming_its_file() {
        let mut state = depends_on_state(&[("a", "gone")]);
        state.decisions.remove("gone");
        with_component(&mut state, "auth", &["nowhere"]);
        state
            .decisions
            .insert("d".into(), sample_decision("missing"));

        let issues = state.validate();

        assert_eq!(
            kinds(&state),
            [
                (IssueKind::ComponentMissing, "d".to_owned()),
                (
                    IssueKind::ConnectionMissing,
                    "auth -> nowhere (connects_to)".to_owned()
                ),
                (
                    IssueKind::DependencyMissing,
                    "a -> gone (depends_on)".to_owned()
                ),
            ]
        );
        assert_eq!(
            issues[0].message,
            "decisions/d.toml: component `missing` does not exist"
        );
        assert_eq!(
            issues[1].message,
            "components/auth.toml: `connects_to` names `nowhere`, which is no component"
        );
    }

    #[test]
    fn a_connection_to_a_decision_and_a_dependency_on_a_component_dangle() {
        let mut state = depends_on_state(&[("a", "b")]);
        with_component(&mut state, "auth", &["a"]);
        state
            .decisions
            .get_mut("a")
            .unwrap()
            .depends_on
            .insert("auth".into());
        assert_eq!(
            kinds(&state),
            [
                (
                    IssueKind::ConnectionMissing,
                    "auth -> a (connects_to)".to_owned()
                ),
                (
                    IssueKind::DependencyMissing,
                    "a -> auth (depends_on)".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_self_edge_is_reported_and_not_as_missing() {
        let mut state = depends_on_state(&[("a", "a")]);
        with_component(&mut state, "auth", &["auth"]);
        assert_eq!(
            kinds(&state),
            [
                (IssueKind::SelfEdge, "a -> a (depends_on)".to_owned()),
                (IssueKind::SelfEdge, "auth -> auth (connects_to)".to_owned()),
            ]
        );
    }

    #[test]
    fn a_name_two_kinds_share_is_one_issue() {
        let mut state = empty_state();
        with_component(&mut state, "auth", &[]);
        state
            .decisions
            .insert("auth".into(), sample_decision("auth"));
        let issues = state.validate();
        assert_eq!(kinds(&state), [(IssueKind::NameShared, "auth".to_owned())]);
        assert!(
            issues[0]
                .message
                .contains("components/auth.toml and decisions/auth.toml")
        );
    }

    #[test]
    fn invalid_and_reserved_names_are_reported_per_file() {
        let mut state = empty_state();
        with_component(&mut state, "Bad_Name", &[]);
        with_component(&mut state, PROJECT_COMPONENT, &[]);
        state
            .decisions
            .insert("Also Bad".into(), sample_decision("project"));
        assert_eq!(
            kinds(&state),
            [
                (IssueKind::NameInvalid, "Also Bad".to_owned()),
                (IssueKind::NameInvalid, "Bad_Name".to_owned()),
                (IssueKind::NameReserved, PROJECT_COMPONENT.to_owned()),
            ]
        );
    }

    #[test]
    fn a_choice_or_reason_that_breaks_a_text_rule_is_reported() {
        let mut state = empty_state();
        let mut decision = sample_decision("project");
        decision.choice = "Two\nlines".into();
        decision.reason = "short".into();
        state.decisions.insert("d".into(), decision);
        let issues = state.validate();
        assert_eq!(
            kinds(&state),
            [
                (IssueKind::InvalidChoice, "d".to_owned()),
                (IssueKind::InvalidReason, "d".to_owned()),
            ]
        );
        assert_eq!(
            issues[0].message,
            "decisions/d.toml: `choice` must be a single line, but breaks at byte 3"
        );
    }

    fn cycle(members: &[&str]) -> Issue {
        let listed: Vec<String> = members.iter().map(|name| format!("`{name}`")).collect();
        Issue {
            kind: IssueKind::DependsOnCycle,
            subject: members.join(", "),
            message: format!("depends_on cycle among {}", listed.join(", ")),
        }
    }

    #[test]
    fn a_cycle_with_a_tail_is_one_issue_naming_exactly_its_members() {
        let state = depends_on_state(&[("tail", "a"), ("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(state.validate(), [cycle(&["a", "b", "c"])]);
    }

    /// A depth-first search that returns at the first cycle it meets leaves
    /// `r` marked as on its path, and later reports `s` as a cycle.
    #[test]
    fn nodes_upstream_of_a_cycle_raise_no_issue() {
        let state = depends_on_state(&[("r", "a"), ("a", "b"), ("b", "a"), ("s", "r")]);
        assert_eq!(state.validate(), [cycle(&["a", "b"])]);
    }

    #[test]
    fn issues_are_sorted_whatever_the_input_order() {
        let mut edges = vec![
            ("a", "b"),
            ("b", "a"),
            ("x", "y"),
            ("y", "x"),
            ("m", "m"),
            ("n", "n"),
        ];
        let forward = depends_on_state(&edges).validate();
        edges.reverse();
        let backward = depends_on_state(&edges).validate();

        assert_eq!(forward, backward);
        let found: Vec<IssueKind> = forward.iter().map(|i| i.kind).collect();
        assert_eq!(
            found,
            [
                IssueKind::SelfEdge,
                IssueKind::SelfEdge,
                IssueKind::DependsOnCycle,
                IssueKind::DependsOnCycle,
            ]
        );
    }

    #[test]
    fn a_cycle_the_graph_had_is_not_introduced() {
        let before = depends_on_state(&[("a", "b"), ("b", "a")]);
        let after = depends_on_state(&[("a", "b"), ("b", "a"), ("c", "a")]);
        assert_eq!(introduced(after.validate(), &before.validate()), []);
    }

    #[test]
    fn a_second_cycle_is_introduced() {
        let before = depends_on_state(&[("a", "b"), ("b", "a")]);
        let after = depends_on_state(&[("a", "b"), ("b", "a"), ("c", "d"), ("d", "c")]);
        assert_eq!(
            introduced(after.validate(), &before.validate()),
            [cycle(&["c", "d"])]
        );
    }

    #[test]
    fn a_cycle_that_gains_a_member_is_introduced() {
        let before = depends_on_state(&[("a", "b"), ("b", "a")]);
        let after = depends_on_state(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "a")]);
        assert_eq!(
            introduced(after.validate(), &before.validate()),
            [cycle(&["a", "b", "c"])]
        );
    }

    #[test]
    fn an_issue_concerns_its_node_both_ends_of_its_edge_and_its_members() {
        let mut state = depends_on_state(&[("a", "gone"), ("b", "c"), ("c", "b")]);
        state.decisions.remove("gone");
        with_component(&mut state, "auth", &["nowhere"]);
        let issues = state.validate();
        let concerned = |name: &str| -> Vec<IssueKind> {
            issues
                .iter()
                .filter(|issue| issue.concerns(name))
                .map(|issue| issue.kind)
                .collect()
        };
        assert_eq!(concerned("a"), [IssueKind::DependencyMissing]);
        assert_eq!(concerned("gone"), [IssueKind::DependencyMissing]);
        assert_eq!(concerned("nowhere"), [IssueKind::ConnectionMissing]);
        assert_eq!(concerned("b"), [IssueKind::DependsOnCycle]);
        assert_eq!(concerned("go"), []);
    }

    /// The fault's byte offset is in the message, not in the subject.
    #[test]
    fn an_issue_whose_message_changes_is_not_introduced() {
        let with_choice = |choice: &str| {
            let mut state = empty_state();
            let mut decision = sample_decision("project");
            decision.choice = choice.into();
            state.decisions.insert("d".into(), decision);
            state
        };
        let before = with_choice("One\nbreak");
        let after = with_choice("One more\nbreak");
        assert_ne!(before.validate(), after.validate());
        assert_eq!(introduced(after.validate(), &before.validate()), []);
    }
}
