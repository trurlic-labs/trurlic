//! Decision extraction from LLM responses and recording to the store.

use chrono::Utc;
use serde_json::Value;

use crate::Result;
use crate::commands;
use crate::store::schema::{Decision, DecisionFile};
use crate::store::{self, Store};

// ── Extraction ──────────────────────────────────────────────────────────────

/// A decision parsed from an LLM response line.
pub(crate) struct ExtractedDecision {
    pub choice: String,
    pub reason: String,
    pub alternatives: Vec<String>,
    pub supersedes: Option<String>,
}

/// Extract decision JSON objects from an LLM response.
///
/// Looks for lines containing `{"choice": "...", "reason": "..."}` with
/// optional `"alternatives"` array and `"supersedes"` string (revisit mode).
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

                    let supersedes = json
                        .get("supersedes")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from);

                    decisions.push(ExtractedDecision {
                        choice: choice.to_string(),
                        reason: reason.to_string(),
                        alternatives,
                        supersedes,
                    });
                }
            }
        }
    }

    decisions
}

/// Check if the response signals design completion.
pub(crate) fn is_design_complete(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.trim() == "DESIGN_COMPLETE")
}

// ── Recording ───────────────────────────────────────────────────────────────

/// Write a single decision to the store, with full validation.
///
/// Uses the caller's cached [`ProjectState`] — no re-load from disk.
/// On success, `state` is updated in-place so subsequent calls see
/// the new decision. On failure, `state` is rolled back.
///
/// If `supersedes` names a decision that doesn't exist (LLM hallucination),
/// it is dropped with a warning rather than failing the entire write —
/// the choice and reason are still valuable.
pub(crate) fn record_decision(
    store: &Store,
    state: &mut store::ProjectState,
    component: &str,
    choice: &str,
    reason: &str,
    alternatives: &[String],
    supersedes: Option<&str>,
) -> Result<String> {
    // Validate supersedes target — warn and drop if the LLM hallucinated
    let validated_supersedes = match supersedes {
        Some(target) if state.decisions.contains_key(target) => Some(target.to_string()),
        Some(target) => {
            eprintln!("  ⚠ ignoring supersedes `{target}` — decision not found");
            None
        }
        None => None,
    };

    let stem = commands::unique_decision_stem(&state.decisions, &commands::slugify(choice));

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: alternatives.to_vec(),
            created: Utc::now(),
            supersedes: validated_supersedes,
        },
    };

    // Insert into in-memory state for validation
    state.decisions.insert(stem.clone(), decision.clone());

    if let Err(e) = commands::validate_mutation(state) {
        state.decisions.remove(&stem);
        return Err(e);
    }

    // Acquire lock only for the write, release immediately after
    let lock = store.lock()?;
    if let Err(e) = store.write_atomic(&lock, &store.decision_path(&stem), &decision) {
        state.decisions.remove(&stem);
        return Err(e);
    }

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
        assert!(decisions[0].supersedes.is_none());
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
        assert!(decisions[0].supersedes.is_none());
    }

    #[test]
    fn extracts_decision_with_supersedes() {
        let response = "{\"choice\": \"Session cookies\", \"reason\": \"Simpler\", \
            \"supersedes\": \"auth-token-format\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].supersedes.as_deref(),
            Some("auth-token-format")
        );
    }

    #[test]
    fn extracts_decision_ignores_empty_supersedes() {
        let response = "{\"choice\": \"X\", \"reason\": \"Y\", \"supersedes\": \"\"}";
        let decisions = extract_decisions(response);
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].supersedes.is_none());
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
}
