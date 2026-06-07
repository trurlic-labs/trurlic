use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── project.toml ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFile {
    pub trurl_version: String,

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

    /// When this decision was recorded (UTC, ISO 8601 / RFC 3339).
    pub created: DateTime<Utc>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    BelongsTo,
    ConnectsTo,
    DependsOn,
    Constrains,
    Supersedes,
    MemberOf,
    AppliesTo,
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

pub const FORMAT_VERSION: &str = "0.2.0";

pub const STORE_DIR: &str = ".trurl";

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
            trurl_version: "0.2.0".into(),
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
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
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
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
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
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
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
            EdgeKind::Supersedes,
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
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
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
"#;
        let result = toml::from_str::<DecisionFile>(toml_str);
        assert!(result.is_err(), "missing created field must be rejected");
    }
}
