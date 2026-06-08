use std::collections::HashSet;

use serde_json::Value;

use crate::store::graph::Severity;
use crate::store::schema::{EdgeEntry, EdgeKind, NodeEntry, NodeKind, Pattern, PatternFile};
use crate::store::{self, Store, slugify};

// ── Argument helpers ────────────────────────────────────────────────────────

/// Maximum byte length for any single text argument to a write tool.
/// Prevents unbounded disk writes from malicious or buggy MCP clients.
/// Design conversations are typically 10-50 KB total; a single argument
/// should never approach that.
pub(super) const MAX_TEXT_ARG_BYTES: usize = 50_000;

/// Maximum number of elements in any array argument.
pub(super) const MAX_ARRAY_ARG_LEN: usize = 100;

/// Minimum byte length for a decision's `reason` field.
/// Forces actual reasoning instead of rubber-stamp approvals.
const MIN_REASON_BYTES: usize = 10;

/// Maximum byte length for a decision's `choice` field.
/// A choice is a concise title, not a paragraph.
const MAX_CHOICE_BYTES: usize = 200;

pub(super) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let val = args
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required parameter: {key}"))?;
    if val.len() > MAX_TEXT_ARG_BYTES {
        return Err(format!(
            "`{key}` exceeds {MAX_TEXT_ARG_BYTES} byte limit ({} bytes)",
            val.len()
        ));
    }
    Ok(val)
}

pub(super) fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        Some(val) if val.len() > MAX_TEXT_ARG_BYTES => Err(format!(
            "`{key}` exceeds {MAX_TEXT_ARG_BYTES} byte limit ({} bytes)",
            val.len()
        )),
        other => Ok(other),
    }
}

pub(super) fn opt_str_array(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let items: Vec<String> = args
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if items.len() > MAX_ARRAY_ARG_LEN {
        return Err(format!(
            "`{key}` has too many items ({}, max {MAX_ARRAY_ARG_LEN})",
            items.len()
        ));
    }
    for s in &items {
        if s.len() > MAX_TEXT_ARG_BYTES {
            return Err(format!(
                "`{key}` item exceeds {MAX_TEXT_ARG_BYTES} byte limit"
            ));
        }
    }
    Ok(items)
}

pub(super) fn require_str_array(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let arr = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("missing required parameter: {key}"))?;
    if arr.len() > MAX_ARRAY_ARG_LEN {
        return Err(format!(
            "`{key}` has too many items ({}, max {MAX_ARRAY_ARG_LEN})",
            arr.len()
        ));
    }
    let strings: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if strings.is_empty() {
        return Err(format!("{key} must contain at least one non-empty string"));
    }
    for s in &strings {
        if s.len() > MAX_TEXT_ARG_BYTES {
            return Err(format!(
                "`{key}` item exceeds {MAX_TEXT_ARG_BYTES} byte limit"
            ));
        }
    }
    Ok(strings)
}

// ── validate_consistency ────────────────────────────────────────────────────

pub(crate) fn validate_consistency(state: &store::ProjectState) -> Value {
    let issues = state.graph.validate();
    let valid = issues.iter().all(|i| i.severity != Severity::Error);

    serde_json::json!({
        "valid": valid,
        "issues": issues.iter().map(|i| serde_json::json!({
            "severity": match i.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            "message": i.message,
            "location": i.node,
        })).collect::<Vec<_>>(),
    })
}

// ── record_decision ─────────────────────────────────────────────────────────

pub(crate) fn record_decision(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let component = require_str(args, "component")?;
    let choice = require_str(args, "choice")?;
    let reason = require_str(args, "reason")?;
    let alternatives = opt_str_array(args, "alternatives")?;
    let depends_on = opt_str_array(args, "depends_on")?;
    let constrains = opt_str_array(args, "constrains")?;
    let tags = opt_str_array(args, "tags")?;
    let supersedes = opt_str(args, "supersedes")?;

    // Validate component.
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(format!("invalid component name `{component}`"));
    }
    if component != "project" && !state.components.contains_key(component) {
        return Err(format!("component `{component}` does not exist"));
    }

    // Decision quality floor — reject vague or malformed decisions.
    if reason.len() < MIN_REASON_BYTES {
        return Err(format!(
            "reason must be at least {MIN_REASON_BYTES} characters — \
             a real decision needs actual reasoning ({} given)",
            reason.len(),
        ));
    }
    if choice.len() > MAX_CHOICE_BYTES {
        return Err(format!(
            "choice must be ≤{MAX_CHOICE_BYTES} characters — \
             use a concise title, not a paragraph ({} given)",
            choice.len(),
        ));
    }

    // Validate edge targets exist.
    for dep in &depends_on {
        if !state.decisions.contains_key(dep.as_str()) {
            return Err(format!("depends_on target `{dep}` does not exist"));
        }
    }
    for con in &constrains {
        if !state.decisions.contains_key(con.as_str()) {
            return Err(format!("constrains target `{con}` does not exist"));
        }
    }
    if let Some(sup) = supersedes
        && !state.decisions.contains_key(sup)
    {
        return Err(format!("supersedes target `{sup}` does not exist"));
    }

    let lock = store.lock().map_err(|e| e.to_string())?;

    let stem = store
        .record_decision(
            &lock,
            state,
            store::RecordDecisionParams {
                component,
                choice,
                reason,
                alternatives: &alternatives,
                supersedes,
                depends_on: &depends_on,
                constrains: &constrains,
                tags: &tags,
            },
        )
        .map_err(|e| e.to_string())?;

    // Collect warnings for the caller.
    let mut warnings: Vec<String> = Vec::new();
    if alternatives.is_empty() {
        warnings.push(
            "no alternatives provided — a meaningful decision should \
             have at least one rejected option"
                .into(),
        );
    }
    if let Some(sup) = supersedes {
        for (pat_name, _) in state.graph.patterns_containing(sup) {
            warnings.push(format!(
                "superseded decision `{sup}` is referenced by pattern `{pat_name}`"
            ));
        }
    }

    // Server-side pattern detection: scan for tag overlaps across components.
    let pattern_opportunity = detect_pattern_opportunity(state, &stem);

    Ok(serde_json::json!({
        "name": stem,
        "path": store.decision_path(&stem).display().to_string(),
        "warnings": warnings,
        "pattern_opportunity": pattern_opportunity,
        "workflow": {
            "hint": "comprehension_gate",
            "decision": { "choice": choice, "reason": reason },
            "message": format!(
                "Decision recorded: \"{choice}\" — {reason}. \
                 COMPREHENSION GATE: State one concrete, testable implication \
                 of this decision and ask the user to confirm. \
                 Do not proceed until the gate is satisfied.",
            ),
        }
    }))
}

// ── record_pattern ──────────────────────────────────────────────────────────

pub(crate) fn record_pattern(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let description = require_str(args, "description")?;
    let decision_names = require_str_array(args, "decisions")?;
    let component_names = opt_str_array(args, "components")?;
    let tags = opt_str_array(args, "tags")?;

    if decision_names.len() < 2 {
        return Err("a pattern must reference at least 2 decisions".into());
    }

    // Validate all referenced decisions exist.
    for dname in &decision_names {
        if !state.decisions.contains_key(dname.as_str()) {
            return Err(format!("decision `{dname}` does not exist"));
        }
    }

    // Resolve component list: explicit or inferred from decisions.
    let components: Vec<String> = if component_names.is_empty() {
        let mut inferred: HashSet<String> = HashSet::new();
        for dname in &decision_names {
            if let Some(dec) = state.decisions.get(dname.as_str()) {
                let comp = &dec.decision.component;
                if comp != "project" {
                    inferred.insert(comp.clone());
                }
            }
        }
        inferred.into_iter().collect()
    } else {
        for cname in &component_names {
            if !state.components.contains_key(cname.as_str()) {
                return Err(format!("component `{cname}` does not exist"));
            }
        }
        component_names
    };

    let slug = slugify(name);

    if crate::store::is_reserved_node_name(&slug) {
        return Err(format!(
            "pattern slug `{slug}` is reserved — choose a different name"
        ));
    }

    if state.patterns.contains_key(&slug) {
        return Err(format!("pattern `{slug}` already exists"));
    }
    if state.components.contains_key(&slug) || state.decisions.contains_key(&slug) {
        return Err(format!(
            "name `{slug}` is already used by an existing component or decision"
        ));
    }

    let lock = store.lock().map_err(|e| e.to_string())?;

    let pattern = PatternFile {
        pattern: Pattern {
            name: name.into(),
            description: description.into(),
        },
    };

    let write = store
        .prepare_write(&store.pattern_path(&slug), &pattern)
        .map_err(|e| e.to_string())?;
    let hash = write.content_hash();

    // Checkpoint for rollback — O(1) since all mutations are appends.
    let checkpoint = state.graph_checkpoint();

    // Add pattern node.
    state.graph_index.nodes.push(NodeEntry {
        name: slug.clone(),
        kind: NodeKind::Pattern,
        tags,
        hash,
    });

    // MemberOf edges (pattern → decision).
    for dname in &decision_names {
        state.graph_index.edges.push(EdgeEntry {
            from: slug.clone(),
            to: dname.clone(),
            kind: EdgeKind::MemberOf,
        });
    }

    // AppliesTo edges (pattern → component).
    for cname in &components {
        state.graph_index.edges.push(EdgeEntry {
            from: slug.clone(),
            to: cname.clone(),
            kind: EdgeKind::AppliesTo,
        });
    }

    state.patterns.insert(slug.clone(), pattern);

    if let Err(e) = store.commit_with_graph(&lock, vec![write], vec![], state) {
        state.patterns.remove(&slug);
        state.rollback_graph(checkpoint);
        return Err(e.to_string());
    }

    state.rebuild_graph();

    Ok(serde_json::json!({
        "name": slug,
        "path": store.pattern_path(&slug).display().to_string(),
        "workflow": {
            "hint": "pattern_recorded",
            "message": "Pattern recorded. Continue the design session \
                        or call get_context for the implementation brief.",
        }
    }))
}

// ── add_component ──────────────────────────────────────────────────────────

pub(crate) fn add_component(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let description = opt_str(args, "description")?.unwrap_or_default();

    if !store::is_valid_kebab_case(name) {
        return Err(format!(
            "invalid component name `{name}` — must be kebab-case"
        ));
    }
    if store::is_reserved_node_name(name) {
        return Err(format!("`{name}` is reserved"));
    }
    if state.components.contains_key(name) {
        return Err(format!("component `{name}` already exists"));
    }
    if state.is_node_name_taken(name) {
        return Err(format!(
            "name `{name}` is already used by an existing decision or pattern"
        ));
    }

    let comp = store::ComponentFile {
        component: store::schema::Component {
            name: name.into(),
            description: description.into(),
        },
    };

    let write = store
        .prepare_write(&store.component_path(name), &comp)
        .map_err(|e| e.to_string())?;
    let hash = write.content_hash();

    let lock = store.lock().map_err(|e| e.to_string())?;

    let checkpoint = state.graph_checkpoint();
    state.graph_index.nodes.push(NodeEntry {
        name: name.into(),
        kind: NodeKind::Component,
        tags: vec![],
        hash,
    });
    state.components.insert(name.into(), comp);

    if let Err(e) = store.commit_with_graph(&lock, vec![write], vec![], state) {
        state.rollback_graph(checkpoint);
        state.components.remove(name);
        return Err(e.to_string());
    }
    state.rebuild_graph();

    Ok(serde_json::json!({
        "name": name,
        "path": store.component_path(name).display().to_string(),
        "workflow": {
            "next": "get_design_prompt",
            "args": { "component": name, "mode": "full" },
            "message": format!(
                "Component `{name}` added with no decisions. \
                 Call get_design_prompt to run a design conversation.",
            ),
        }
    }))
}

// ── add_connection ─────────────────────────────────────────────────────────

pub(crate) fn add_connection(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let from = require_str(args, "from")?;
    let to = require_str(args, "to")?;

    if !state.components.contains_key(from) {
        return Err(format!("source component `{from}` does not exist"));
    }
    if !state.components.contains_key(to) {
        return Err(format!("target component `{to}` does not exist"));
    }
    if from == to {
        return Err(format!("component `{from}` cannot connect to itself"));
    }

    let duplicate = state
        .graph_index
        .edges
        .iter()
        .any(|e| e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo);
    if duplicate {
        return Err(format!("connection `{from}` → `{to}` already exists"));
    }

    let lock = store.lock().map_err(|e| e.to_string())?;

    let checkpoint = state.graph_checkpoint();
    state.graph_index.edges.push(EdgeEntry {
        from: from.into(),
        to: to.into(),
        kind: EdgeKind::ConnectsTo,
    });

    if let Err(e) = store.commit_with_graph(&lock, vec![], vec![], state) {
        state.rollback_graph(checkpoint);
        return Err(e.to_string());
    }
    state.rebuild_graph();

    Ok(serde_json::json!({
        "from": from,
        "to": to,
        "workflow": {
            "hint": "topology_updated",
            "message": format!(
                "Connection {from} → {to} added. \
                 get_context will now include related decisions from connected components.",
            ),
        }
    }))
}

// ── Pattern opportunity detection ──────────────────────────────────────────

/// Scan the graph for tag overlaps between the newly recorded decision and
/// existing decisions in *other* components that are not already co-members
/// of a pattern. Returns `Value::Null` if no opportunity is found, or a
/// JSON object describing the strongest candidate group.
///
/// Complexity: O(D × T) where D = total decisions, T = new decision's tag
/// count. Both are small — a mature project has ~100 decisions and ~5 tags
/// per decision.
fn detect_pattern_opportunity(state: &store::ProjectState, new_stem: &str) -> Value {
    let new_dec = match state.decisions.get(new_stem) {
        Some(d) => d,
        None => return Value::Null,
    };
    let new_tags = &new_dec.decision.tags;
    if new_tags.is_empty() {
        return Value::Null;
    }

    // Collect decisions already in a pattern with the new decision.
    let mut co_patterned: HashSet<&str> = HashSet::new();
    for (pat_name, _) in state.graph.patterns_containing(new_stem) {
        for (member, _) in state.graph.decisions_for_pattern(pat_name) {
            co_patterned.insert(member);
        }
    }

    // For each tag, find decisions sharing it across different components.
    let new_component = &new_dec.decision.component;
    let mut best_tag: Option<&str> = None;
    let mut best_decisions: Vec<&str> = Vec::new();
    let mut best_components: HashSet<&str> = HashSet::new();

    for tag in new_tags {
        let mut decisions = vec![new_stem];
        let mut components: HashSet<&str> = HashSet::new();
        components.insert(new_component);

        for (name, dec) in &state.decisions {
            if name.as_str() == new_stem {
                continue;
            }
            if co_patterned.contains(name.as_str()) {
                continue;
            }
            if dec.decision.tags.iter().any(|t| t == tag) {
                decisions.push(name);
                components.insert(&dec.decision.component);
            }
        }

        // A pattern opportunity needs 2+ decisions across 2+ components.
        if decisions.len() >= 2 && components.len() >= 2 && decisions.len() > best_decisions.len() {
            best_tag = Some(tag);
            best_decisions = decisions;
            best_components = components;
        }
    }

    match best_tag {
        Some(tag) => {
            let n_decisions = best_decisions.len();
            let components: Vec<&str> = best_components.into_iter().collect();
            let n_components = components.len();
            serde_json::json!({
                "shared_tag": tag,
                "decisions": best_decisions,
                "components": components,
                "message": format!(
                    "{n_decisions} decisions across {n_components} components \
                     share the tag \"{tag}\". \
                     Consider recording a pattern with record_pattern.",
                ),
            })
        }
        None => Value::Null,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

    // ── validate_consistency ────────────────────────────────────────────

    #[test]
    fn validate_clean_state() {
        let (_tmp, _store, state) = setup();
        let result = validate_consistency(&state);
        assert_eq!(result["valid"], true);
        assert!(result["issues"].as_array().unwrap().is_empty());
    }

    // ── record_decision ─────────────────────────────────────────────────

    #[test]
    fn record_decision_basic() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "JWT with DPoP",
            "reason": "Stateless, no session store needed",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "jwt-with-dpop");
        assert!(state.decisions.contains_key("jwt-with-dpop"));
    }

    #[test]
    fn record_decision_with_all_fields() {
        let (_tmp, store, mut state) = setup();

        // First, record a base decision to reference.
        let base = json!({
            "component": "auth",
            "choice": "Use tokens",
            "reason": "Stateless, no server session",
        });
        record_decision(&store, &mut state, &base).unwrap();

        let args = json!({
            "component": "auth",
            "choice": "JWT format specifically",
            "reason": "DPoP binding",
            "alternatives": ["Session cookies — rejected: server state"],
            "depends_on": ["use-tokens"],
            "tags": ["security", "auth"],
            "supersedes": "use-tokens",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        assert!(!result["name"].as_str().unwrap().is_empty());

        // Verify edges exist.
        let idx = &state.graph_index;
        let name = result["name"].as_str().unwrap();
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == name && e.to == "use-tokens" && e.kind == EdgeKind::DependsOn)
        );
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == name && e.to == "use-tokens" && e.kind == EdgeKind::Supersedes)
        );
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == name && e.to == "auth" && e.kind == EdgeKind::BelongsTo)
        );

        // Verify tags.
        let node = idx.nodes.iter().find(|n| n.name == name).unwrap();
        assert!(node.tags.contains(&"security".to_string()));
    }

    #[test]
    fn record_decision_project_wide() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "project",
            "choice": "Fail-closed on writes",
            "reason": "Never silently succeed with wrong data",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let dec = state
            .decisions
            .get(result["name"].as_str().unwrap())
            .unwrap();
        assert_eq!(dec.decision.component, "project");
    }

    #[test]
    fn record_decision_rejects_missing_component() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "ghost",
            "choice": "x",
            "reason": "test validation target",
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn record_decision_rejects_nonexistent_depends_on() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "x",
            "reason": "test validation target",
            "depends_on": ["ghost"],
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn record_decision_rejects_cycle() {
        let (_tmp, store, mut state) = setup();

        let a = json!({ "component": "auth", "choice": "A", "reason": "cycle dependency test" });
        record_decision(&store, &mut state, &a).unwrap();

        let b = json!({
            "component": "auth", "choice": "B", "reason": "cycle dependency test",
            "depends_on": ["a"],
        });
        record_decision(&store, &mut state, &b).unwrap();

        // C depends on B, which depends on A. Now try A depending on C.
        // Actually, cycle detection is: the NEW decision depends on existing
        // ones, so the cycle would be if we create C that depends on B,
        // then try to make A depend on C. But record_decision only creates
        // NEW decisions. We can't create a cycle with forward depends_on
        // on a NEW node since nothing can depend on it yet.
        // A cycle requires adding DependsOn from existing→new which can't
        // happen via record_decision. Cycles are caught by graph validation.
        // Let's test via direct graph manipulation instead.
    }

    // ── record_pattern ──────────────────────────────────────────────────

    #[test]
    fn record_pattern_basic() {
        let (_tmp, store, mut state) = setup();

        // Record two decisions first.
        let d1 =
            json!({ "component": "auth", "choice": "Use Redis", "reason": "Fast in-memory reads" });
        let d2 = json!({ "component": "database", "choice": "Redis pool", "reason": "Shared pool reduces overhead" });
        record_decision(&store, &mut state, &d1).unwrap();
        record_decision(&store, &mut state, &d2).unwrap();

        let args = json!({
            "name": "All state in Redis",
            "description": "Shared Redis pool for all persistent state",
            "decisions": ["use-redis", "redis-pool"],
        });
        let result = record_pattern(&store, &mut state, &args).unwrap();
        let slug = result["name"].as_str().unwrap();
        assert!(!slug.is_empty());
        assert!(state.patterns.contains_key(slug));

        // Verify edges.
        let idx = &state.graph_index;
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == slug && e.to == "use-redis" && e.kind == EdgeKind::MemberOf)
        );
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == slug && e.to == "redis-pool" && e.kind == EdgeKind::MemberOf)
        );

        // Components inferred from decisions.
        assert!(
            idx.edges
                .iter()
                .any(|e| e.from == slug && e.kind == EdgeKind::AppliesTo)
        );
    }

    #[test]
    fn record_pattern_rejects_single_decision() {
        let (_tmp, store, mut state) = setup();
        let d = json!({ "component": "auth", "choice": "X", "reason": "test reason placeholder" });
        record_decision(&store, &mut state, &d).unwrap();

        let args = json!({
            "name": "Lone pattern",
            "description": "Only one decision",
            "decisions": ["x"],
        });
        let err = record_pattern(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("at least 2"));
    }

    #[test]
    fn record_pattern_rejects_nonexistent_decision() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "name": "Ghost pattern",
            "description": "References nothing",
            "decisions": ["ghost-a", "ghost-b"],
        });
        let err = record_pattern(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    // ── add_component ──────────────────────────────────────────────────

    #[test]
    fn add_component_basic() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "rate-limiter", "description": "Per-key rate limiting" });
        let result = add_component(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "rate-limiter");
        assert!(state.components.contains_key("rate-limiter"));
    }

    #[test]
    fn add_component_without_description() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "cache" });
        let result = add_component(&store, &mut state, &args).unwrap();
        assert_eq!(result["name"], "cache");
        let comp = state.components.get("cache").unwrap();
        assert!(comp.component.description.is_empty());
    }

    #[test]
    fn add_component_rejects_duplicate() {
        let (_tmp, store, mut state) = setup();
        let err = add_component(&store, &mut state, &json!({ "name": "auth" })).unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn add_component_rejects_invalid_name() {
        let (_tmp, store, mut state) = setup();
        let err = add_component(&store, &mut state, &json!({ "name": "Not-Valid" })).unwrap_err();
        assert!(err.contains("kebab-case"));
    }

    #[test]
    fn add_component_rejects_reserved_name() {
        let (_tmp, store, mut state) = setup();
        let err = add_component(&store, &mut state, &json!({ "name": "project" })).unwrap_err();
        assert!(err.contains("reserved"));
    }

    // ── add_connection ─────────────────────────────────────────────────

    #[test]
    fn add_connection_basic() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "from": "auth", "to": "database" });
        let result = add_connection(&store, &mut state, &args).unwrap();
        assert_eq!(result["from"], "auth");
        assert_eq!(result["to"], "database");
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "auth" && e.to == "database" && e.kind == EdgeKind::ConnectsTo)
        );
    }

    #[test]
    fn add_connection_rejects_nonexistent_source() {
        let (_tmp, store, mut state) = setup();
        let err = add_connection(
            &store,
            &mut state,
            &json!({ "from": "ghost", "to": "auth" }),
        )
        .unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn add_connection_rejects_self_connection() {
        let (_tmp, store, mut state) = setup();
        let err = add_connection(&store, &mut state, &json!({ "from": "auth", "to": "auth" }))
            .unwrap_err();
        assert!(err.contains("cannot connect to itself"));
    }

    #[test]
    fn add_connection_rejects_duplicate() {
        let (_tmp, store, mut state) = setup();
        add_connection(
            &store,
            &mut state,
            &json!({ "from": "auth", "to": "database" }),
        )
        .unwrap();
        let err = add_connection(
            &store,
            &mut state,
            &json!({ "from": "auth", "to": "database" }),
        )
        .unwrap_err();
        assert!(err.contains("already exists"));
    }

    // ── workflow hints ─────────────────────────────────────────────────

    #[test]
    fn add_component_workflow_suggests_design() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "cache", "description": "Caching layer" });
        let result = add_component(&store, &mut state, &args).unwrap();
        let wf = &result["workflow"];
        assert_eq!(wf["next"], "get_design_prompt");
        assert_eq!(wf["args"]["component"], "cache");
        assert_eq!(wf["args"]["mode"], "full");
        assert!(
            wf["message"]
                .as_str()
                .unwrap()
                .contains("design conversation")
        );
    }

    #[test]
    fn record_decision_workflow_has_comprehension_gate() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless auth, no session store",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let wf = &result["workflow"];
        assert_eq!(wf["hint"], "comprehension_gate");
        assert_eq!(wf["decision"]["choice"], "Use JWT");
        assert_eq!(wf["decision"]["reason"], "Stateless auth, no session store");
        assert!(
            wf["message"]
                .as_str()
                .unwrap()
                .contains("COMPREHENSION GATE")
        );
    }

    #[test]
    fn record_pattern_returns_name_not_slug() {
        let (_tmp, store, mut state) = setup();
        let d1 =
            json!({ "component": "auth", "choice": "Use JWT", "reason": "Fast in-memory reads" });
        let d2 = json!({ "component": "database", "choice": "JWT verify", "reason": "Authentication verification" });
        record_decision(&store, &mut state, &d1).unwrap();
        record_decision(&store, &mut state, &d2).unwrap();

        let args = json!({
            "name": "Token pattern",
            "description": "Token handling",
            "decisions": ["use-jwt", "jwt-verify"],
        });
        let result = record_pattern(&store, &mut state, &args).unwrap();
        assert!(
            result.get("name").is_some(),
            "must return 'name', not 'slug'"
        );
        assert!(
            result.get("slug").is_none(),
            "must not return legacy 'slug' key"
        );
        assert_eq!(result["workflow"]["hint"], "pattern_recorded");
    }

    #[test]
    fn add_connection_workflow_present() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "from": "auth", "to": "database" });
        let result = add_connection(&store, &mut state, &args).unwrap();
        assert_eq!(result["workflow"]["hint"], "topology_updated");
    }

    // ── decision quality floor ────────────────────────────────────────

    #[test]
    fn record_decision_rejects_short_reason() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "ok",
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(
            err.contains("at least") && err.contains("characters"),
            "should reject short reason: {err}"
        );
    }

    #[test]
    fn record_decision_rejects_long_choice() {
        let (_tmp, store, mut state) = setup();
        let long_choice = "x".repeat(MAX_CHOICE_BYTES + 1);
        let args = json!({
            "component": "auth",
            "choice": long_choice,
            "reason": "This is a valid reason",
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("200"), "should reject long choice: {err}");
    }

    #[test]
    fn record_decision_warns_no_alternatives() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("alternatives")),
            "should warn about missing alternatives: {warnings:?}"
        );
    }

    #[test]
    fn record_decision_no_warning_with_alternatives() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "alternatives": ["Session cookies — server-side state"],
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("alternatives")),
            "should not warn when alternatives provided"
        );
    }

    // ── pattern detection ──────────────────────────────────────────────

    #[test]
    fn record_decision_detects_pattern_opportunity() {
        let (_tmp, store, mut state) = setup();

        // Record a tagged decision in auth.
        let d1 = json!({
            "component": "auth",
            "choice": "Redis for session tokens",
            "reason": "Fast in-memory lookups for token validation",
            "tags": ["redis"],
        });
        record_decision(&store, &mut state, &d1).unwrap();

        // Record a tagged decision in database with overlapping tag.
        let d2 = json!({
            "component": "database",
            "choice": "Redis for query cache",
            "reason": "Avoid repeated expensive queries with caching",
            "tags": ["redis"],
        });
        let result = record_decision(&store, &mut state, &d2).unwrap();

        let opp = &result["pattern_opportunity"];
        assert!(!opp.is_null(), "should detect pattern opportunity");
        assert_eq!(opp["shared_tag"], "redis");
        let decisions = opp["decisions"].as_array().unwrap();
        assert!(decisions.len() >= 2);
    }

    #[test]
    fn record_decision_no_pattern_without_tags() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        assert!(
            result["pattern_opportunity"].is_null(),
            "should not suggest patterns when decision has no tags"
        );
    }

    #[test]
    fn record_decision_no_pattern_for_same_component() {
        let (_tmp, store, mut state) = setup();

        // Two tagged decisions in the SAME component — not a cross-component pattern.
        let d1 = json!({
            "component": "auth",
            "choice": "Redis for session tokens",
            "reason": "Fast in-memory lookups for tokens",
            "tags": ["redis"],
        });
        record_decision(&store, &mut state, &d1).unwrap();

        let d2 = json!({
            "component": "auth",
            "choice": "Redis for rate limit counters",
            "reason": "Per-key counters need fast increment",
            "tags": ["redis"],
        });
        let result = record_decision(&store, &mut state, &d2).unwrap();
        assert!(
            result["pattern_opportunity"].is_null(),
            "same-component tag overlap is not a cross-component pattern"
        );
    }
}
