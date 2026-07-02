use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── project.toml ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFile {
    pub trurlic_version: String,

    pub project: Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,

    pub description: String,
}

// ── components/<name>.toml ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentFile {
    pub component: Component,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Component {
    /// Kebab-case name, must match filename.
    pub name: String,

    pub description: String,
}

// ── decisions/<name>.toml ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionFile {
    pub decision: Decision,
}

/// Source code location where a decision manifests.
///
/// No line numbers by design — lines go stale on every edit. Symbols
/// (function names, struct names) are more durable and more meaningful
/// in prompts. If a symbol is renamed, the decision drifts — which is
/// correct behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeRef {
    /// Relative path from project root (forward slashes, no leading ./).
    pub file: String,

    /// Function, method, struct, or constant name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    #[default]
    User,
    Agent,
}

/// A prior version of a decision, captured when its choice or reason is revised.
///
/// A decision evolves in place: each revision pushes the pre-edit choice and
/// reason here before overwriting them. Metadata (tags, code refs, attribution)
/// updates without leaving history — only the substantive fields are versioned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Choice text as it stood before the revision.
    pub choice: String,

    /// Reason text as it stood before the revision.
    pub reason: String,

    /// When this version was replaced by a revision (UTC, RFC 3339).
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    /// Component this decision belongs to, or `"project"` for project-wide.
    pub component: String,

    pub choice: String,

    pub reason: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,

    /// Categorical tags for filtering and search.
    /// Source of truth: stored here in the node file, mirrored to graph index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(default)]
    pub attribution: Attribution,

    /// When this decision was recorded (UTC, ISO 8601 / RFC 3339).
    pub created: DateTime<Utc>,

    /// Source code locations where this decision manifests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_refs: Vec<CodeRef>,

    /// Prior versions, oldest first. Each revision of choice or reason
    /// appends the previous values here; reading top-to-bottom traces the
    /// decision's evolution up to its current form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

// ── patterns/<name>.toml ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatternFile {
    pub pattern: Pattern,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pattern {
    pub name: String,

    pub description: String,
}

// ── Graph index (graph.toml) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Component,
    Decision,
    Pattern,
}

impl NodeKind {
    /// Canonical snake_case string, matching serde serialization.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Decision => "decision",
            Self::Pattern => "pattern",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    BelongsTo,
    ConnectsTo,
    DependsOn,
    Constrains,
    MemberOf,
    AppliesTo,
}

impl EdgeKind {
    /// Canonical snake_case string, matching serde serialization.
    ///
    /// Use this for JSON payloads, WebSocket events, and any
    /// user-facing output — never `Debug` formatting, which
    /// produces CamelCase (`ConnectsTo`) instead of snake_case
    /// (`connects_to`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BelongsTo => "belongs_to",
            Self::ConnectsTo => "connects_to",
            Self::DependsOn => "depends_on",
            Self::Constrains => "constrains",
            Self::MemberOf => "member_of",
            Self::AppliesTo => "applies_to",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeEntry {
    pub name: String,
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EdgeEntry {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphIndex {
    pub version: u32,
    pub rebuilt: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<EdgeEntry>,
}

// ── Constants ────────────────────────────────────────────────────────────────

pub const FORMAT_VERSION: &str = "0.3.0";

pub const STORE_DIR: &str = ".trurlic";

pub const COMPONENTS_DIR: &str = "components";

pub const DECISIONS_DIR: &str = "decisions";

pub const PATTERNS_DIR: &str = "patterns";

pub const STATE_DIR: &str = ".state";

pub const GRAPH_FILE: &str = "graph.toml";

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn project_round_trip() {
        let file = ProjectFile {
            trurlic_version: "0.2.0".into(),
            project: Project {
                name: "my-project".into(),
                description: "Test project".into(),
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        let deserialized: ProjectFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn component_round_trip() {
        let file = ComponentFile {
            component: Component {
                name: "auth".into(),
                description: "Authentication and token management".into(),
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        let deserialized: ComponentFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn decision_round_trip() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT with DPoP binding".into(),
                reason: "Stateless, no session store needed".into(),
                alternatives: vec!["Session cookies — rejected: requires server-side state".into()],
                tags: vec!["security".into(), "auth".into()],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn decision_without_tags_deserializes() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
attribution = "user"
created = "2025-06-01T10:30:00Z"
"#;
        let file: DecisionFile = toml::from_str(toml_str).expect("deserialize");
        assert!(file.decision.tags.is_empty());
    }

    #[test]
    fn decision_tags_round_trip() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec!["security".into(), "auth".into()],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(serialized.contains("tags = ["));
        let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file.decision.tags, deserialized.decision.tags);
    }

    #[test]
    fn decision_empty_tags_omitted_in_toml() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            !serialized.contains("tags"),
            "empty tags should not appear in TOML"
        );
    }

    #[test]
    fn pattern_round_trip() {
        let file = PatternFile {
            pattern: Pattern {
                name: "All persistent state uses Redis".into(),
                description: "Shared Redis pool via app state".into(),
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        let deserialized: PatternFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn graph_index_round_trip() {
        let index = GraphIndex {
            version: 1,
            rebuilt: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
            nodes: vec![
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "abc123".into(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec!["auth".into(), "security".into()],
                    hash: "def456".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "use-jwt".into(),
                to: "auth".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let serialized = toml::to_string_pretty(&index).expect("serialize");
        let deserialized: GraphIndex = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(index, deserialized);
    }

    #[test]
    fn graph_index_empty_round_trip() {
        let index = GraphIndex {
            version: 1,
            rebuilt: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
            nodes: vec![],
            edges: vec![],
        };
        let serialized = toml::to_string_pretty(&index).expect("serialize");
        assert!(!serialized.contains("[[nodes]]"));
        assert!(!serialized.contains("[[edges]]"));
        let deserialized: GraphIndex = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(index, deserialized);
    }

    #[test]
    fn edge_kind_serializes_snake_case() {
        let edge = EdgeEntry {
            from: "a".into(),
            to: "b".into(),
            kind: EdgeKind::BelongsTo,
        };
        let serialized = toml::to_string_pretty(&edge).expect("serialize");
        assert!(
            serialized.contains(r#"kind = "belongs_to""#),
            "EdgeKind should serialize as snake_case, got:\n{serialized}"
        );
    }

    #[test]
    fn node_kind_serializes_snake_case() {
        let node = NodeEntry {
            name: "auth".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: "abc".into(),
        };
        let serialized = toml::to_string_pretty(&node).expect("serialize");
        assert!(
            serialized.contains(r#"kind = "component""#),
            "NodeKind should serialize as snake_case, got:\n{serialized}"
        );
    }

    #[test]
    fn all_edge_kinds_round_trip() {
        for kind in [
            EdgeKind::BelongsTo,
            EdgeKind::ConnectsTo,
            EdgeKind::DependsOn,
            EdgeKind::Constrains,
            EdgeKind::MemberOf,
            EdgeKind::AppliesTo,
        ] {
            let edge = EdgeEntry {
                from: "a".into(),
                to: "b".into(),
                kind,
            };
            let s = toml::to_string_pretty(&edge).expect("serialize");
            let d: EdgeEntry = toml::from_str(&s).expect("deserialize");
            assert_eq!(edge, d);
        }
    }

    #[test]
    fn decision_created_serializes_iso8601_utc() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            serialized.contains("2025-06-01T10:30:00Z"),
            "created must serialize as ISO 8601 with Z suffix, got:\n{serialized}"
        );
    }

    #[test]
    fn decision_deserializes_from_spec_format() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT with DPoP binding, 15min lease"
reason = "Stateless, no session store needed. DPoP prevents token theft."
alternatives = [
    "Session cookies — rejected: requires server-side state",
    "Opaque tokens — rejected: requires token introspection endpoint",
]
tags = ["security", "auth"]
attribution = "user"
created = "2025-06-01T10:30:00Z"
"#;
        let file: DecisionFile = toml::from_str(toml_str).expect("deserialize spec format");
        assert_eq!(file.decision.component, "auth");
        assert_eq!(file.decision.tags, vec!["security", "auth"]);
        assert_eq!(
            file.decision.created,
            Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap()
        );
    }

    #[test]
    fn decision_rejects_invalid_timestamp() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
attribution = "user"
created = "not-a-timestamp"
"#;
        let result = toml::from_str::<DecisionFile>(toml_str);
        assert!(result.is_err(), "invalid timestamp must be rejected");
    }

    #[test]
    fn decision_rejects_missing_timestamp() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
attribution = "user"
"#;
        let result = toml::from_str::<DecisionFile>(toml_str);
        assert!(result.is_err(), "missing created field must be rejected");
    }

    // ── as_str ──────────────────────────────────────────────────────────

    #[test]
    fn edge_kind_as_str_matches_serde() {
        for kind in [
            EdgeKind::BelongsTo,
            EdgeKind::ConnectsTo,
            EdgeKind::DependsOn,
            EdgeKind::Constrains,
            EdgeKind::MemberOf,
            EdgeKind::AppliesTo,
        ] {
            let edge = EdgeEntry {
                from: "a".into(),
                to: "b".into(),
                kind,
            };
            let serialized = toml::to_string_pretty(&edge).expect("serialize");
            let expected = format!("kind = \"{}\"", kind.as_str());
            assert!(
                serialized.contains(&expected),
                "{kind:?}.as_str() = {:?} must match serde output, got:\n{serialized}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn node_kind_as_str_matches_serde() {
        for kind in [NodeKind::Component, NodeKind::Decision, NodeKind::Pattern] {
            let node = NodeEntry {
                name: "x".into(),
                kind,
                tags: vec![],
                hash: "h".into(),
            };
            let serialized = toml::to_string_pretty(&node).expect("serialize");
            let expected = format!("kind = \"{}\"", kind.as_str());
            assert!(
                serialized.contains(&expected),
                "{kind:?}.as_str() = {:?} must match serde output, got:\n{serialized}",
                kind.as_str()
            );
        }
    }

    // ── attribution ────────────────────────────────────────────────────

    #[test]
    fn decision_with_attribution_round_trips() {
        for attr in [Attribution::User, Attribution::Agent] {
            let file = DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT".into(),
                    reason: "Stateless".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: attr,
                    created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                    code_refs: vec![],
                    history: vec![],
                },
            };
            let serialized = toml::to_string_pretty(&file).expect("serialize");
            let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
            assert_eq!(file, deserialized);
        }
    }

    #[test]
    fn decision_without_attribution_defaults_to_user() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
created = "2025-06-01T10:30:00Z"
"#;
        let file: DecisionFile = toml::from_str(toml_str).expect("should parse with default");
        assert_eq!(
            file.decision.attribution,
            Attribution::User,
            "missing attribution must default to User for 0.2.0 compatibility"
        );
    }

    #[test]
    fn attribution_serializes_snake_case() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            serialized.contains(r#"attribution = "user""#),
            "Attribution::User must serialize as snake_case, got:\n{serialized}"
        );

        let agent_file = DecisionFile {
            decision: Decision {
                attribution: Attribution::Agent,
                ..file.decision.clone()
            },
        };
        let agent_serialized = toml::to_string_pretty(&agent_file).expect("serialize");
        assert!(
            agent_serialized.contains(r#"attribution = "agent""#),
            "Attribution::Agent must serialize as snake_case, got:\n{agent_serialized}"
        );
    }

    #[test]
    fn code_ref_round_trip_with_symbol() {
        let file = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "BLAKE3 hashing".into(),
                reason: "Fast, no-std capable".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![
                    CodeRef {
                        file: "src/store/write.rs".into(),
                        symbol: Some("content_hash".into()),
                    },
                    CodeRef {
                        file: "src/store/validate.rs".into(),
                        symbol: Some("verify_hashes".into()),
                    },
                ],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(serialized.contains("[[decision.code_refs]]"));
        assert!(serialized.contains(r#"file = "src/store/write.rs""#));
        assert!(serialized.contains(r#"symbol = "content_hash""#));
        let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn code_ref_round_trip_without_symbol() {
        let file = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "Atomic writes".into(),
                reason: "Crash safety".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![CodeRef {
                    file: "src/store/write.rs".into(),
                    symbol: None,
                }],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            !serialized.contains("symbol"),
            "None symbol should be omitted"
        );
        let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn decision_without_code_refs_deserializes() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
attribution = "user"
created = "2025-06-01T10:30:00Z"
"#;
        let file: DecisionFile = toml::from_str(toml_str).expect("deserialize");
        assert!(file.decision.code_refs.is_empty());
    }

    #[test]
    fn empty_code_refs_omitted_in_toml() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            !serialized.contains("code_refs"),
            "empty code_refs should not appear in TOML"
        );
    }

    // ── history ─────────────────────────────────────────────────────────

    #[test]
    fn decision_history_round_trips() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT with DPoP binding".into(),
                reason: "Proof-of-possession prevents replay".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![
                    HistoryEntry {
                        choice: "JWT tokens".into(),
                        reason: "Stateless authentication".into(),
                        changed_at: Utc.with_ymd_and_hms(2025, 7, 15, 14, 0, 0).unwrap(),
                    },
                    HistoryEntry {
                        choice: "JWT with refresh tokens".into(),
                        reason: "Stateless auth with rotation".into(),
                        changed_at: Utc.with_ymd_and_hms(2025, 8, 1, 9, 30, 0).unwrap(),
                    },
                ],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(serialized.contains("[[decision.history]]"));
        let deserialized: DecisionFile = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(file, deserialized);
    }

    #[test]
    fn decision_empty_history_omitted_in_toml() {
        let file = DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT".into(),
                reason: "Stateless".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let serialized = toml::to_string_pretty(&file).expect("serialize");
        assert!(
            !serialized.contains("history"),
            "empty history should not appear in TOML"
        );
    }

    #[test]
    fn decision_without_history_deserializes() {
        let toml_str = r#"
[decision]
component = "auth"
choice = "JWT"
reason = "Stateless"
attribution = "user"
created = "2025-06-01T10:30:00Z"
"#;
        let file: DecisionFile = toml::from_str(toml_str).expect("deserialize");
        assert!(file.decision.history.is_empty());
    }
}
