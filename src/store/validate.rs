//! Graph integrity validation.
//!
//! All validation checks for [`InMemoryGraph`]. Separated from the core
//! graph module for readability — the validation surface is ~350 lines
//! across 11 checks and does not affect the query or build paths.

use std::collections::HashSet;

use super::graph::{Edge, InMemoryGraph, Issue, Severity};
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
                            "edge target `{}` (from `{from}`, {:?}) is not a known node",
                            edge.target, edge.kind
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
                    EdgeKind::DependsOn | EdgeKind::Constrains | EdgeKind::Supersedes => {
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
                            "{:?} edge `{from}` ({from_k:?}) → `{}` ({to_k:?}): \
                             invalid node kinds",
                            edge.kind, edge.target
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
                        message: format!("self-edge on `{from}` ({:?})", edge.kind),
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
                            "duplicate {:?} edge `{from}` → `{}`",
                            edge.kind, edge.target
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
            let belongs_to: Vec<&Edge> = self
                .forward
                .get(name)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|e| e.kind == EdgeKind::BelongsTo)
                        .collect()
                })
                .unwrap_or_default();

            if belongs_to.is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has no BelongsTo edge"),
                    node: Some(name.to_string()),
                });
            } else if belongs_to.len() > 1 {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` has {} BelongsTo edges (must be exactly 1)",
                        belongs_to.len()
                    ),
                    node: Some(name.to_string()),
                });
            }

            // Verify BelongsTo target matches the decision file's component field.
            if let Some(dec) = self.decisions.get(name) {
                for edge in &belongs_to {
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
                        "{:?} node `{name}` exists in index but has no content \
                         (file may be missing or unparseable)",
                        meta.kind
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
    }
}
