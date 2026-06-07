use chrono::Utc;
use serde_json::Value;

use crate::Result;
use crate::commands;
use crate::store::schema::{Decision, DecisionFile, EdgeEntry, EdgeKind, NodeEntry, NodeKind};
use crate::store::{self, Store};

// ── Extraction ──────────────────────────────────────────────────────────────

pub(crate) struct ExtractedDecision {
    pub choice: String,
    pub reason: String,
    pub alternatives: Vec<String>,
}

pub(crate) fn extract_decisions(response: &str) -> Vec<ExtractedDecision> {
    let mut decisions = Vec::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
            if let (Some(choice), Some(reason)) = (
                json.get("choice").and_then(|v| v.as_str()),
                json.get("reason").and_then(|v| v.as_str()),
            ) {
                if !choice.is_empty() && !reason.is_empty() {
                    let alternatives = json
                        .get("alternatives")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();

                    decisions.push(ExtractedDecision {
                        choice: choice.to_string(),
                        reason: reason.to_string(),
                        alternatives,
                    });
                }
            }
        }
    }

    decisions
}

pub(crate) fn is_design_complete(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.trim() == "DESIGN_COMPLETE")
}

// ── Recording ───────────────────────────────────────────────────────────────

/// Write a single decision to the store, with full validation.
/// Acquires the store lock **before** deriving the filename stem and
/// validating, closing the TOCTOU window between validation and write.
/// Uses the caller's cached [`ProjectState`] — no re-load from disk.
/// On success, `state` is updated in-place so subsequent calls see
/// the new decision. On failure, `state` is rolled back.
pub(crate) fn record_decision(
    store: &Store,
    state: &mut store::ProjectState,
    component: &str,
    choice: &str,
    reason: &str,
    alternatives: &[String],
) -> Result<String> {
    // Acquire lock FIRST — prevents concurrent writes between stem
    // derivation / validation and the actual disk write.
    let lock = store.lock()?;

    let stem = commands::unique_decision_stem(&state.decisions, &commands::slugify(choice))?;

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: alternatives.to_vec(),
            created: Utc::now(),
        },
    };

    let write = store.prepare_write(&store.decision_path(&stem), &decision)?;
    let hash = write.content_hash();

    // Snapshot graph index for rollback.
    let graph_snapshot = state.graph_index.clone();

    // Add node and BelongsTo edge to graph index.
    state.graph_index.nodes.push(NodeEntry {
        name: stem.clone(),
        kind: NodeKind::Decision,
        tags: vec![],
        hash,
    });
    state.graph_index.edges.push(EdgeEntry {
        from: stem.clone(),
        to: component.into(),
        kind: EdgeKind::BelongsTo,
    });

    // Insert into in-memory state for validation.
    state.decisions.insert(stem.clone(), decision);

    if let Err(e) = store.commit_with_graph(&lock, vec![write], vec![], state) {
        state.decisions.remove(&stem);
        state.graph_index = graph_snapshot;
        return Err(e);
    }

    // Refresh the cached InMemoryGraph so subsequent calls within the
    // same session see the decision we just committed. Without this,
    // multi-decision design sessions validate against a stale graph.
    state.rebuild_graph();

    Ok(stem)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_decisions ────────────────────────────────────────────────

    #[test]
    fn extracts_decision_json() {
        let response = "Great question!\n\
            {\"choice\": \"Use JWT\", \"reason\": \"Stateless auth\"}\n\
            Next, let's talk about storage.";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].choice, "Use JWT");
        assert_eq!(decisions[0].reason, "Stateless auth");
        assert!(decisions[0].alternatives.is_empty());
    }

    #[test]
    fn extracts_decision_with_alternatives() {
        let response = "{\"choice\": \"Redis\", \"reason\": \"Persistent and fast\", \
            \"alternatives\": [\"Memcached — rejected: no persistence\", \
            \"In-memory — rejected: lost on restart\"]}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].choice, "Redis");
        assert_eq!(decisions[0].alternatives.len(), 2);
        assert!(decisions[0].alternatives[0].contains("Memcached"));
        assert!(decisions[0].alternatives[1].contains("In-memory"));
    }

    #[test]
    fn extracts_multiple_decisions() {
        let response = "{\"choice\": \"A\", \"reason\": \"R1\"}\ntext\n\
            {\"choice\": \"B\", \"reason\": \"R2\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn ignores_non_decision_json() {
        let response = "{\"type\": \"greeting\", \"text\": \"hello\"}";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn ignores_plain_text() {
        let response = "What token format will you use?";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn ignores_empty_choice_or_reason() {
        let response = "{\"choice\": \"\", \"reason\": \"something\"}";
        assert!(extract_decisions(response).is_empty());
    }

    #[test]
    fn handles_whitespace_around_json() {
        let response = "  {\"choice\": \"X\", \"reason\": \"Y\"}  ";
        assert_eq!(extract_decisions(response).len(), 1);
    }

    // ── is_design_complete ──────────────────────────────────────────────

    #[test]
    fn detects_completion() {
        assert!(is_design_complete("all done\nDESIGN_COMPLETE\n"));
        assert!(is_design_complete("DESIGN_COMPLETE"));
        assert!(is_design_complete("  DESIGN_COMPLETE  "));
    }

    #[test]
    fn no_false_completion() {
        assert!(!is_design_complete("the DESIGN_COMPLETE flag"));
        assert!(!is_design_complete("almost done"));
    }

    // ── record_decision ─────────────────────────────────────────────────

    #[test]
    fn record_decision_refreshes_graph_cache() {
        use crate::commands;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        commands::init(tmp.path()).unwrap();
        commands::add_component(tmp.path(), "auth", None).unwrap();

        let store = store::Store::discover(tmp.path()).unwrap();
        let mut state = store.load_state().unwrap();

        // Record first decision.
        let stem1 =
            record_decision(&store, &mut state, "auth", "Use JWT", "Stateless", &[]).unwrap();
        assert_eq!(stem1, "use-jwt");

        // The cached graph must reflect the new decision — otherwise the
        // second call would validate against a stale graph.
        assert!(state.graph.decision(&stem1).is_some());
        assert_eq!(state.graph.decisions_for("auth").len(), 1);

        // Record second decision in the same session.
        let stem2 = record_decision(
            &store,
            &mut state,
            "auth",
            "Token expiry 15min",
            "Short-lived tokens reduce theft window",
            &[],
        )
        .unwrap();

        assert!(state.graph.decision(&stem2).is_some());
        assert_eq!(state.graph.decisions_for("auth").len(), 2);

        // Both decisions and their BelongsTo edges must be present.
        let edge_count = state
            .graph_index
            .edges
            .iter()
            .filter(|e| e.to == "auth" && e.kind == EdgeKind::BelongsTo)
            .count();
        assert_eq!(edge_count, 2);
    }

    #[test]
    fn record_decision_rolls_back_on_invalid_component() {
        use crate::commands;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        commands::init(tmp.path()).unwrap();

        let store = store::Store::discover(tmp.path()).unwrap();
        let mut state = store.load_state().unwrap();

        let node_count_before = state.graph_index.nodes.len();
        let edge_count_before = state.graph_index.edges.len();

        // Component "ghost" doesn't exist — commit_with_graph will reject.
        let result = record_decision(
            &store,
            &mut state,
            "ghost",
            "Bad decision",
            "Should fail",
            &[],
        );

        assert!(result.is_err());
        assert_eq!(state.graph_index.nodes.len(), node_count_before);
        assert_eq!(state.graph_index.edges.len(), edge_count_before);
        assert!(state.decisions.is_empty());
    }
}
