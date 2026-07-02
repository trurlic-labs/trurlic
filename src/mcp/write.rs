use std::collections::HashSet;

use serde_json::Value;

use crate::store::graph::Severity;
use crate::store::limits::{
    MAX_ARRAY_ITEMS, MAX_CHOICE_BYTES, MAX_TEXT_FIELD_BYTES, MIN_REASON_BYTES,
};
use crate::store::schema::{Attribution, CodeRef};
use crate::store::{self, Store};

// ── Argument helpers ────────────────────────────────────────────────────────

pub(super) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let val = args
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required parameter: {key}"))?;
    if val.len() > MAX_TEXT_FIELD_BYTES {
        return Err(format!(
            "`{key}` exceeds {MAX_TEXT_FIELD_BYTES} byte limit ({} bytes)",
            val.len()
        ));
    }
    if has_control_chars(val) {
        return Err(format!("`{key}` contains invalid control characters"));
    }
    Ok(val)
}

pub(super) fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        Some(val) if val.len() > MAX_TEXT_FIELD_BYTES => Err(format!(
            "`{key}` exceeds {MAX_TEXT_FIELD_BYTES} byte limit ({} bytes)",
            val.len()
        )),
        Some(val) if has_control_chars(val) => {
            Err(format!("`{key}` contains invalid control characters"))
        }
        other => Ok(other),
    }
}

fn has_control_chars(s: &str) -> bool {
    crate::store::has_control_chars(s)
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
    if items.len() > MAX_ARRAY_ITEMS {
        return Err(format!(
            "`{key}` has too many items ({}, max {MAX_ARRAY_ITEMS})",
            items.len()
        ));
    }
    for s in &items {
        if s.len() > MAX_TEXT_FIELD_BYTES {
            return Err(format!(
                "`{key}` item exceeds {MAX_TEXT_FIELD_BYTES} byte limit"
            ));
        }
        if has_control_chars(s) {
            return Err(format!("`{key}` item contains invalid control characters"));
        }
    }
    Ok(items)
}

pub(super) fn require_str_array(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let arr = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("missing required parameter: {key}"))?;
    if arr.len() > MAX_ARRAY_ITEMS {
        return Err(format!(
            "`{key}` has too many items ({}, max {MAX_ARRAY_ITEMS})",
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
        if s.len() > MAX_TEXT_FIELD_BYTES {
            return Err(format!(
                "`{key}` item exceeds {MAX_TEXT_FIELD_BYTES} byte limit"
            ));
        }
        if has_control_chars(s) {
            return Err(format!("`{key}` item contains invalid control characters"));
        }
    }
    Ok(strings)
}

pub(super) fn parse_code_refs(args: &Value) -> Result<Vec<CodeRef>, String> {
    // Absent or explicit null means "no code_refs supplied". A present-but-
    // non-array value is malformed and rejected — never silently ignored.
    let arr = match args.get("code_refs") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("code_refs must be an array".into()),
    };
    let mut refs = Vec::with_capacity(arr.len());
    for item in arr {
        let file = item
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or("code_ref missing required field: file")?
            .to_string();
        let symbol = item
            .get("symbol")
            .and_then(|v| v.as_str())
            .map(String::from);
        refs.push(CodeRef { file, symbol });
    }
    store::validate_code_refs(&refs).map_err(|e| e.to_string())?;
    Ok(refs)
}

// ── validate_consistency ────────────────────────────────────────────────────

pub(crate) fn validate_consistency(state: &store::ProjectState) -> Value {
    let issues = state.graph().validate();
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
    let code_refs = parse_code_refs(args)?;
    let attribution = match require_str(args, "attribution")? {
        "user" => Attribution::User,
        "agent" => Attribution::Agent,
        other => {
            return Err(format!(
                "invalid attribution `{other}`: must be \"user\" or \"agent\""
            ));
        }
    };

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

    // Reject an exact restatement of an existing decision in the same
    // component (case-insensitive). Forking the graph with a duplicate node
    // loses the original's history and edges — revising it in place keeps both.
    let choice_lower = choice.to_ascii_lowercase();
    for (existing_name, existing_dec) in &state.decisions {
        if existing_dec.decision.component == component
            && existing_dec.decision.choice.to_ascii_lowercase() == choice_lower
        {
            return Err(format!(
                "decision `{existing_name}` in [{component}] has identical choice text — \
                 use update_decision(mode=\"revise\") to update it"
            ));
        }
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
                depends_on: &depends_on,
                constrains: &constrains,
                tags: &tags,
                attribution,
                code_refs: &code_refs,
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
    // Flag probable near-duplicates: an existing decision in the same
    // component whose choice shares most of its significant words with the
    // new one. Advisory, not a block — reworded variants are sometimes
    // legitimately distinct, so the agent decides whether to consolidate.
    for (existing_name, existing_dec) in &state.decisions {
        if existing_name.as_str() == stem {
            continue;
        }
        if existing_dec.decision.component != component {
            continue;
        }
        let overlap = word_overlap(choice, &existing_dec.decision.choice);
        if overlap > NEAR_DUPLICATE_OVERLAP {
            let percent = (overlap * 100.0).round() as u64;
            warnings.push(format!(
                "high text overlap ({percent}%) with decision `{existing_name}` in \
                 [{component}] — consider using update_decision(mode=\"revise\") instead"
            ));
        }
    }

    // Server-side pattern detection: scan for tag overlaps across components.
    let pattern_opportunity = detect_pattern_opportunity(state, &stem);

    Ok(serde_json::json!({
        "name": stem,
        "path": store.decision_path(&stem).display().to_string(),
        "code_refs": store::code_refs_to_json(&code_refs),
        "warnings": warnings,
        "pattern_opportunity": pattern_opportunity,
    }))
}

// ── Near-duplicate detection ─────────────────────────────────────────────────

/// Word-overlap ratio above which two same-component decisions are flagged as
/// probable near-duplicates.
const NEAR_DUPLICATE_OVERLAP: f64 = 0.7;

/// Jaccard similarity over the significant words of two choice texts. Words
/// shorter than three characters are dropped as noise, surrounding punctuation
/// is stripped so `"JWT."` matches `"JWT"`, and comparison is case-insensitive.
/// Returns a value in `[0.0, 1.0]`; `0.0` when either side has no significant
/// words.
fn word_overlap(a: &str, b: &str) -> f64 {
    fn significant_words(text: &str) -> HashSet<String> {
        text.split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_ascii_lowercase()
            })
            .filter(|w| w.len() >= 3)
            .collect()
    }

    let words_a = significant_words(a);
    let words_b = significant_words(b);
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    intersection as f64 / union as f64
}

// ── record_pattern ──────────────────────────────────────────────────────────

pub(crate) fn record_pattern(
    store: &Store,
    state: &mut store::ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let description = require_str(args, "description")?;
    let decisions = require_str_array(args, "decisions")?;
    let components = opt_str_array(args, "components")?;
    let tags = opt_str_array(args, "tags")?;

    let lock = store.lock().map_err(|e| e.to_string())?;
    let slug = store
        .record_pattern(
            &lock,
            state,
            store::RecordPatternParams {
                name,
                description,
                decisions: &decisions,
                components: &components,
                tags: &tags,
            },
        )
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "name": slug,
        "path": store.pattern_path(&slug).display().to_string(),
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

    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .add_component(&lock, state, name, description)
        .map_err(|e| e.to_string())?;

    let mut warnings: Vec<String> = Vec::new();
    if description.is_empty() {
        warnings.push(
            "component has no description — add one so get_context and \
             the map can show what this component is responsible for"
                .into(),
        );
    }

    Ok(serde_json::json!({
        "name": name,
        "path": store.component_path(name).display().to_string(),
        "warnings": warnings,
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

    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .add_connection(&lock, state, from, to)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "from": from,
        "to": to,
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
    for (pat_name, _) in state.graph().patterns_containing(new_stem) {
        for (member, _) in state.graph().decisions_for_pattern(pat_name) {
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
    use crate::store::schema::EdgeKind;
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
            "attribution": "user",
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
            "attribution": "user",
        });
        record_decision(&store, &mut state, &base).unwrap();

        let args = json!({
            "component": "auth",
            "choice": "JWT format specifically",
            "reason": "DPoP binding",
            "alternatives": ["Session cookies — rejected: server state"],
            "depends_on": ["use-tokens"],
            "tags": ["security", "auth"],
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("ghost"));
    }

    // ── record_pattern ──────────────────────────────────────────────────

    #[test]
    fn record_pattern_basic() {
        let (_tmp, store, mut state) = setup();

        // Record two decisions first.
        let d1 = json!({ "component": "auth", "choice": "Use Redis", "reason": "Fast in-memory reads", "attribution": "user" });
        let d2 = json!({ "component": "database", "choice": "Redis pool", "reason": "Shared pool reduces overhead", "attribution": "user" });
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
        let d = json!({ "component": "auth", "choice": "X", "reason": "test reason placeholder", "attribution": "user" });
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

    // ── no workflow hints ─────────────────────────────────────────────

    #[test]
    fn add_component_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "cache", "description": "Caching layer" });
        let result = add_component(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
    }

    #[test]
    fn record_decision_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless auth, no session store",
            "attribution": "user",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
        assert!(result.get("coverage_gap").is_none());
    }

    #[test]
    fn record_pattern_returns_name_not_slug() {
        let (_tmp, store, mut state) = setup();
        let d1 = json!({ "component": "auth", "choice": "Use JWT", "reason": "Fast in-memory reads", "attribution": "user" });
        let d2 = json!({ "component": "database", "choice": "JWT verify", "reason": "Authentication verification", "attribution": "user" });
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
        assert!(result.get("workflow").is_none());
    }

    #[test]
    fn add_connection_no_workflow() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "from": "auth", "to": "database" });
        let result = add_connection(&store, &mut state, &args).unwrap();
        assert!(result.get("workflow").is_none());
    }

    // ── decision quality floor ────────────────────────────────────────

    #[test]
    fn record_decision_rejects_short_reason() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "ok",
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d1).unwrap();

        // Record a tagged decision in database with overlapping tag.
        let d2 = json!({
            "component": "database",
            "choice": "Redis for query cache",
            "reason": "Avoid repeated expensive queries with caching",
            "tags": ["redis"],
            "attribution": "user",
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
            "attribution": "user",
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
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d1).unwrap();

        let d2 = json!({
            "component": "auth",
            "choice": "Redis for rate limit counters",
            "reason": "Per-key counters need fast increment",
            "tags": ["redis"],
            "attribution": "user",
        });
        let result = record_decision(&store, &mut state, &d2).unwrap();
        assert!(
            result["pattern_opportunity"].is_null(),
            "same-component tag overlap is not a cross-component pattern"
        );
    }

    // ── input hardening ───────────────────────────────────────────────

    #[test]
    fn require_str_rejects_control_characters() {
        let args = json!({ "key": "hello\x00world" });
        let err = require_str(&args, "key").unwrap_err();
        assert!(err.contains("control characters"));
    }

    #[test]
    fn require_str_allows_normal_whitespace() {
        let args = json!({ "key": "hello\nworld\ttab" });
        assert!(require_str(&args, "key").is_ok());
    }

    #[test]
    fn record_decision_rejects_duplicate_choice() {
        let (_tmp, store, mut state) = setup();
        let d1 = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d1).unwrap();

        // Same choice text (case-insensitive), same component — hard error.
        let d2 = json!({
            "component": "auth",
            "choice": "use jwt TOKENS",
            "reason": "Different reasoning entirely",
            "attribution": "user",
        });
        let err = record_decision(&store, &mut state, &d2).unwrap_err();
        assert!(
            err.contains("identical choice") && err.contains("revise"),
            "should reject duplicate and point at revise: {err}"
        );
        // The rejected decision must never reach disk or state.
        assert_eq!(state.decisions.len(), 1);
    }

    #[test]
    fn record_decision_warns_on_high_overlap() {
        let (_tmp, store, mut state) = setup();
        let d1 = json!({
            "component": "auth",
            "choice": "Use JWT tokens for authentication",
            "reason": "Stateless authentication model",
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d1).unwrap();

        // Same significant words plus one — high Jaccard overlap, not identical.
        let d2 = json!({
            "component": "auth",
            "choice": "Use JWT tokens for authentication flow",
            "reason": "Slightly different framing of the same idea",
            "attribution": "user",
        });
        let result = record_decision(&store, &mut state, &d2).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("overlap")),
            "should warn about high text overlap: {warnings:?}"
        );
    }

    #[test]
    fn record_decision_no_warning_on_low_overlap() {
        let (_tmp, store, mut state) = setup();
        let d1 = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "attribution": "user",
        });
        record_decision(&store, &mut state, &d1).unwrap();

        // Unrelated wording in the same component — no near-duplicate warning.
        let d2 = json!({
            "component": "auth",
            "choice": "Rotate signing keys quarterly",
            "reason": "Limit blast radius of a key compromise",
            "attribution": "user",
        });
        let result = record_decision(&store, &mut state, &d2).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("overlap")),
            "should not warn on low overlap: {warnings:?}"
        );
    }

    #[test]
    fn word_overlap_is_jaccard_over_significant_words() {
        // Identical significant words, differing only in case → 1.0.
        assert!((word_overlap("Use JWT tokens", "use jwt TOKENS") - 1.0).abs() < 1e-9);
        // Disjoint word sets → 0.0.
        assert_eq!(word_overlap("alpha beta gamma", "delta epsilon zeta"), 0.0);
        // Words shorter than three chars carry no signal → 0.0.
        assert_eq!(word_overlap("a an of", "no it is"), 0.0);
        // Partial overlap → intersection / union: {jwt} over {jwt, tokens, cookies}.
        let overlap = word_overlap("jwt tokens", "jwt cookies");
        assert!((overlap - 1.0 / 3.0).abs() < 1e-9, "got {overlap}");
    }

    #[test]
    fn record_decision_requires_attribution() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
        });
        let err = record_decision(&store, &mut state, &d).unwrap_err();
        assert!(
            err.contains("attribution"),
            "should require attribution parameter: {err}"
        );
    }

    #[test]
    fn record_decision_rejects_invalid_attribution() {
        let (_tmp, store, mut state) = setup();
        let d = json!({
            "component": "auth",
            "choice": "Use JWT",
            "reason": "Stateless, no server session",
            "attribution": "maybe",
        });
        let err = record_decision(&store, &mut state, &d).unwrap_err();
        assert!(
            err.contains("invalid attribution"),
            "should reject invalid attribution value: {err}"
        );
    }

    #[test]
    fn add_component_warns_on_empty_description() {
        let (_tmp, store, mut state) = setup();
        let args = json!({ "name": "cache" });
        let result = add_component(&store, &mut state, &args).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("no description")),
            "should warn about missing description: {warnings:?}"
        );
    }

    // ── code_refs ─────────────────────────────────────────────────────

    #[test]
    fn record_decision_with_code_refs() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "JWT with DPoP",
            "reason": "Stateless, no session store needed",
            "attribution": "user",
            "code_refs": [
                { "file": "src/auth/token.rs", "symbol": "verify_token" },
                { "file": "src/auth/middleware.rs" }
            ],
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let name = result["name"].as_str().unwrap();

        let dec = state.decisions.get(name).unwrap();
        assert_eq!(dec.decision.code_refs.len(), 2);
        assert_eq!(dec.decision.code_refs[0].file, "src/auth/token.rs");
        assert_eq!(
            dec.decision.code_refs[0].symbol.as_deref(),
            Some("verify_token")
        );
        assert_eq!(dec.decision.code_refs[1].file, "src/auth/middleware.rs");
        assert!(dec.decision.code_refs[1].symbol.is_none());

        let refs = result["code_refs"].as_array().unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0]["file"], "src/auth/token.rs");
        assert_eq!(refs[0]["symbol"], "verify_token");
    }

    #[test]
    fn record_decision_without_code_refs() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "attribution": "user",
        });
        let result = record_decision(&store, &mut state, &args).unwrap();
        let refs = result["code_refs"].as_array().unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn record_decision_rejects_invalid_code_ref() {
        let (_tmp, store, mut state) = setup();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "attribution": "user",
            "code_refs": [{ "file": "/absolute/path.rs" }],
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("relative"), "{err}");
    }

    #[test]
    fn record_decision_rejects_too_many_code_refs() {
        let (_tmp, store, mut state) = setup();
        let refs: Vec<_> = (0..21)
            .map(|i| json!({ "file": format!("src/f{i}.rs") }))
            .collect();
        let args = json!({
            "component": "auth",
            "choice": "Use JWT tokens",
            "reason": "Stateless authentication model",
            "attribution": "user",
            "code_refs": refs,
        });
        let err = record_decision(&store, &mut state, &args).unwrap_err();
        assert!(err.contains("too many"), "{err}");
    }

    #[test]
    fn parse_code_refs_rejects_empty_symbol() {
        let args = json!({
            "code_refs": [{ "file": "src/lib.rs", "symbol": "" }],
        });
        let err = parse_code_refs(&args).unwrap_err();
        assert!(err.contains("symbol"), "{err}");
    }

    #[test]
    fn parse_code_refs_rejects_non_array() {
        let args = json!({ "code_refs": "src/lib.rs" });
        let err = parse_code_refs(&args).unwrap_err();
        assert!(err.contains("must be an array"), "{err}");
    }

    #[test]
    fn parse_code_refs_absent_and_null_yield_empty() {
        assert!(parse_code_refs(&json!({})).unwrap().is_empty());
        assert!(
            parse_code_refs(&json!({ "code_refs": null }))
                .unwrap()
                .is_empty()
        );
    }
}
