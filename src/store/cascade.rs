//! Cascade pre-flight checks for node removal.
//!
//! Before removing a decision or component, these checks identify
//! blockers (dependents that must be removed first), warnings (side
//! effects the user should know about), and cleanups (outgoing edges
//! that will be silently removed).
//!
//! Shared by CLI, MCP server, and map API to enforce identical
//! deletion rules regardless of the mutation entry point.

use super::graph::{Direction, InMemoryGraph};
use super::schema::EdgeKind;

// ── Cascade types ────────────────────────────────────────────────────────

/// A hard blocker that prevents node removal.
#[derive(Debug, Clone)]
pub struct CascadeBlocker {
    /// The node that causes the block.
    pub node: String,
    /// The edge kind creating the dependency.
    pub edge: EdgeKind,
    /// Human-readable explanation.
    pub message: String,
}

/// A non-blocking side-effect the user should know about.
#[derive(Debug, Clone)]
pub struct CascadeEffect {
    /// The affected node.
    pub node: String,
    /// The edge kind being removed or broken.
    pub edge: EdgeKind,
    /// Human-readable explanation.
    pub message: String,
}

/// An outgoing edge from the deleted node that will be silently removed.
#[derive(Debug, Clone)]
pub struct CascadeCleanup {
    /// The edge kind being removed.
    pub edge: EdgeKind,
    /// The target node of the edge.
    pub target: String,
}

/// Result of a cascade pre-flight check before removing a node.
///
/// Shared by CLI, MCP, and map API to enforce identical deletion rules
/// regardless of the mutation entry point.
#[derive(Debug, Clone)]
pub struct CascadeResult {
    /// Non-empty means removal is blocked.
    pub blockers: Vec<CascadeBlocker>,
    /// Non-blocking side-effects the user should be aware of.
    pub warnings: Vec<CascadeEffect>,
    /// Outgoing edges that will be silently cleaned up.
    pub cleanups: Vec<CascadeCleanup>,
}

impl CascadeResult {
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }

    /// Concatenate blocker messages for flat error reporting (CLI, map 409).
    #[must_use]
    pub fn blocker_summary(&self) -> String {
        self.blockers
            .iter()
            .map(|b| b.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

// ── Cascade checks ──────────────────────────────────────────────────────

impl InMemoryGraph {
    pub fn check_decision_cascade(&self, name: &str) -> CascadeResult {
        let involved = self.edges_involving(name);
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut cleanups = Vec::new();

        for (other, edge, dir) in &involved {
            match (edge.kind, *dir) {
                // Block: other decisions depend on this one.
                (EdgeKind::DependsOn, Direction::Reverse) => {
                    blockers.push(CascadeBlocker {
                        node: other.to_string(),
                        edge: EdgeKind::DependsOn,
                        message: format!(
                            "decision `{other}` depends on `{name}` — \
                             remove or update it first"
                        ),
                    });
                }
                // Block or warn: pattern membership.
                (EdgeKind::MemberOf, Direction::Reverse) => {
                    let member_count = self.forward_edge_count(other, EdgeKind::MemberOf);
                    if member_count <= 2 {
                        blockers.push(CascadeBlocker {
                            node: other.to_string(),
                            edge: EdgeKind::MemberOf,
                            message: format!(
                                "pattern `{other}` would have fewer than 2 members — \
                                 remove or update the pattern first"
                            ),
                        });
                    } else {
                        warnings.push(CascadeEffect {
                            node: other.to_string(),
                            edge: EdgeKind::MemberOf,
                            message: format!(
                                "pattern `{other}` will be updated to exclude this decision"
                            ),
                        });
                    }
                }
                // Warn: incoming constrains edges.
                (EdgeKind::Constrains, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::Constrains,
                        message: format!("constraint from `{other}` will be removed"),
                    });
                }
                // Clean: all outgoing edges from this decision.
                (_, Direction::Forward) => {
                    cleanups.push(CascadeCleanup {
                        edge: edge.kind,
                        target: other.to_string(),
                    });
                }
                // Reverse edges not relevant to decision removal.
                (EdgeKind::BelongsTo, Direction::Reverse)
                | (EdgeKind::ConnectsTo, Direction::Reverse)
                | (EdgeKind::AppliesTo, Direction::Reverse) => {}
            }
        }

        CascadeResult {
            blockers,
            warnings,
            cleanups,
        }
    }

    /// Pre-flight check for removing a component. Returns blockers that
    /// must prevent the removal, warnings about side-effects, and cleanups
    /// for outgoing edges that will be silently removed.
    #[must_use]
    pub fn check_component_cascade(&self, name: &str) -> CascadeResult {
        let involved = self.edges_involving(name);
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut cleanups = Vec::new();

        for (other, edge, dir) in &involved {
            match (edge.kind, *dir) {
                // Block: decisions belong to this component.
                (EdgeKind::BelongsTo, Direction::Reverse) => {
                    blockers.push(CascadeBlocker {
                        node: other.to_string(),
                        edge: EdgeKind::BelongsTo,
                        message: format!(
                            "decision `{other}` belongs to `{name}` — \
                             remove or reassign it first"
                        ),
                    });
                }
                // Warn: patterns that apply to this component.
                (EdgeKind::AppliesTo, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::AppliesTo,
                        message: format!(
                            "pattern `{other}` applies_to association will be removed"
                        ),
                    });
                }
                // Warn: incoming connections from other components.
                (EdgeKind::ConnectsTo, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::ConnectsTo,
                        message: format!("incoming connection from `{other}` will be removed"),
                    });
                }
                // Clean: all outgoing edges from this component.
                (_, Direction::Forward) => {
                    cleanups.push(CascadeCleanup {
                        edge: edge.kind,
                        target: other.to_string(),
                    });
                }
                // Reverse edges not relevant to component removal.
                (EdgeKind::DependsOn, Direction::Reverse)
                | (EdgeKind::Constrains, Direction::Reverse)
                | (EdgeKind::MemberOf, Direction::Reverse) => {}
            }
        }

        CascadeResult {
            blockers,
            warnings,
            cleanups,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::graph::InMemoryGraph;
    use crate::store::schema::*;
    use crate::store::testing::{arc_map, ts};
    use std::collections::BTreeMap;

    // ── cascade checks ──────────────────────────────────────────────────

    fn cascade_graph() -> InMemoryGraph {
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
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "d".into(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "token-expiry".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "db-pool".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
                NodeEntry {
                    name: "auth-pattern".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "4".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "use-jwt".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "token-expiry".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "db-pool".into(),
                    to: "database".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "token-expiry".into(),
                    to: "use-jwt".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "use-jwt".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "token-expiry".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "auth".into(),
                    kind: EdgeKind::AppliesTo,
                },
            ],
        };

        let mut components = BTreeMap::new();
        for name in ["auth", "database"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: String::new(),
                    },
                },
            );
        }
        let mut decisions = BTreeMap::new();
        for (name, comp) in [
            ("use-jwt", "auth"),
            ("token-expiry", "auth"),
            ("db-pool", "database"),
        ] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: comp.into(),
                        choice: name.into(),
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
        }
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "auth-pattern".into(),
            PatternFile {
                pattern: Pattern {
                    name: "Auth pattern".into(),
                    description: "test".into(),
                },
            },
        );

        InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &arc_map(patterns),
        )
    }

    #[test]
    fn cascade_decision_blocks_when_depended_on() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("use-jwt");
        assert!(r.is_blocked());
        assert!(
            r.blockers
                .iter()
                .any(|b| b.node == "token-expiry" && b.edge == EdgeKind::DependsOn)
        );
    }

    #[test]
    fn cascade_decision_blocks_when_pattern_would_shrink() {
        let g = cascade_graph();
        // token-expiry is a member of auth-pattern (2 members total).
        let r = g.check_decision_cascade("token-expiry");
        // Also blocked by nothing depending on it via DependsOn (only use-jwt has
        // DependsOn, pointing the other way). But pattern check should block.
        assert!(r.is_blocked());
        assert!(r.blockers.iter().any(|b| b.node == "auth-pattern"
            && b.edge == EdgeKind::MemberOf
            && b.message.contains("fewer than 2")));
    }

    #[test]
    fn cascade_decision_allows_independent() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("db-pool");
        assert!(!r.is_blocked());
    }

    #[test]
    fn cascade_decision_collects_cleanups() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("db-pool");
        // db-pool has a forward BelongsTo edge to database.
        assert!(
            r.cleanups
                .iter()
                .any(|c| c.edge == EdgeKind::BelongsTo && c.target == "database")
        );
    }

    #[test]
    fn cascade_component_blocks_when_has_decisions() {
        let g = cascade_graph();
        let r = g.check_component_cascade("auth");
        assert!(r.is_blocked());
        assert!(
            r.blockers
                .iter()
                .any(|b| b.node == "use-jwt" || b.node == "token-expiry")
        );
    }

    #[test]
    fn cascade_component_warns_about_patterns_and_connections() {
        let g = cascade_graph();
        let r = g.check_component_cascade("database");
        // database has no decisions belonging to it except db-pool, but
        // db-pool's BelongsTo edge makes it blocked.
        assert!(r.is_blocked());
        // Also has incoming ConnectsTo from auth.
        // Rebuild without the decision to test warnings only.
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
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "d".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "auth".into(),
                to: "database".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let mut components = BTreeMap::new();
        for name in ["auth", "database"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: String::new(),
                    },
                },
            );
        }
        let g2 = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let r2 = g2.check_component_cascade("database");
        assert!(!r2.is_blocked());
        assert!(
            r2.warnings
                .iter()
                .any(|w| w.node == "auth" && w.edge == EdgeKind::ConnectsTo)
        );
    }

    #[test]
    fn cascade_component_allows_empty() {
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
                    name: "orphan".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "o".into(),
                },
            ],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "orphan".into(),
            ComponentFile {
                component: Component {
                    name: "orphan".into(),
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
        let r = g.check_component_cascade("orphan");
        assert!(!r.is_blocked());
        assert!(r.warnings.is_empty());
        assert!(r.cleanups.is_empty());
    }

    #[test]
    fn cascade_result_is_blocked_semantics() {
        let empty = CascadeResult {
            blockers: vec![],
            warnings: vec![CascadeEffect {
                node: "x".into(),
                edge: EdgeKind::Constrains,
                message: "something".into(),
            }],
            cleanups: vec![],
        };
        assert!(!empty.is_blocked());
        let blocked = CascadeResult {
            blockers: vec![CascadeBlocker {
                node: "y".into(),
                edge: EdgeKind::DependsOn,
                message: "reason".into(),
            }],
            warnings: vec![],
            cleanups: vec![],
        };
        assert!(blocked.is_blocked());
    }

    #[test]
    fn cascade_blocker_summary_joins_messages() {
        let result = CascadeResult {
            blockers: vec![
                CascadeBlocker {
                    node: "a".into(),
                    edge: EdgeKind::DependsOn,
                    message: "first".into(),
                },
                CascadeBlocker {
                    node: "b".into(),
                    edge: EdgeKind::MemberOf,
                    message: "second".into(),
                },
            ],
            warnings: vec![],
            cleanups: vec![],
        };
        assert_eq!(result.blocker_summary(), "first; second");
    }
}
