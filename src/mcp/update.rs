use serde_json::Value;

use crate::store::limits::{MAX_CHOICE_BYTES, MIN_REASON_BYTES};
use crate::store::schema::{Attribution, DecisionFile};
use crate::store::{self, Store};
use crate::workflow::concerns;

use super::write::{opt_str, opt_str_array, parse_code_refs, require_str};

// ── remove_decision ─────────────────────────────────────────────────────────

pub(crate) fn remove_decision(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;

    if !state.decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    // Cascade analysis via the shared graph method.
    let cascade = state.graph().check_decision_cascade(name);

    if cascade.is_blocked() {
        return Ok(serde_json::json!({
            "removed": false,
            "blocked_by": cascade.blockers.iter()
                .map(|b| serde_json::json!({
                    "node": b.node,
                    "edge": b.edge.as_str(),
                    "message": b.message,
                }))
                .collect::<Vec<_>>(),
            "would_warn": cascade.warnings.iter()
                .map(|w| serde_json::json!({
                    "node": w.node,
                    "edge": w.edge.as_str(),
                    "message": w.message,
                }))
                .collect::<Vec<_>>(),
            "would_clean": cascade.cleanups.iter()
                .map(|c| serde_json::json!({
                    "edge": c.edge.as_str(),
                    "target": c.target,
                }))
                .collect::<Vec<_>>(),
        }));
    }

    // Capture the decision before the write path deletes it — its content and
    // component are needed to report the concern coverage the removal erases.
    let removed = std::sync::Arc::clone(&state.decisions[name]);
    let component = removed.decision.component.clone();

    // Execute removal via shared write path.
    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .remove_decision(&lock, state, name)
        .map_err(|e| e.to_string())?;
    drop(lock);

    let coverage_impact = coverage_impact(state, &removed, &component);

    Ok(serde_json::json!({
        "removed": true,
        "blocked_by": [],
        "would_warn": cascade.warnings.iter()
            .map(|w| serde_json::json!({
                "node": w.node,
                "edge": w.edge.as_str(),
                "message": w.message,
            }))
            .collect::<Vec<_>>(),
        "would_clean": cascade.cleanups.iter()
            .map(|c| serde_json::json!({
                "edge": c.edge.as_str(),
                "target": c.target,
            }))
            .collect::<Vec<_>>(),
        "coverage_impact": coverage_impact,
    }))
}

/// Concern coverage delta for the component after a decision is removed.
///
/// `lost_coverage` names areas the removed decision was the last to cover;
/// the remaining counts partition every concern area across the decisions
/// still belonging to the component.
fn coverage_impact(state: &store::ProjectState, removed: &DecisionFile, component: &str) -> Value {
    // Baseline includes project rules, matching get_context — a concern a
    // project-wide rule still covers must not be reported as lost.
    let remaining = state.graph().coverage_baseline(component);

    let lost = concerns::coverage_lost(removed, &remaining);
    let (covered, uncovered) = concerns::compute_concern_coverage(&remaining);

    serde_json::json!({
        "component": component,
        "lost_coverage": lost,
        "remaining_covered": covered.len(),
        "remaining_uncovered": uncovered.len(),
        "total_concerns": concerns::CONCERNS.len(),
    })
}

// ── update_decision ─────────────────────────────────────────────────────────

pub(crate) fn update_decision(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let mode = require_str(args, "mode")?;

    if !state.decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    match mode {
        "revise" => revise_decision(store, state, name, args),
        "promote" => promote_decision(store, state, name),
        other => Err(format!(
            "invalid mode `{other}` — expected: revise, promote"
        )),
    }
}

/// Edit a decision in place, versioning the prior choice and reason into
/// history. The name, `created` timestamp, and every edge survive unchanged.
fn revise_decision(
    store: &Store,
    state: &mut store::ProjectState,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let new_choice = opt_str(args, "choice")?;
    let new_reason = opt_str(args, "reason")?;

    // Distinguish an omitted field from an empty one: a missing `tags`/
    // `code_refs` key leaves the current values intact, while an explicit
    // (possibly empty) array replaces them.
    let new_tags = if args.get("tags").is_some_and(|v| v.is_array()) {
        Some(opt_str_array(args, "tags")?)
    } else {
        None
    };
    let parsed_refs = parse_code_refs(args)?;
    let new_code_refs =
        if parsed_refs.is_empty() && !args.get("code_refs").is_some_and(|v| v.is_array()) {
            None
        } else {
            Some(parsed_refs)
        };

    if new_choice.is_none() && new_reason.is_none() && new_tags.is_none() && new_code_refs.is_none()
    {
        return Err(
            "revise requires at least one of `choice`, `reason`, `tags`, or `code_refs`".into(),
        );
    }

    // Quality floor: revised values must meet the same bar as new decisions.
    if let Some(c) = new_choice
        && c.len() > MAX_CHOICE_BYTES
    {
        return Err(format!(
            "choice must be ≤{MAX_CHOICE_BYTES} characters ({} given)",
            c.len(),
        ));
    }
    if let Some(r) = new_reason
        && r.len() < MIN_REASON_BYTES
    {
        return Err(format!(
            "reason must be at least {MIN_REASON_BYTES} characters ({} given)",
            r.len(),
        ));
    }

    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .revise_decision(
            &lock,
            state,
            name,
            store::ReviseDecisionParams {
                choice: new_choice,
                reason: new_reason,
                tags: new_tags,
                code_refs: new_code_refs,
            },
        )
        .map_err(|e| e.to_string())?;
    drop(lock);

    let history_length = state
        .decisions
        .get(name)
        .map_or(0, |d| d.decision.history.len());

    Ok(serde_json::json!({
        "name": name,
        "revised": true,
        "history_length": history_length,
        "path": store.decision_path(name).display().to_string(),
    }))
}

/// Mark an agent decision as human-reviewed by flipping its attribution to
/// `user`. Rejects decisions already attributed to the user.
fn promote_decision(
    store: &Store,
    state: &mut store::ProjectState,
    name: &str,
) -> Result<Value, String> {
    let already_user = state
        .decisions
        .get(name)
        .is_some_and(|d| d.decision.attribution == Attribution::User);
    if already_user {
        return Err(format!("decision `{name}` already has attribution=user"));
    }

    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .promote_decision(&lock, state, name)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "name": name,
        "promoted": true,
        "attribution": "user",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::write::{record_decision, record_pattern};
    use crate::store::Store;
    use serde_json::json;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Store, store::ProjectState) {
        let tmp = TempDir::new().unwrap();
        let (store, state) = store::testing::setup_store_with_components(
            tmp.path(),
            &[("auth", "Authentication"), ("database", "Database layer")],
        );
        (tmp, store, state)
    }

    // ── remove_decision ─────────────────────────────────────────────────

    #[test]
    fn remove_decision_basic() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], true);
        assert!(!state.decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_dependent() {
        let (_tmp, store, mut state) = setup();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d1).unwrap();

        let d2 = json!({
            "component": "auth", "choice": "Token expiry", "reason": "Fifteen-minute expiry window",
            "depends_on": ["use-jwt"],
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d2).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], false);
        let blocked = result["blocked_by"].as_array().unwrap();
        assert!(!blocked.is_empty());
        assert!(
            blocked
                .iter()
                .any(|b| b["node"] == "token-expiry" && b["edge"] == "depends_on")
        );
        assert!(state.decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_pattern() {
        let (_tmp, store, mut state) = setup();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        let d2 = json!({ "component": "auth", "choice": "Token refresh", "reason": "Token rotation for security", "attribution": "user" });
        record_decision(&store, &mut state, &d1).unwrap();
        record_decision(&store, &mut state, &d2).unwrap();

        let pat = json!({
            "name": "Auth tokens",
            "description": "Token handling",
            "decisions": ["use-jwt", "token-refresh"],
        });
        record_pattern(&store, &mut state, &pat).unwrap();

        // Pattern has exactly 2 members — removing one would violate minimum.
        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["removed"], false);
        let blocked = result["blocked_by"].as_array().unwrap();
        assert!(blocked.iter().any(|b| {
            b["edge"] == "member_of"
                && b["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("fewer than 2"))
        }));
    }

    #[test]
    fn remove_decision_rejects_nonexistent() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "ghost" });
        let err = remove_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn remove_decision_reports_coverage_impact() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "JWT security tokens", "reason": "Authentication boundary protection", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let result = remove_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-security-tokens" }),
        )
        .unwrap();
        assert_eq!(result["removed"], true);

        let impact = &result["coverage_impact"];
        assert_eq!(impact["component"], "auth");
        assert_eq!(impact["total_concerns"], concerns::CONCERNS.len());
        assert_eq!(impact["remaining_uncovered"], concerns::CONCERNS.len());
        assert_eq!(impact["remaining_covered"], 0);

        let lost = impact["lost_coverage"].as_array().unwrap();
        assert!(
            lost.iter().any(|c| c == "Security boundaries"),
            "removing the only security decision must report it as lost: {lost:?}"
        );
    }

    #[test]
    fn remove_decision_no_lost_coverage_when_area_still_covered() {
        let (_tmp, store, mut state) = setup();
        record_decision(
            &store,
            &mut state,
            &json!({ "component": "auth", "choice": "JWT security tokens", "reason": "Authentication boundary protection", "attribution": "user" }),
        )
        .unwrap();
        record_decision(
            &store,
            &mut state,
            &json!({ "component": "auth", "choice": "OAuth delegated flow", "reason": "External identity security provider", "attribution": "user" }),
        )
        .unwrap();

        let result = remove_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-security-tokens" }),
        )
        .unwrap();

        let impact = &result["coverage_impact"];
        let lost = impact["lost_coverage"].as_array().unwrap();
        assert!(
            !lost.iter().any(|c| c == "Security boundaries"),
            "a remaining security decision keeps the area covered: {lost:?}"
        );
        assert_eq!(impact["remaining_covered"], 1);
    }

    #[test]
    fn remove_decision_no_lost_coverage_when_project_rule_covers_area() {
        let (_tmp, store, mut state) = setup();
        // A project-wide security rule covers the same concern as the
        // component's only security decision.
        record_decision(
            &store,
            &mut state,
            &json!({ "component": "project", "choice": "Security boundaries enforced across every module", "reason": "Authentication and authorization security boundary protection", "attribution": "user" }),
        )
        .unwrap();
        record_decision(
            &store,
            &mut state,
            &json!({ "component": "auth", "choice": "JWT security tokens", "reason": "Authentication boundary protection", "attribution": "user" }),
        )
        .unwrap();

        let result = remove_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-security-tokens" }),
        )
        .unwrap();

        let impact = &result["coverage_impact"];
        let lost = impact["lost_coverage"].as_array().unwrap();
        assert!(
            !lost.iter().any(|c| c == "Security boundaries"),
            "a project-wide security rule keeps the area covered: {lost:?}"
        );
        assert!(
            impact["remaining_covered"].as_u64().unwrap() >= 1,
            "the project rule counts toward remaining coverage"
        );
    }

    // ── update_decision: revise ──────────────────────────────────────────

    #[test]
    fn update_decision_revise_choice() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": "Use JWT v2" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");
        assert_eq!(result["revised"], true);

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT v2");
        assert_eq!(dec.decision.reason, "Stateless, no server session"); // unchanged
    }

    #[test]
    fn update_decision_revise_grows_history() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let first = update_decision(
            &store,
            &mut state,
            &json!({ "name": "use-jwt", "mode": "revise", "choice": "Use OAuth" }),
        )
        .unwrap();
        assert_eq!(first["history_length"], 1);

        let second = update_decision(
            &store,
            &mut state,
            &json!({ "name": "use-jwt", "mode": "revise", "reason": "Delegated identity provider" }),
        )
        .unwrap();
        assert_eq!(second["history_length"], 2);

        let dec = state.decisions.get("use-jwt").unwrap();
        // Oldest first: history traces the decision's evolution.
        assert_eq!(dec.decision.history[0].choice, "Use JWT");
        assert_eq!(dec.decision.history[1].choice, "Use OAuth");
    }

    #[test]
    fn update_decision_revise_reason() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "reason": "Better reason text" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT"); // unchanged
        assert_eq!(dec.decision.reason, "Better reason text");
    }

    #[test]
    fn update_decision_revise_preserves_timestamp() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();
        let original_ts = state.decisions["use-jwt"].decision.created;

        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": "JWT v2" });
        update_decision(&store, &mut state, &args).unwrap();

        assert_eq!(state.decisions["use-jwt"].decision.created, original_ts);
    }

    #[test]
    fn update_decision_revise_rejects_no_changes() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn update_decision_rejects_invalid_mode() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "X", "reason": "test reason placeholder", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "x", "mode": "delete" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("invalid mode"));
    }

    #[test]
    fn update_decision_rejects_legacy_amend_mode() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "X" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("invalid mode"),
            "amend is no longer a mode: {err}"
        );
    }

    #[test]
    fn update_decision_rejects_legacy_supersede_mode() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "supersede", "choice": "X" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("invalid mode"),
            "supersede is no longer a mode: {err}"
        );
    }

    #[test]
    fn update_decision_rejects_nonexistent() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "ghost", "mode": "revise", "choice": "X" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    // ── update_decision: promote ─────────────────────────────────────────

    #[test]
    fn update_decision_promote_flips_agent_to_user() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "agent" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "promote" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["promoted"], true);
        assert_eq!(result["attribution"], "user");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.attribution, Attribution::User);
    }

    #[test]
    fn update_decision_promote_rejects_user() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "promote" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("already"), "{err}");
    }

    #[test]
    fn update_decision_promote_leaves_no_history() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "agent" });
        record_decision(&store, &mut state, &d).unwrap();

        update_decision(
            &store,
            &mut state,
            &json!({ "name": "use-jwt", "mode": "promote" }),
        )
        .unwrap();

        assert!(state.decisions["use-jwt"].decision.history.is_empty());
    }

    // ── full decision lifecycle ───────────────────────────────────────

    #[test]
    fn record_revise_history_promote_lifecycle() {
        let (_tmp, store, mut state) = setup();

        // Record an agent decision.
        let d = json!({ "component": "auth", "choice": "JWT tokens", "reason": "Stateless authentication", "attribution": "agent" });
        record_decision(&store, &mut state, &d).unwrap();

        // Two substantive revisions grow the history chain.
        let first = update_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-tokens", "mode": "revise", "choice": "JWT with refresh tokens" }),
        )
        .unwrap();
        assert_eq!(first["history_length"], 1);

        let second = update_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-tokens", "mode": "revise", "choice": "JWT with DPoP binding", "reason": "Proof-of-possession prevents token replay" }),
        )
        .unwrap();
        assert_eq!(second["history_length"], 2);

        // get_decision_history exposes the full chronological chain.
        let history = crate::mcp::context::get_decision_history(&state, "jwt-tokens").unwrap();
        assert_eq!(history["current"]["choice"], "JWT with DPoP binding");
        assert_eq!(history["revision_count"], 2);
        let entries = history["history"].as_array().unwrap();
        assert_eq!(entries[0]["choice"], "JWT tokens");
        assert_eq!(entries[1]["choice"], "JWT with refresh tokens");

        // Promote flips the surviving decision from agent to user.
        let promoted = update_decision(
            &store,
            &mut state,
            &json!({ "name": "jwt-tokens", "mode": "promote" }),
        )
        .unwrap();
        assert_eq!(promoted["attribution"], "user");
        assert_eq!(
            state.decisions["jwt-tokens"].decision.attribution,
            Attribution::User
        );
        // History survives the promotion untouched.
        assert_eq!(state.decisions["jwt-tokens"].decision.history.len(), 2);
    }

    // ── no workflow hints ─────────────────────────────────────────────

    #[test]
    fn remove_decision_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let result = remove_decision(&store, &mut state, &json!({ "name": "use-jwt" })).unwrap();
        assert_eq!(result["removed"], true);
        assert!(result.get("workflow").is_none());
    }

    #[test]
    fn revise_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": "Use JWT v2" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
    }

    // ── revise quality floor ──────────────────────────────────────────

    #[test]
    fn revise_rejects_short_reason() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "reason": "ok" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("at least") && err.contains("characters"),
            "revise should enforce quality floor: {err}"
        );
    }

    #[test]
    fn revise_rejects_long_choice() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let long = "x".repeat(201);
        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": long });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("200"),
            "revise should enforce choice length: {err}"
        );
    }

    // ── revise: tags and code_refs ────────────────────────────────────

    #[test]
    fn revise_tags_leaves_no_history() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "tags": ["security", "auth"] });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["history_length"], 0);

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.tags, vec!["security", "auth"]);
        assert!(dec.decision.history.is_empty());
    }

    #[test]
    fn revise_code_refs() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [
                { "file": "src/auth/jwt.rs", "symbol": "verify" },
            ],
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/auth/jwt.rs");
    }

    #[test]
    fn revise_code_refs_replaces_existing() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/old.rs" }],
        });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [{ "file": "src/new.rs" }],
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/new.rs");
    }

    #[test]
    fn revise_empty_code_refs_clears() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/old.rs" }],
        });
        record_decision(&store, &mut state, &d).unwrap();
        assert_eq!(state.decisions["use-jwt"].decision.code_refs.len(), 1);

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [],
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert!(dec.decision.code_refs.is_empty());
    }

    #[test]
    fn revise_without_code_refs_key_preserves_existing() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/keep.rs", "symbol": "verify" }],
        });
        record_decision(&store, &mut state, &d).unwrap();

        // Revise an unrelated field — omitting code_refs must leave refs intact.
        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "reason": "Stateless, avoids a server-side session store",
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/keep.rs");
    }
}
