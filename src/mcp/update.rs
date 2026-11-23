use serde_json::Value;

use crate::store::cascade::CascadeResult;
use crate::store::schema::Attribution;
use crate::store::{self, Writer};

use super::write::{opt_str_array, opt_text, parse_code_refs, require_str};

/// Cascade analysis → JSON arrays for MCP tool responses.
///
/// Private to this module: the CLI reports cascade results as messages
/// only (`blocker_summary`), so a shared helper in `store::cascade` would
/// go unused outside MCP.
fn cascade_to_json(cascade: &CascadeResult) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let blocked_by = cascade
        .blockers
        .iter()
        .map(|b| {
            serde_json::json!({
                "node": b.node,
                "edge": b.edge.as_str(),
                "message": b.message,
            })
        })
        .collect();
    let would_warn = cascade
        .warnings
        .iter()
        .map(|w| {
            serde_json::json!({
                "node": w.node,
                "edge": w.edge.as_str(),
                "message": w.message,
            })
        })
        .collect();
    let would_clean = cascade
        .cleanups
        .iter()
        .map(|c| {
            serde_json::json!({
                "edge": c.edge.as_str(),
                "target": c.target,
            })
        })
        .collect();
    (blocked_by, would_warn, would_clean)
}

// ── remove_decision ─────────────────────────────────────────────────────────

pub(crate) fn remove_decision(writer: &mut Writer<'_>, args: &Value) -> Result<Value, String> {
    let name = require_str(args, "name")?;

    if !writer.state().decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    // Cascade analysis via the shared graph method.
    let cascade = writer.state().graph().check_decision_cascade(name);
    let (blocked_by, would_warn, would_clean) = cascade_to_json(&cascade);

    if cascade.is_blocked() {
        return Ok(serde_json::json!({
            "removed": false,
            "blocked_by": blocked_by,
            "would_warn": would_warn,
            "would_clean": would_clean,
        }));
    }

    writer.remove_decision(name).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "removed": true,
        "blocked_by": blocked_by,
        "would_warn": would_warn,
        "would_clean": would_clean,
    }))
}

// ── update_decision ─────────────────────────────────────────────────────────

pub(crate) fn update_decision(writer: &mut Writer<'_>, args: &Value) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let mode = require_str(args, "mode")?;

    if !writer.state().decisions.contains_key(name) {
        return Err(format!("decision `{name}` does not exist"));
    }

    match mode {
        "revise" => revise_decision(writer, name, args),
        "promote" => promote_decision(writer, name),
        other => Err(format!(
            "invalid mode `{other}` — expected: revise, promote"
        )),
    }
}

/// Edit a decision in place, versioning the prior choice and reason into
/// history. The name, `created` timestamp, and every edge survive unchanged.
fn revise_decision(writer: &mut Writer<'_>, name: &str, args: &Value) -> Result<Value, String> {
    let new_choice = opt_text(args, "choice")?;
    let new_reason = opt_text(args, "reason")?;

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

    writer
        .revise_decision(
            name,
            store::ReviseDecisionParams {
                choice: new_choice,
                reason: new_reason,
                tags: new_tags.as_deref(),
                code_refs: new_code_refs.as_deref(),
            },
        )
        .map_err(|e| e.to_string())?;

    let history_length = writer
        .state()
        .decisions
        .get(name)
        .map_or(0, |d| d.decision.history.len());

    Ok(serde_json::json!({
        "name": name,
        "revised": true,
        "history_length": history_length,
        "path": writer.store().decision_path(name).display().to_string(),
    }))
}

/// Mark an agent decision as human-reviewed by flipping its attribution to
/// `user`. Rejects decisions already attributed to the user.
fn promote_decision(writer: &mut Writer<'_>, name: &str) -> Result<Value, String> {
    let already_user = writer
        .state()
        .decisions
        .get(name)
        .is_some_and(|d| d.decision.attribution == Attribution::User);
    if already_user {
        return Err(format!("decision `{name}` already has attribution=user"));
    }

    writer.promote_decision(name).map_err(|e| e.to_string())?;

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

    fn setup() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let (store, _) = store::testing::setup_store_with_components(
            tmp.path(),
            &[("auth", "Authentication"), ("database", "Database layer")],
        );
        (tmp, store)
    }

    // ── remove_decision ─────────────────────────────────────────────────

    #[test]
    fn remove_decision_basic() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&mut writer, &args).unwrap();
        assert_eq!(result["removed"], true);
        let keys: Vec<&str> = result
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["blocked_by", "removed", "would_clean", "would_warn"]);
        assert!(!writer.state().decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_dependent() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d1).unwrap();

        let d2 = json!({
            "component": "auth", "choice": "Token expiry", "reason": "Fifteen-minute expiry window",
            "depends_on": ["use-jwt"],
            "attribution": "user",
        });
        record_decision(&mut writer, &d2).unwrap();

        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&mut writer, &args).unwrap();
        assert_eq!(result["removed"], false);
        let blocked = result["blocked_by"].as_array().unwrap();
        assert!(!blocked.is_empty());
        assert!(
            blocked
                .iter()
                .any(|b| b["node"] == "token-expiry" && b["edge"] == "depends_on")
        );
        assert!(writer.state().decisions.contains_key("use-jwt"));
    }

    #[test]
    fn remove_decision_blocked_by_pattern() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();

        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        let d2 = json!({ "component": "auth", "choice": "Token refresh", "reason": "Token rotation for security", "attribution": "user" });
        record_decision(&mut writer, &d1).unwrap();
        record_decision(&mut writer, &d2).unwrap();

        let pat = json!({
            "name": "Auth tokens",
            "description": "Token handling",
            "decisions": ["use-jwt", "token-refresh"],
        });
        record_pattern(&mut writer, &pat).unwrap();

        // Pattern has exactly 2 members — removing one would violate minimum.
        let args = json!({ "name": "use-jwt" });
        let result = remove_decision(&mut writer, &args).unwrap();
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
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let args = json!({ "name": "ghost" });
        let err = remove_decision(&mut writer, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    // ── update_decision: revise ──────────────────────────────────────────

    #[test]
    fn update_decision_revise_choice() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": "Use JWT v2" });
        let result = update_decision(&mut writer, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");
        assert_eq!(result["revised"], true);

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT v2");
        assert_eq!(dec.decision.reason, "Stateless, no server session"); // unchanged
    }

    #[test]
    fn update_decision_revise_grows_history() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let first = update_decision(
            &mut writer,
            &json!({ "name": "use-jwt", "mode": "revise", "choice": "Use OAuth" }),
        )
        .unwrap();
        assert_eq!(first["history_length"], 1);

        let second = update_decision(
            &mut writer,
            &json!({ "name": "use-jwt", "mode": "revise", "reason": "Delegated identity provider" }),
        )
        .unwrap();
        assert_eq!(second["history_length"], 2);

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        // Oldest first: history traces the decision's evolution.
        assert_eq!(dec.decision.history[0].choice, "Use JWT");
        assert_eq!(dec.decision.history[1].choice, "Use OAuth");
    }

    #[test]
    fn update_decision_revise_reason() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "reason": "Better reason text" });
        let result = update_decision(&mut writer, &args).unwrap();
        assert_eq!(result["name"], "use-jwt");

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.choice, "Use JWT"); // unchanged
        assert_eq!(dec.decision.reason, "Better reason text");
    }

    #[test]
    fn update_decision_revise_preserves_timestamp() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();
        let original_ts = writer.state().decisions["use-jwt"].decision.created;

        let args = json!({ "name": "use-jwt", "mode": "revise", "choice": "JWT v2" });
        update_decision(&mut writer, &args).unwrap();

        assert_eq!(
            writer.state().decisions["use-jwt"].decision.created,
            original_ts
        );
    }

    #[test]
    fn update_decision_revise_rejects_no_changes() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn update_decision_rejects_invalid_mode() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "X", "reason": "test reason placeholder", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "x", "mode": "delete" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(err.contains("invalid mode"));
    }

    #[test]
    fn update_decision_rejects_legacy_amend_mode() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "amend", "choice": "X" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(
            err.contains("invalid mode"),
            "amend is no longer a mode: {err}"
        );
    }

    #[test]
    fn update_decision_rejects_legacy_supersede_mode() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "supersede", "choice": "X" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(
            err.contains("invalid mode"),
            "supersede is no longer a mode: {err}"
        );
    }

    #[test]
    fn update_decision_rejects_nonexistent() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let args = json!({ "name": "ghost", "mode": "revise", "choice": "X" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    // ── update_decision: promote ─────────────────────────────────────────

    #[test]
    fn update_decision_promote_flips_agent_to_user() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "agent" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "promote" });
        let result = update_decision(&mut writer, &args).unwrap();
        assert_eq!(result["promoted"], true);
        assert_eq!(result["attribution"], "user");

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.attribution, Attribution::User);
    }

    #[test]
    fn update_decision_promote_rejects_user() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "promote" });
        let err = update_decision(&mut writer, &args).unwrap_err();
        assert!(err.contains("already"), "{err}");
    }

    #[test]
    fn update_decision_promote_leaves_no_history() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "agent" });
        record_decision(&mut writer, &d).unwrap();

        update_decision(
            &mut writer,
            &json!({ "name": "use-jwt", "mode": "promote" }),
        )
        .unwrap();

        assert!(
            writer.state().decisions["use-jwt"]
                .decision
                .history
                .is_empty()
        );
    }

    // ── full decision lifecycle ───────────────────────────────────────

    #[test]
    fn record_revise_history_promote_lifecycle() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();

        // Record an agent decision.
        let d = json!({ "component": "auth", "choice": "JWT tokens", "reason": "Stateless authentication", "attribution": "agent" });
        record_decision(&mut writer, &d).unwrap();

        // Two substantive revisions grow the history chain.
        let first = update_decision(
            &mut writer,
            &json!({ "name": "jwt-tokens", "mode": "revise", "choice": "JWT with refresh tokens" }),
        )
        .unwrap();
        assert_eq!(first["history_length"], 1);

        let second = update_decision(
            &mut writer,
            &json!({ "name": "jwt-tokens", "mode": "revise", "choice": "JWT with DPoP binding", "reason": "Proof-of-possession prevents token replay" }),
        )
        .unwrap();
        assert_eq!(second["history_length"], 2);

        // get_decision_history exposes the full chronological chain.
        let history =
            crate::mcp::context::get_decision_history(writer.state(), "jwt-tokens").unwrap();
        assert_eq!(history["current"]["choice"], "JWT with DPoP binding");
        assert_eq!(history["revision_count"], 2);
        let entries = history["history"].as_array().unwrap();
        assert_eq!(entries[0]["choice"], "JWT tokens");
        assert_eq!(entries[1]["choice"], "JWT with refresh tokens");

        // Promote flips the surviving decision from agent to user.
        let promoted = update_decision(
            &mut writer,
            &json!({ "name": "jwt-tokens", "mode": "promote" }),
        )
        .unwrap();
        assert_eq!(promoted["attribution"], "user");
        assert_eq!(
            writer.state().decisions["jwt-tokens"].decision.attribution,
            Attribution::User
        );
        // History survives the promotion untouched.
        assert_eq!(
            writer.state().decisions["jwt-tokens"]
                .decision
                .history
                .len(),
            2
        );
    }

    // ── cascade_to_json ─────────────────────────────────────────────

    #[test]
    fn cascade_to_json_produces_expected_shape() {
        use crate::store::cascade::{CascadeBlocker, CascadeCleanup, CascadeEffect, CascadeResult};
        use crate::store::schema::EdgeKind;

        let cascade = CascadeResult {
            blockers: vec![CascadeBlocker {
                node: "dep".into(),
                edge: EdgeKind::DependsOn,
                message: "depends on target".into(),
            }],
            warnings: vec![CascadeEffect {
                node: "pat".into(),
                edge: EdgeKind::MemberOf,
                message: "will be updated".into(),
            }],
            cleanups: vec![CascadeCleanup {
                edge: EdgeKind::BelongsTo,
                target: "auth".into(),
            }],
        };

        let (blocked_by, would_warn, would_clean) = cascade_to_json(&cascade);

        assert_eq!(blocked_by.len(), 1);
        assert_eq!(blocked_by[0]["node"], "dep");
        assert_eq!(blocked_by[0]["edge"], "depends_on");
        assert_eq!(blocked_by[0]["message"], "depends on target");

        assert_eq!(would_warn.len(), 1);
        assert_eq!(would_warn[0]["node"], "pat");
        assert_eq!(would_warn[0]["edge"], "member_of");
        assert_eq!(would_warn[0]["message"], "will be updated");

        assert_eq!(would_clean.len(), 1);
        assert_eq!(would_clean[0]["edge"], "belongs_to");
        assert_eq!(would_clean[0]["target"], "auth");
    }

    // ── revise: tags and code_refs ────────────────────────────────────

    #[test]
    fn revise_tags_leaves_no_history() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({ "component": "auth", "choice": "Use JWT", "reason": "Stateless, no server session", "attribution": "user" });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({ "name": "use-jwt", "mode": "revise", "tags": ["security", "auth"] });
        let result = update_decision(&mut writer, &args).unwrap();
        assert_eq!(result["history_length"], 0);

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.tags, vec!["security", "auth"]);
        assert!(dec.decision.history.is_empty());
    }

    #[test]
    fn revise_code_refs() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
        });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [
                { "file": "src/auth/jwt.rs", "symbol": "verify" },
            ],
        });
        update_decision(&mut writer, &args).unwrap();

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/auth/jwt.rs");
    }

    #[test]
    fn revise_code_refs_replaces_existing() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/old.rs" }],
        });
        record_decision(&mut writer, &d).unwrap();

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [{ "file": "src/new.rs" }],
        });
        update_decision(&mut writer, &args).unwrap();

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/new.rs");
    }

    #[test]
    fn revise_empty_code_refs_clears() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/old.rs" }],
        });
        record_decision(&mut writer, &d).unwrap();
        assert_eq!(
            writer.state().decisions["use-jwt"].decision.code_refs.len(),
            1
        );

        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "code_refs": [],
        });
        update_decision(&mut writer, &args).unwrap();

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert!(dec.decision.code_refs.is_empty());
    }

    #[test]
    fn revise_without_code_refs_key_preserves_existing() {
        let (_tmp, store) = setup();
        let mut writer = store.begin_write().unwrap();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "user",
            "code_refs": [{ "file": "src/keep.rs", "symbol": "verify" }],
        });
        record_decision(&mut writer, &d).unwrap();

        // Revise an unrelated field — omitting code_refs must leave refs intact.
        let args = json!({
            "name": "use-jwt",
            "mode": "revise",
            "reason": "Stateless, avoids a server-side session store",
        });
        update_decision(&mut writer, &args).unwrap();

        let dec = writer.state().decisions.get("use-jwt").unwrap();
        assert_eq!(dec.decision.code_refs.len(), 1);
        assert_eq!(dec.decision.code_refs[0].file, "src/keep.rs");
    }
}
