//! Graph integrity checks for [`InMemoryGraph`].
//!
//! [`InMemoryGraph::validate`] runs every check and sorts what they find, so
//! neither the order of the checks nor the iteration order of the graph's
//! hash maps reaches a caller. [`InMemoryGraph::introduced_errors`] is the
//! rule every write commits under: a write may keep an error the graph
//! already had, never add one.

use std::collections::BTreeSet;

use super::graph::{Edge, InMemoryGraph, Issue, IssueKind, Severity};
use super::schema::{EdgeKind, NodeKind};
use super::state::is_valid_kebab_case;

impl InMemoryGraph {
    /// Every issue in the graph, sorted by severity, kind, subject and
    /// message. Empty when the graph is valid.
    #[must_use]
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        self.check_edge_endpoints(&mut issues);
        self.check_edge_type_constraints(&mut issues);
        self.check_self_edges(&mut issues);
        self.check_duplicate_edges(&mut issues);
        self.check_pattern_membership(&mut issues);
        self.check_depends_on_cycles(&mut issues);
        self.check_belongs_to_integrity(&mut issues);
        self.check_content_integrity(&mut issues);
        self.check_name_integrity(&mut issues);
        self.check_node_content_coherence(&mut issues);
        issues.sort_unstable_by(|a, b| order(a).cmp(&order(b)));
        issues
    }

    /// The errors of this graph that `before` does not have, matched on kind
    /// and subject: a message may change with a count while the error stays
    /// the same. `before` is validated only when this graph has an error.
    #[must_use]
    pub(super) fn introduced_errors(&self, before: &Self) -> Vec<Issue> {
        let mut errors = self.validate();
        errors.retain(|issue| issue.severity() == Severity::Error);
        if errors.is_empty() {
            return errors;
        }
        let before_issues = before.validate();
        let known: BTreeSet<(IssueKind, &str)> = before_issues
            .iter()
            .filter(|issue| issue.severity() == Severity::Error)
            .map(|issue| (issue.kind, issue.subject.as_str()))
            .collect();
        errors.retain(|issue| !known.contains(&(issue.kind, issue.subject.as_str())));
        errors
    }

    // ── Validation checks ────────────────────────────────────────────────

    /// Every edge endpoint exists in nodes.
    fn check_edge_endpoints(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            if !self.nodes.contains_key(from) {
                issues.push(Issue {
                    kind: IssueKind::EdgeSourceMissing,
                    subject: from.to_string(),
                    message: format!("edge source `{from}` is not a known node"),
                });
            }
            for edge in edge_list {
                if !self.nodes.contains_key(&edge.target) {
                    issues.push(Issue {
                        kind: IssueKind::EdgeTargetMissing,
                        subject: edge_subject(from, edge),
                        message: format!(
                            "edge target `{}` (from `{from}`, {}) is not a known node",
                            edge.target,
                            edge.kind.as_str()
                        ),
                    });
                }
            }
        }
    }

    /// Each edge kind joins the node kinds it is defined for.
    fn check_edge_type_constraints(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            let from_kind = self.nodes.get(from).map(|m| m.kind);
            for edge in edge_list {
                let to_kind = self.nodes.get(&edge.target).map(|m| m.kind);
                let (from_k, to_k) = match (from_kind, to_kind) {
                    (Some(f), Some(t)) => (f, t),
                    _ => continue, // endpoint-missing already reported
                };

                let violation = match edge.kind {
                    EdgeKind::BelongsTo => {
                        from_k != NodeKind::Decision || to_k != NodeKind::Component
                    }
                    EdgeKind::ConnectsTo => {
                        from_k != NodeKind::Component || to_k != NodeKind::Component
                    }
                    EdgeKind::DependsOn | EdgeKind::Constrains => {
                        from_k != NodeKind::Decision || to_k != NodeKind::Decision
                    }
                    EdgeKind::MemberOf => from_k != NodeKind::Pattern || to_k != NodeKind::Decision,
                    EdgeKind::AppliesTo => {
                        from_k != NodeKind::Pattern || to_k != NodeKind::Component
                    }
                };

                if violation {
                    issues.push(Issue {
                        kind: IssueKind::EdgeKindMismatch,
                        subject: edge_subject(from, edge),
                        message: format!(
                            "{} edge `{from}` ({}) -> `{}` ({}): invalid node kinds",
                            edge.kind.as_str(),
                            from_k.as_str(),
                            edge.target,
                            to_k.as_str()
                        ),
                    });
                }
            }
        }
    }

    fn check_self_edges(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                if *from == edge.target {
                    issues.push(Issue {
                        kind: IssueKind::SelfEdge,
                        subject: edge_subject(from, edge),
                        message: format!("self-edge on `{from}` ({})", edge.kind.as_str()),
                    });
                }
            }
        }
    }

    /// No two edges share source, target and kind. Edges are grouped by
    /// source, so a duplicate sits in the same list as the edge it repeats.
    fn check_duplicate_edges(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            let mut edges: Vec<&Edge> = edge_list.iter().collect();
            edges.sort_unstable_by(|a, b| (&a.target, a.kind).cmp(&(&b.target, b.kind)));
            for (earlier, edge) in edges.iter().zip(edges.iter().skip(1)) {
                if (&earlier.target, earlier.kind) == (&edge.target, edge.kind) {
                    issues.push(Issue {
                        kind: IssueKind::DuplicateEdge,
                        subject: edge_subject(from, edge),
                        message: format!(
                            "duplicate {} edge `{from}` -> `{}`",
                            edge.kind.as_str(),
                            edge.target
                        ),
                    });
                }
            }
        }
    }

    /// Every pattern has at least two `MemberOf` edges.
    fn check_pattern_membership(&self, issues: &mut Vec<Issue>) {
        for (name, meta) in &self.nodes {
            if meta.kind != NodeKind::Pattern {
                continue;
            }
            let member_count = self
                .forward
                .get(name)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|e| e.kind == EdgeKind::MemberOf)
                        .count()
                })
                .unwrap_or(0);
            if member_count < 2 {
                issues.push(Issue {
                    kind: IssueKind::PatternTooFewMembers,
                    subject: name.to_string(),
                    message: format!(
                        "pattern `{name}` has {member_count} member decision(s) (minimum 2)"
                    ),
                });
            }
        }
    }

    fn check_depends_on_cycles(&self, issues: &mut Vec<Issue>) {
        for members in self.depends_on_cycles() {
            let listed: Vec<String> = members.iter().map(|name| format!("`{name}`")).collect();
            issues.push(Issue {
                kind: IssueKind::DependsOnCycle,
                subject: members.join(", "),
                message: format!("depends_on cycle among {}", listed.join(", ")),
            });
        }
    }

    /// Decision and pattern fields that must not be empty, and the component
    /// each decision names.
    fn check_content_integrity(&self, issues: &mut Vec<Issue>) {
        for (name, dec) in &self.decisions {
            if dec.decision.choice.trim().is_empty() {
                issues.push(Issue {
                    kind: IssueKind::EmptyChoice,
                    subject: name.to_string(),
                    message: format!("decision `{name}` has empty choice"),
                });
            }
            if dec.decision.reason.trim().is_empty() {
                issues.push(Issue {
                    kind: IssueKind::EmptyReason,
                    subject: name.to_string(),
                    message: format!("decision `{name}` has empty reason"),
                });
            }
            let comp = &dec.decision.component;
            if comp != "project" && !self.components.contains_key(comp.as_str()) {
                issues.push(Issue {
                    kind: IssueKind::DecisionComponentMissing,
                    subject: name.to_string(),
                    message: format!(
                        "decision `{name}` references component `{comp}` which does not exist"
                    ),
                });
            }
            if comp != "project" && !is_valid_kebab_case(comp) {
                issues.push(Issue {
                    kind: IssueKind::DecisionComponentInvalid,
                    subject: name.to_string(),
                    message: format!(
                        "decision `{name}` has invalid component `{comp}` \
                         (must be kebab-case or \"project\")"
                    ),
                });
            }
        }
        for (name, pat) in &self.patterns {
            if pat.pattern.name.trim().is_empty() {
                issues.push(Issue {
                    kind: IssueKind::EmptyPatternName,
                    subject: name.to_string(),
                    message: format!("pattern `{name}` has empty name"),
                });
            }
            if pat.pattern.description.trim().is_empty() {
                issues.push(Issue {
                    kind: IssueKind::EmptyPatternDescription,
                    subject: name.to_string(),
                    message: format!("pattern `{name}` has empty description"),
                });
            }
        }
    }

    /// Every Decision node has exactly one BelongsTo edge whose target matches
    /// the `decision.component` field in the node file.
    fn check_belongs_to_integrity(&self, issues: &mut Vec<Issue>) {
        for (name, meta) in &self.nodes {
            if meta.kind != NodeKind::Decision {
                continue;
            }
            let edges = self.forward.get(name);
            let belongs_to_count = edges
                .map(|el| el.iter().filter(|e| e.kind == EdgeKind::BelongsTo).count())
                .unwrap_or(0);

            if belongs_to_count == 0 {
                issues.push(Issue {
                    kind: IssueKind::MissingBelongsTo,
                    subject: name.to_string(),
                    message: format!("decision `{name}` has no BelongsTo edge"),
                });
            } else if belongs_to_count > 1 {
                issues.push(Issue {
                    kind: IssueKind::MultipleBelongsTo,
                    subject: name.to_string(),
                    message: format!(
                        "decision `{name}` has {belongs_to_count} BelongsTo edges (must be exactly 1)",
                    ),
                });
            }

            if let (Some(el), Some(dec)) = (edges, self.decisions.get(name)) {
                for edge in el.iter().filter(|e| e.kind == EdgeKind::BelongsTo) {
                    if edge.target.as_ref() != dec.decision.component {
                        issues.push(Issue {
                            kind: IssueKind::BelongsToMismatch,
                            subject: edge_subject(name, edge),
                            message: format!(
                                "decision `{name}` BelongsTo target `{}` does not match \
                                 decision.component `{}`",
                                edge.target, dec.decision.component
                            ),
                        });
                    }
                }
            }
        }
    }

    fn check_name_integrity(&self, issues: &mut Vec<Issue>) {
        for (key, comp) in &self.components {
            if key.as_ref() != comp.component.name {
                issues.push(Issue {
                    kind: IssueKind::ComponentNameMismatch,
                    subject: key.to_string(),
                    message: format!(
                        "component key `{key}` does not match internal name `{}`",
                        comp.component.name
                    ),
                });
            }
            if !is_valid_kebab_case(&comp.component.name) {
                issues.push(Issue {
                    kind: IssueKind::ComponentNameInvalid,
                    subject: key.to_string(),
                    message: format!(
                        "component `{key}` has invalid name `{}` (must be kebab-case)",
                        comp.component.name
                    ),
                });
            }
        }
        // Decision keys (filenames) must be kebab-case: slugify enforces it on
        // creation, but manual edits or external tools could violate it.
        for key in self.decisions.keys() {
            if !is_valid_kebab_case(key) {
                issues.push(Issue {
                    kind: IssueKind::DecisionNameInvalid,
                    subject: key.to_string(),
                    message: format!("decision key `{key}` is not valid kebab-case"),
                });
            }
        }
        // Pattern names are human-readable (e.g. "All persistent state uses Redis")
        // and intentionally differ from the kebab-case filename key. No key-vs-name
        // check: only content checks (empty name/description) apply.
    }

    /// Every node in the index must have matching content in the typed cache.
    /// Catches graph.toml / node-file desync (missing files, parse failures
    /// that were swallowed, or manual index edits).
    fn check_node_content_coherence(&self, issues: &mut Vec<Issue>) {
        for (name, meta) in &self.nodes {
            // "project" is a virtual component node with no on-disk content file.
            if name.as_ref() == "project" {
                continue;
            }
            let has_content = match meta.kind {
                NodeKind::Component => self.components.contains_key(name),
                NodeKind::Decision => self.decisions.contains_key(name),
                NodeKind::Pattern => self.patterns.contains_key(name),
            };
            if !has_content {
                issues.push(Issue {
                    kind: IssueKind::NodeContentMissing,
                    subject: name.to_string(),
                    message: format!(
                        "{} node `{name}` exists in index but has no content \
                         (file may be missing or unparseable)",
                        meta.kind.as_str()
                    ),
                });
            }
        }
    }
}

/// Sort key of [`InMemoryGraph::validate`]: errors first, then by kind.
fn order(issue: &Issue) -> (Severity, IssueKind, &str, &str) {
    (issue.severity(), issue.kind, &issue.subject, &issue.message)
}

fn edge_subject(from: &str, edge: &Edge) -> String {
    format!("{from} -> {} ({})", edge.target, edge.kind.as_str())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::graph::NodeMeta;
    use crate::store::schema::*;
    use crate::store::testing::{arc_map, depends_on_graph, test_graph, ts};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // ── validate: clean graph ────────────────────────────────────────────

    #[test]
    fn validate_clean_graph() {
        let g = test_graph();
        let issues = g.validate();
        assert!(issues.is_empty(), "expected no issues, got: {issues:?}");
    }

    // ── validate: edge endpoint missing ──────────────────────────────────

    #[test]
    fn validate_catches_dangling_edge_target() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "a".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![EdgeEntry {
                from: "a".into(),
                to: "ghost".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("ghost")));
    }

    // ── validate: edge type violations ───────────────────────────────────

    #[test]
    fn validate_catches_belongs_to_wrong_types() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "c1".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "c2".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![
                // BelongsTo must be decision → component, not component → component.
                EdgeEntry {
                    from: "c1".into(),
                    to: "c2".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.severity() == Severity::Error && i.message.contains("belongs_to"))
        );
    }

    #[test]
    fn validate_catches_connects_to_wrong_types() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "d1".into(),
                to: "d2".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("connects_to")));
    }

    // ── validate: self-edge ──────────────────────────────────────────────

    #[test]
    fn validate_catches_self_edge() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "a".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![EdgeEntry {
                from: "a".into(),
                to: "a".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("self-edge")));
    }

    // ── validate: duplicate edge ─────────────────────────────────────────

    #[test]
    fn validate_catches_duplicate_edge() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::ConnectsTo,
                },
            ],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("duplicate")));
    }

    // ── validate: pattern < 2 members ────────────────────────────────────

    #[test]
    fn validate_catches_pattern_too_few_members() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "pat".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "pat".into(),
                to: "d1".into(),
                kind: EdgeKind::MemberOf,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("minimum 2")));
    }

    // ── validate: cycle in depends_on ────────────────────────────────────

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
        let g = depends_on_graph(&[("tail", "a"), ("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(g.validate(), [cycle(&["a", "b", "c"])]);
    }

    /// A depth-first search that returns at the first cycle it meets leaves
    /// `r` marked as on its path, and later reports `s` as a cycle.
    #[test]
    fn nodes_upstream_of_a_cycle_raise_no_issue() {
        let g = depends_on_graph(&[("r", "a"), ("a", "b"), ("b", "a"), ("s", "r")]);
        assert_eq!(g.validate(), [cycle(&["a", "b"])]);
    }

    /// Two graphs equal but for edge order, each with its own hash maps,
    /// validate to the same sorted list.
    #[test]
    fn issues_are_sorted_whatever_the_input_order() {
        let mut edges = vec![
            ("a", "b"),
            ("b", "a"),
            ("a", "b"),
            ("x", "y"),
            ("y", "x"),
            ("m", "m"),
            ("n", "n"),
        ];
        let forward = depends_on_graph(&edges).validate();
        edges.reverse();
        let backward = depends_on_graph(&edges).validate();

        assert_eq!(forward, backward);
        let kinds: Vec<IssueKind> = forward.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                IssueKind::SelfEdge,
                IssueKind::SelfEdge,
                IssueKind::DuplicateEdge,
                IssueKind::DependsOnCycle,
                IssueKind::DependsOnCycle,
            ]
        );
        assert!(forward.is_sorted_by_key(|i| (i.severity(), i.kind, i.subject.clone())));
    }

    // introduced_errors

    #[test]
    fn a_cycle_the_graph_had_is_not_introduced() {
        let before = depends_on_graph(&[("a", "b"), ("b", "a")]);
        let after = depends_on_graph(&[("a", "b"), ("b", "a"), ("c", "a")]);
        assert_eq!(after.introduced_errors(&before), []);
    }

    #[test]
    fn a_second_cycle_is_introduced() {
        let before = depends_on_graph(&[("a", "b"), ("b", "a")]);
        let after = depends_on_graph(&[("a", "b"), ("b", "a"), ("c", "d"), ("d", "c")]);
        assert_eq!(after.introduced_errors(&before), [cycle(&["c", "d"])]);
    }

    #[test]
    fn a_cycle_that_gains_a_member_is_introduced() {
        let before = depends_on_graph(&[("a", "b"), ("b", "a")]);
        let after = depends_on_graph(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "a")]);
        assert_eq!(after.introduced_errors(&before), [cycle(&["a", "b", "c"])]);
    }

    /// The pattern's member count is in the message, not in the subject.
    #[test]
    fn an_error_whose_message_changes_is_not_introduced() {
        let with_members = |members: &[&str]| {
            let mut g = depends_on_graph(&[("a", "b")]);
            g.nodes.insert(
                "pat".into(),
                NodeMeta {
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: String::new(),
                },
            );
            g.patterns.insert(
                "pat".into(),
                Arc::new(PatternFile {
                    pattern: Pattern {
                        name: "Pattern".into(),
                        description: "Shared approach".into(),
                    },
                }),
            );
            let edges = members.iter().map(|&member| Edge {
                target: member.into(),
                kind: EdgeKind::MemberOf,
            });
            g.forward.entry("pat".into()).or_default().extend(edges);
            g
        };
        let before = with_members(&["a"]);
        let after = with_members(&[]);
        assert_ne!(before.validate(), after.validate());
        assert_eq!(after.introduced_errors(&before), []);
    }

    // ── validate: empty choice / reason ──────────────────────────────────

    #[test]
    fn validate_catches_empty_choice() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "bad".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: String::new(),
                    reason: "ok".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty choice")));
    }

    #[test]
    fn validate_catches_empty_reason() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "bad".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "ok".into(),
                    reason: "   ".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty reason")));
    }

    // ── validate: name integrity ─────────────────────────────────────────

    #[test]
    fn validate_catches_component_name_mismatch() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "auth".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "WRONG".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("does not match")));
    }

    #[test]
    fn validate_catches_non_kebab_component() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "Bad_Name".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "Bad_Name".into(),
            ComponentFile {
                component: Component {
                    name: "Bad_Name".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("kebab-case")));
    }
    // ── validate: BelongsTo integrity ────────────────────────────────────

    #[test]
    fn validate_catches_missing_belongs_to() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![], // no BelongsTo edge
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("no BelongsTo")));
    }

    #[test]
    fn validate_catches_duplicate_belongs_to() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "comp-a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "comp-b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "dec".into(),
                    to: "comp-a".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "dec".into(),
                    to: "comp-b".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-a".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp-a".into(),
            ComponentFile {
                component: Component {
                    name: "comp-a".into(),
                    description: String::new(),
                },
            },
        );
        components.insert(
            "comp-b".into(),
            ComponentFile {
                component: Component {
                    name: "comp-b".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("2 BelongsTo edges"))
        );
    }

    #[test]
    fn validate_catches_belongs_to_target_mismatch() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "comp-a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "comp-b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "dec".into(),
                to: "comp-a".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-b".into(), // does NOT match the edge target
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp-a".into(),
            ComponentFile {
                component: Component {
                    name: "comp-a".into(),
                    description: String::new(),
                },
            },
        );
        components.insert(
            "comp-b".into(),
            ComponentFile {
                component: Component {
                    name: "comp-b".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("comp-a") && i.message.contains("comp-b"))
        );
    }

    // ── validate: pattern content ────────────────────────────────────────

    #[test]
    fn validate_catches_empty_pattern_name() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "pat".into(),
                kind: NodeKind::Pattern,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "pat".into(),
            PatternFile {
                pattern: Pattern {
                    name: String::new(),
                    description: "something".into(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &arc_map(patterns),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty name")));
    }

    #[test]
    fn validate_allows_pattern_name_different_from_key() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "state-in-redis".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "state-in-redis".into(),
                    to: "d1".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "state-in-redis".into(),
                    to: "d2".into(),
                    kind: EdgeKind::MemberOf,
                },
            ],
        };
        let mut decisions = BTreeMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "project".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        tags: vec![],
                        attribution: Attribution::User,
                        created: ts(),
                        code_refs: vec![],
                        history: vec![],
                    },
                },
            );
        }
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "state-in-redis".into(),
            PatternFile {
                pattern: Pattern {
                    // Human-readable name, intentionally different from the key.
                    name: "All persistent state uses Redis".into(),
                    description: "Shared Redis pool via app state".into(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &arc_map(patterns),
        );
        let issues = g.validate();
        // Must NOT produce any warnings or errors about name mismatch.
        assert!(
            issues.is_empty(),
            "expected no issues, got: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    // ── validate: node-content coherence ─────────────────────────────────

    #[test]
    fn validate_catches_orphan_node_without_content() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "ghost".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "g".into(),
                },
            ],
            edges: vec![],
        };
        // ghost exists in nodes but has no DecisionFile in the content map.
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("ghost") && i.message.contains("no content")),
            "should flag orphan node: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn validate_allows_project_virtual_node_without_content() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "p".into(),
            }],
            edges: vec![],
        };
        // "project" has no ComponentFile — it's virtual. Must not error.
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            !issues.iter().any(|i| i.message.contains("no content")),
            "project virtual node should be exempt: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    // ── validate: decision key kebab-case ────────────────────────────────

    #[test]
    fn validate_catches_non_kebab_decision_key() {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "Bad_Key".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "b".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "Bad_Key".into(),
                to: "project".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "Bad_Key".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("Bad_Key") && i.message.contains("kebab-case")),
            "should flag non-kebab decision key: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }
}
