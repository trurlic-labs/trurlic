use serde_json::Value;

use crate::store::graph::Direction;
use crate::store::limits::{MAX_CHOICE_BYTES, MIN_REASON_BYTES};
use crate::store::schema::EdgeKind;
use crate::store::{self, Store};

use super::write::{opt_str, parse_code_refs, record_decision, require_str};
use crate::store::schema::Attribution;

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

    // Execute removal via shared write path.
    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .remove_decision(&lock, state, name)
        .map_err(|e| e.to_string())?;

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
    }))
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
        "amend" => amend_decision(store, state, name, args),
        "supersede" => supersede_decision(store, state, name, args),
        _ => Err(format!(
            "invalid mode `{mode}` — expected \"amend\" or \"supersede\""
        )),
    }
}

/// Small correction: edit the file in place. `created` timestamp unchanged.
fn amend_decision(
    store: &Store,
    state: &mut store::ProjectState,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let new_choice = opt_str(args, "choice")?;
    let new_reason = opt_str(args, "reason")?;
    let new_code_refs = parse_code_refs(args)?;
    let code_refs_param =
        if new_code_refs.is_empty() && !args.get("code_refs").is_some_and(|v| v.is_array()) {
            None
        } else {
            Some(new_code_refs)
        };

    if new_choice.is_none() && new_reason.is_none() && code_refs_param.is_none() {
        return Err("amend requires at least one of `choice`, `reason`, or `code_refs`".into());
    }

    // Quality floor: amended values must meet the same bar as new decisions.
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
        .amend_decision(
            &lock,
            state,
            name,
            store::AmendDecisionParams {
                choice: new_choice,
                reason: new_reason,
                tags: None,
                code_refs: code_refs_param.as_deref(),
            },
        )
        .map_err(|e| e.to_string())?;

    // Collect affected patterns and decisions.
    let (affected_patterns, affected_decisions) = collect_affected(state.graph(), name);

    Ok(serde_json::json!({
        "name": name,
        "path": store.decision_path(name).display().to_string(),
        "affected_patterns": affected_patterns,
        "affected_decisions": affected_decisions,
    }))
}

/// Substantive change: create a new decision that supersedes the old one.
fn supersede_decision(
    store: &Store,
    state: &mut store::ProjectState,
    old_name: &str,
    args: &Value,
) -> Result<Value, String> {
    let old_dec = state
        .decisions
        .get(old_name)
        .ok_or_else(|| format!("decision `{old_name}` does not exist"))?;

    let new_choice = opt_str(args, "choice")?;
    let new_reason = opt_str(args, "reason")?;

    if new_choice.is_none() && new_reason.is_none() {
        return Err("supersede requires at least one of `choice` or `reason`".into());
    }

    let choice = new_choice.unwrap_or(&old_dec.decision.choice);
    let reason = new_reason.unwrap_or(&old_dec.decision.reason);

    let component = old_dec.decision.component.clone();
    let tags = old_dec.decision.tags.clone();
    let old_code_refs = old_dec.decision.code_refs.clone();
    let attribution = match old_dec.decision.attribution {
        Attribution::User => "user",
        Attribution::Agent => "agent",
    };

    // Resolve code_refs: inherit from old decision, override if explicitly provided.
    let new_code_refs = parse_code_refs(args)?;
    let resolved_refs =
        if new_code_refs.is_empty() && !args.get("code_refs").is_some_and(|v| v.is_array()) {
            store::code_refs_to_json(&old_code_refs)
        } else {
            store::code_refs_to_json(&new_code_refs)
        };

    // Delegate to record_decision with supersedes set.
    let record_args = serde_json::json!({
        "component": component,
        "choice": choice,
        "reason": reason,
        "supersedes": old_name,
        "tags": tags,
        "attribution": attribution,
        "code_refs": resolved_refs,
    });

    let result = record_decision(store, state, &record_args)?;
    let new_name = result["name"].as_str().unwrap_or_default();

    // Collect affected info from the OLD decision.
    let (affected_patterns, affected_decisions) = collect_affected(state.graph(), old_name);

    Ok(serde_json::json!({
        "name": new_name,
        "superseded": old_name,
        "path": result["path"],
        "affected_patterns": affected_patterns,
        "affected_decisions": affected_decisions,
    }))
}

/// Collect pattern and decision names affected by edges involving a decision.
fn collect_affected(
    graph: &crate::store::graph::InMemoryGraph,
    decision: &str,
) -> (Vec<String>, Vec<String>) {
    let involved = graph.edges_involving(decision);

    let patterns: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| e.kind == EdgeKind::MemberOf && *d == Direction::Reverse)
        .map(|(other, _, _)| other.to_string())
        .collect();

    let decisions: Vec<String> = involved
        .iter()
        .filter(|(_, e, d)| {
            matches!(
                e.kind,
                EdgeKind::DependsOn | EdgeKind::Constrains | EdgeKind::Supersedes
            ) && *d == Direction::Reverse
        })
        .map(|(other, _, _)| other.to_string())
        .collect();

    (patterns, decisions)
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

    // ── update_decision ─────────────────────────────────────────────────

    #[test]
    fn update_decision_amend_choice() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "Use JWT v2" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT v2");
        assert_eq!(dec.decision.reason, "Stateless, no server session"); // unchanged
    }

    #[test]
    fn update_decision_amend_reason() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "reason": "Better reason" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT"); // unchanged
        assert_eq!(dec.decision.reason, "Better reason");
    }

    #[test]
    fn update_decision_amend_preserves_timestamp() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();
        let original_ts = state.decisions["use-jwt"].decision.created;

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "JWT v2" });
        update_decision(&store, &mut state, &args).unwrap();

        assert_eq!(state.decisions["use-jwt"].decision.created, original_ts);
    }

    #[test]
    fn update_decision_amend_rejects_no_changes() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn update_decision_supersede_creates_new() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Session cookies", "reason": "Simple session-based model", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "session-cookies",
            "mode": "supersede",
            "choice": "JWT tokens",
            "reason": "Stateless, no server session",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();
        assert_ne!(new_name, "session-cookies");

        // Old decision still exists.
        assert!(state.decisions.contains_key("session-cookies"));
        // New decision exists.
        assert!(state.decisions.contains_key(new_name));
        // Supersedes edge exists.
        assert!(state.graph_index.edges.iter().any(|e| e.from == new_name
            && e.to == "session-cookies"
            && e.kind == EdgeKind::Supersedes));
    }

    #[test]
    fn update_decision_supersede_inherits_component() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "supersede",
            "choice": "Use PASETO",
            "reason": "Better defaults",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();
        let new_dec = state.decisions.get(new_name).unwrap();
        assert_eq!(new_dec.decision.component, "auth");
    }

    #[test]
    fn update_decision_supersede_carries_tags() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "tags": ["security", "auth"],
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d).unwrap();
        assert_eq!(
            state.decisions["use-jwt"].decision.tags,
            vec!["security", "auth"]
        );

        let args = json!({
            "name": "use-jwt",
            "mode": "supersede",
            "choice": "Use PASETO",
            "reason": "Better defaults",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();
        let new_dec = state.decisions.get(new_name).unwrap();
        assert_eq!(
            new_dec.decision.tags,
            vec!["security", "auth"],
            "supersede must carry tags forward"
        );
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
    fn update_decision_rejects_nonexistent() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "ghost", "mode": "amend", "choice": "X" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
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
    fn amend_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "Use JWT v2" });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
    }

    #[test]
    fn supersede_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "supersede",
            "choice": "Use PASETO",
            "reason": "Better defaults",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
        assert!(result.get("superseded").is_some());
    }

    // ── amend quality floor ───────────────────────────────────────────

    #[test]
    fn amend_rejects_short_reason() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "reason": "ok" });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("at least") && err.contains("characters"),
            "amend should enforce quality floor: {err}"
        );
    }

    #[test]
    fn amend_rejects_long_choice() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&store, &mut state, &d).unwrap();

        let long = "x".repeat(201);
        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": long });
        let err = update_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("200"),
            "amend should enforce choice length: {err}"
        );
    }

    // ── code_refs ─────────────────────────────────────────────────────

    #[test]
    fn amend_code_refs() {
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
            "mode": "amend",
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
    fn amend_code_refs_replaces_existing() {
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
            "mode": "amend",
            "code_refs": [{ "file": "src/new.rs" }],
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/new.rs");
    }

    #[test]
    fn amend_empty_code_refs_clears() {
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
            "mode": "amend",
            "code_refs": [],
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert!(dec.decision.code_refs.is_empty());
    }

    #[test]
    fn amend_without_code_refs_key_preserves_existing() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/keep.rs", "symbol": "verify" }],
        });
        record_decision(&store, &mut state, &d).unwrap();

        // Amend an unrelated field — omitting code_refs must leave refs intact.
        let args = json!({
            "name": "use-jwt",
            "mode": "amend",
            "reason": "Stateless, avoids a server-side session store",
        });
        update_decision(&store, &mut state, &args).unwrap();

        let dec = state.decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/keep.rs");
    }

    #[test]
    fn supersede_inherits_code_refs() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Session cookies",
            "reason": "Simple session-based model",
            "attribution": "user",
            "code_refs": [
                { "file": "src/auth/session.rs", "symbol": "create_session" },
            ],
        });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "session-cookies",
            "mode": "supersede",
            "choice": "JWT tokens",
            "reason": "Stateless, no server session",
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();

        let new_dec = state.decisions.get(new_name).unwrap();
        assert_eq!(
            new_dec.decision.code_refs.len(),
            1,
            "supersede must inherit code_refs"
        );
        assert_eq!(new_dec.decision.code_refs[0].file, "src/auth/session.rs");
    }

    #[test]
    fn supersede_explicit_code_refs_override() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Session cookies",
            "reason": "Simple session-based model",
            "attribution": "user",
            "code_refs": [{ "file": "src/old.rs" }],
        });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "session-cookies",
            "mode": "supersede",
            "choice": "JWT tokens",
            "reason": "Stateless, no server session",
            "code_refs": [{ "file": "src/new.rs", "symbol": "verify" }],
        });
        let result = update_decision(&store, &mut state, &args).unwrap();
        let new_name = result["name"].as_str().unwrap();

        let new_dec = state.decisions.get(new_name).unwrap();
        assert_eq!(new_dec.decision.code_refs.len(), 1);
        assert_eq!(new_dec.decision.code_refs[0].file, "src/new.rs");
    }
}
