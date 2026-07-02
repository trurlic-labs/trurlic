//! Graph integrity validation.
//!
//! All validation checks for [`InMemoryGraph`]. Separated from the core
//! graph module for readability — the validation surface is ~350 lines
//! across 11 checks and does not affect the query or build paths.

use std::collections::HashSet;

use super::graph::{InMemoryGraph, Issue, Severity};
use super::schema::{EdgeKind, NodeKind};
use super::state::is_valid_kebab_case;

impl InMemoryGraph {
    /// Full graph integrity check. Returns empty vec when valid.
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
        issues
    }

    // ── Validation checks ────────────────────────────────────────────────

    /// Checks 1-2: every edge endpoint exists in nodes.
    fn check_edge_endpoints(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            if !self.nodes.contains_key(from) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("edge source `{from}` is not a known node"),
                    node: Some(from.to_string()),
                });
            }
            for edge in edge_list {
                if !self.nodes.contains_key(&edge.target) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!(
                            "edge target `{}` (from `{from}`, {}) is not a known node",
                            edge.target,
                            edge.kind.as_str()
                        ),
                        node: Some(edge.target.to_string()),
                    });
                }
            }
        }
    }

    /// Checks 3-7: edge kind → node kind constraints.
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
                        severity: Severity::Error,
                        message: format!(
                            "{} edge `{from}` ({}) → `{}` ({}): \
                             invalid node kinds",
                            edge.kind.as_str(),
                            from_k.as_str(),
                            edge.target,
                            to_k.as_str()
                        ),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 14: no self-edges.
    fn check_self_edges(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                if *from == edge.target {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!("self-edge on `{from}` ({})", edge.kind.as_str()),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 10: no duplicate edges (same from + to + kind).
    fn check_duplicate_edges(&self, issues: &mut Vec<Issue>) {
        let mut seen: HashSet<(&str, &str, EdgeKind)> = HashSet::new();
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                if !seen.insert((from, &edge.target, edge.kind)) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!(
                            "duplicate {} edge `{from}` → `{}`",
                            edge.kind.as_str(),
                            edge.target
                        ),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 8: every pattern has ≥ 2 MemberOf edges.
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
                    severity: Severity::Error,
                    message: format!(
                        "pattern `{name}` has {member_count} member decision(s) (minimum 2)"
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
    }

    /// Check 9: no cycles in the DependsOn subgraph.
    fn check_depends_on_cycles(&self, issues: &mut Vec<Issue>) {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut in_stack: HashSet<&str> = HashSet::new();

        for (name, meta) in &self.nodes {
            if meta.kind == NodeKind::Decision
                && !visited.contains(name.as_ref())
                && self.dfs_has_cycle(name, &mut visited, &mut in_stack)
            {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("cycle in depends_on chain involving `{name}`"),
                    node: Some(name.to_string()),
                });
            }
        }
    }

    fn dfs_has_cycle<'a>(
        &'a self,
        node: &'a str,
        visited: &mut HashSet<&'a str>,
        in_stack: &mut HashSet<&'a str>,
    ) -> bool {
        visited.insert(node);
        in_stack.insert(node);

        if let Some(edges) = self.forward.get(node) {
            for edge in edges {
                if edge.kind != EdgeKind::DependsOn {
                    continue;
                }
                let target: &str = &edge.target;
                if in_stack.contains(target) {
                    return true;
                }
                if !visited.contains(target) && self.dfs_has_cycle(target, visited, in_stack) {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    /// Checks 11-13: content integrity.
    fn check_content_integrity(&self, issues: &mut Vec<Issue>) {
        for (name, dec) in &self.decisions {
            if dec.decision.choice.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has empty choice"),
                    node: Some(name.to_string()),
                });
            }
            if dec.decision.reason.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has empty reason"),
                    node: Some(name.to_string()),
                });
            }
            let comp = &dec.decision.component;
            if comp != "project" && !self.components.contains_key(comp.as_str()) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` references component `{comp}` which does not exist"
                    ),
                    node: Some(name.to_string()),
                });
            }
            if comp != "project" && !is_valid_kebab_case(comp) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` has invalid component `{comp}` \
                         (must be kebab-case or \"project\")"
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
        for (name, pat) in &self.patterns {
            if pat.pattern.name.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("pattern `{name}` has empty name"),
                    node: Some(name.to_string()),
                });
            }
            if pat.pattern.description.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Warning,
                    message: format!("pattern `{name}` has empty description"),
                    node: Some(name.to_string()),
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
                    severity: Severity::Error,
                    message: format!("decision `{name}` has no BelongsTo edge"),
                    node: Some(name.to_string()),
                });
            } else if belongs_to_count > 1 {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` has {belongs_to_count} BelongsTo edges (must be exactly 1)",
                    ),
                    node: Some(name.to_string()),
                });
            }

            if let (Some(el), Some(dec)) = (edges, self.decisions.get(name)) {
                for edge in el.iter().filter(|e| e.kind == EdgeKind::BelongsTo) {
                    if edge.target.as_ref() != dec.decision.component {
                        issues.push(Issue {
                            severity: Severity::Error,
                            message: format!(
                                "decision `{name}` BelongsTo target `{}` does not match \
                                 decision.component `{}`",
                                edge.target, dec.decision.component
                            ),
                            node: Some(name.to_string()),
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
                    severity: Severity::Error,
                    message: format!(
                        "component key `{key}` does not match internal name `{}`",
                        comp.component.name
                    ),
                    node: Some(key.to_string()),
                });
            }
            if !is_valid_kebab_case(&comp.component.name) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "component `{key}` has invalid name `{}` (must be kebab-case)",
                        comp.component.name
                    ),
                    node: Some(key.to_string()),
                });
            }
        }
        // Decision keys (filenames) must be kebab-case — enforced by slugify
        // on creation, but manual edits or external tools could violate this.
        for key in self.decisions.keys() {
            if !is_valid_kebab_case(key) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision key `{key}` is not valid kebab-case"),
                    node: Some(key.to_string()),
                });
            }
        }
        // Pattern names are human-readable (e.g. "All persistent state uses Redis")
        // and intentionally differ from the kebab-case filename key. No key-vs-name
        // check — only content checks (empty name/description) apply.
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
                    severity: Severity::Error,
                    message: format!(
                        "{} node `{name}` exists in index but has no content \
                         (file may be missing or unparseable)",
                        meta.kind.as_str()
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use crate::store::testing::{arc_map, test_graph, ts};
    use std::collections::BTreeMap;

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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
                .any(|i| i.severity == Severity::Error && i.message.contains("belongs_to"))
        );
    }

    #[test]
    fn validate_catches_connects_to_wrong_types() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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

    #[test]
    fn validate_catches_depends_on_cycle() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "d2".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "d1".into(),
                    kind: EdgeKind::DependsOn,
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
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("cycle")));
    }

    // ── validate: empty choice / reason ──────────────────────────────────

    #[test]
    fn validate_catches_empty_choice() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
            rebuilt: ts(),
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
