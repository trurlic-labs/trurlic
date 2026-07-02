//! REST API handlers for the map server.
//!
//! All endpoints operate on the shared `MapState`. Read operations
//! acquire only a read lock. Write operations use `spawn_blocking` to
//! run the synchronous `Store` write path (which acquires a file lock
//! that could block for up to 5 seconds) without stalling the tokio
//! runtime.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::limits::{
    MAX_ARRAY_ITEMS, MAX_CHOICE_BYTES, MAX_TEXT_FIELD_BYTES, MIN_REASON_BYTES,
};
use crate::store::schema::{EdgeKind, NodeKind};
use crate::store::{self};

use super::diff::WsEvent;
use super::layout::Position;
use super::{MapState, ws};

// ── Request bodies ─────────────────────────────────────────────────────────

/// Maximum number of layout positions in a single PUT.
const MAX_LAYOUT_POSITIONS: usize = 10_000;

#[derive(Deserialize)]
pub(crate) struct CreateComponent {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
pub(crate) struct CreateConnection {
    from: String,
    to: String,
}

#[derive(Deserialize)]
pub(crate) struct ReviseDecision {
    choice: Option<String>,
    reason: Option<String>,
    tags: Option<Vec<String>>,
    code_refs: Option<Vec<store::CodeRef>>,
}

#[derive(Deserialize)]
pub(crate) struct PutLayout {
    positions: std::collections::BTreeMap<String, Position>,
    layout_version: u64,
}

// ── Input validation ──────────────────────────────────────────────────────

fn check_field_len(field: &str, value: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if value.len() > MAX_TEXT_FIELD_BYTES {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("`{field}` exceeds {MAX_TEXT_FIELD_BYTES} byte limit"),
        ));
    }
    if store::has_control_chars(value) {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("`{field}` contains invalid control characters"),
        ));
    }
    Ok(())
}

// ── Error helper ───────────────────────────────────────────────────────────

fn api_err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": msg.into() })))
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

// ── GET /api/graph ─────────────────────────────────────────────────────────

pub(crate) async fn get_graph(State(state): State<Arc<MapState>>) -> ApiResult {
    let ps = state.read_project_state();
    let layout = state.read_layout();
    let graph = ps.graph();

    let components: Vec<Value> = ps
        .components
        .iter()
        .map(|(name, comp)| {
            let pos = layout.positions.get(name.as_str());
            json!({
                "name": name,
                "description": comp.component.description,
                "position": pos.map(|p| json!({"x": p.x, "y": p.y})),
                "pinned": pos.is_some_and(|p| p.pinned),
                "decision_count": graph.decisions_for(name).len(),
                "pattern_count": graph.patterns_for(name).len(),
            })
        })
        .collect();

    let decisions: Vec<Value> = ps
        .decisions
        .iter()
        .map(|(name, dec)| {
            let d = &dec.decision;
            let mut obj = json!({
                "name": name,
                "component": d.component,
                "choice": d.choice,
                "reason": d.reason,
                "tags": d.tags,
                "created": d.created.to_rfc3339(),
                "alternatives": d.alternatives,
            });
            if !d.code_refs.is_empty() {
                obj["code_refs"] = json!(store::code_refs_to_json(&d.code_refs));
            }
            obj
        })
        .collect();

    let patterns: Vec<Value> = ps
        .patterns
        .iter()
        .map(|(slug, pat)| {
            let member_decisions: Vec<&str> = graph
                .decisions_for_pattern(slug)
                .into_iter()
                .map(|(n, _)| n.as_ref())
                .collect();
            let applied_components: Vec<&str> =
                graph.components_for_pattern(slug).into_iter().collect();
            json!({
                "name": slug,
                "description": pat.pattern.description,
                "decisions": member_decisions,
                "components": applied_components,
            })
        })
        .collect();

    let edges: Vec<Value> = ps
        .graph_index
        .edges
        .iter()
        .map(|e| {
            json!({
                "from": e.from,
                "to": e.to,
                "kind": e.kind.as_str(),
            })
        })
        .collect();

    Ok(Json(json!({
        "project": {
            "name": ps.project.project.name,
            "description": ps.project.project.description,
        },
        "components": components,
        "decisions": decisions,
        "patterns": patterns,
        "edges": edges,
        "layout_version": layout.version,
    })))
}

// ── PUT /api/layout ────────────────────────────────────────────────────────

pub(crate) async fn put_layout(
    State(state): State<Arc<MapState>>,
    Json(body): Json<PutLayout>,
) -> ApiResult {
    if body.positions.len() > MAX_LAYOUT_POSITIONS {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("positions exceeds {MAX_LAYOUT_POSITIONS} entry limit"),
        ));
    }
    for (name, pos) in &body.positions {
        if !pos.x.is_finite() || !pos.y.is_finite() {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!("non-finite position for `{name}`"),
            ));
        }
    }
    let mut layout = state.write_layout();
    if body.layout_version != layout.version {
        return Err(api_err(
            StatusCode::CONFLICT,
            format!(
                "layout version mismatch: expected {}, got {}",
                layout.version, body.layout_version
            ),
        ));
    }
    layout.positions = body.positions;
    layout.version += 1;
    let snapshot = layout.clone();
    drop(layout);

    let root = state.store.root().to_path_buf();
    if let Err(e) = super::layout::save(&root, &snapshot) {
        return Err(api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("layout save failed: {e}"),
        ));
    }

    Ok(Json(json!({ "layout_version": snapshot.version })))
}

// ── POST /api/layout/reset ────────────────────────────────────────────────

pub(crate) async fn reset_layout(State(state): State<Arc<MapState>>) -> ApiResult {
    let mut layout = state.write_layout();
    layout.positions.clear();
    layout.version += 1;
    let snapshot = layout.clone();
    drop(layout);

    let root = state.store.root().to_path_buf();
    if let Err(e) = super::layout::save(&root, &snapshot) {
        return Err(api_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("layout save failed: {e}"),
        ));
    }

    Ok(Json(json!({ "layout_version": snapshot.version })))
}

// ── POST /api/component ────────────────────────────────────────────────────

pub(crate) async fn post_component(
    State(state): State<Arc<MapState>>,
    Json(body): Json<CreateComponent>,
) -> ApiResult {
    let map_state = state.clone();
    tokio::task::spawn_blocking(move || write_component(map_state, body))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn write_component(state: Arc<MapState>, body: CreateComponent) -> ApiResult {
    check_field_len("name", &body.name)?;
    check_field_len("description", &body.description)?;

    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();
    state
        .store
        .add_component(&lock, &mut ps, &body.name, &body.description)
        .map_err(|e| api_err(StatusCode::CONFLICT, e.to_string()))?;

    let name = body.name;
    ws::broadcast(
        &state.ws_tx,
        &[WsEvent::NodeAdded {
            node: super::diff::NodeSnapshot {
                name: name.clone(),
                kind: NodeKind::Component.as_str().into(),
                tags: vec![],
            },
        }],
    );

    Ok(Json(json!({ "name": name })))
}

// ── POST /api/connection ───────────────────────────────────────────────────

pub(crate) async fn post_connection(
    State(state): State<Arc<MapState>>,
    Json(body): Json<CreateConnection>,
) -> ApiResult {
    let map_state = state.clone();
    tokio::task::spawn_blocking(move || write_connection(map_state, body))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn write_connection(state: Arc<MapState>, body: CreateConnection) -> ApiResult {
    check_field_len("from", &body.from)?;
    check_field_len("to", &body.to)?;

    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();
    state
        .store
        .add_connection(&lock, &mut ps, &body.from, &body.to)
        .map_err(|e| api_err(StatusCode::CONFLICT, e.to_string()))?;

    ws::broadcast(
        &state.ws_tx,
        &[WsEvent::EdgeAdded {
            edge: super::diff::EdgeSnapshot {
                from: body.from,
                to: body.to,
                kind: EdgeKind::ConnectsTo.as_str().into(),
            },
        }],
    );

    Ok(Json(json!({ "ok": true })))
}

// ── PUT /api/decision/:name ────────────────────────────────────────────────

pub(crate) async fn put_decision(
    State(state): State<Arc<MapState>>,
    Path(name): Path<String>,
    Json(body): Json<ReviseDecision>,
) -> ApiResult {
    let map_state = state.clone();
    tokio::task::spawn_blocking(move || revise_decision(map_state, name, body))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn revise_decision(state: Arc<MapState>, name: String, body: ReviseDecision) -> ApiResult {
    if body.choice.is_none() && body.reason.is_none() && body.tags.is_none() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "at least one of choice, reason, or tags required",
        ));
    }
    if let Some(ref c) = body.choice {
        check_field_len("choice", c)?;
        if c.len() > MAX_CHOICE_BYTES {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!(
                    "choice must be \u{2264}{MAX_CHOICE_BYTES} characters \
                     ({} given)",
                    c.len()
                ),
            ));
        }
    }
    if let Some(ref r) = body.reason {
        check_field_len("reason", r)?;
        if r.trim().len() < MIN_REASON_BYTES {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!(
                    "reason must be at least {MIN_REASON_BYTES} characters \
                     ({} given)",
                    r.trim().len()
                ),
            ));
        }
    }
    if let Some(ref t) = body.tags {
        if t.len() > MAX_ARRAY_ITEMS {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!("tags exceeds {MAX_ARRAY_ITEMS} item limit"),
            ));
        }
        for tag in t {
            check_field_len("tags[]", tag)?;
        }
    }

    if let Some(ref refs) = body.code_refs {
        store::validate_code_refs(refs)
            .map_err(|e| api_err(StatusCode::BAD_REQUEST, e.to_string()))?;
    }

    // ReviseDecisionParams owns its collections; clone so the post-write
    // broadcast below can still echo the submitted fields.
    let params = store::ReviseDecisionParams {
        choice: body.choice.as_deref(),
        reason: body.reason.as_deref(),
        tags: body.tags.clone(),
        code_refs: body.code_refs.clone(),
    };

    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();
    state
        .store
        .revise_decision(&lock, &mut ps, &name, params)
        .map_err(|e| {
            let status = match e {
                crate::Error::DecisionNotFound(_) => StatusCode::NOT_FOUND,
                crate::Error::Validation(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            api_err(status, e.to_string())
        })?;

    ws::broadcast(
        &state.ws_tx,
        &[WsEvent::NodeUpdated {
            name: name.clone(),
            changes: json!({
                "choice": body.choice,
                "reason": body.reason,
                "tags": body.tags,
            }),
        }],
    );

    Ok(Json(json!({ "name": name })))
}

// ── DELETE /api/component/:name ────────────────────────────────────────────

pub(crate) async fn delete_component(
    State(state): State<Arc<MapState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let map_state = state.clone();
    match tokio::task::spawn_blocking(move || remove_component(map_state, name)).await {
        Ok(r) => r,
        Err(e) => Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

fn remove_component(state: Arc<MapState>, name: String) -> ApiResult {
    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();

    if !ps.components.contains_key(&name) {
        return Err(api_err(StatusCode::NOT_FOUND, "component not found"));
    }

    let cascade = ps.graph().check_component_cascade(&name);
    if cascade.is_blocked() {
        return Err(api_err(StatusCode::CONFLICT, cascade.blocker_summary()));
    }

    state
        .store
        .remove_component(&lock, &mut ps, &name)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    ws::broadcast(&state.ws_tx, &[WsEvent::NodeRemoved { name: name.clone() }]);
    Ok(Json(json!({
        "ok": true,
        "warnings": cascade.warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    })))
}

// ── DELETE /api/decision/:name ─────────────────────────────────────────────

pub(crate) async fn delete_decision(
    State(state): State<Arc<MapState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let map_state = state.clone();
    match tokio::task::spawn_blocking(move || remove_decision(map_state, name)).await {
        Ok(r) => r,
        Err(e) => Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

fn remove_decision(state: Arc<MapState>, name: String) -> ApiResult {
    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();

    if !ps.decisions.contains_key(&name) {
        return Err(api_err(StatusCode::NOT_FOUND, "decision not found"));
    }

    let cascade = ps.graph().check_decision_cascade(&name);
    if cascade.is_blocked() {
        return Err(api_err(StatusCode::CONFLICT, cascade.blocker_summary()));
    }

    state
        .store
        .remove_decision(&lock, &mut ps, &name)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    ws::broadcast(&state.ws_tx, &[WsEvent::NodeRemoved { name: name.clone() }]);
    Ok(Json(json!({
        "ok": true,
        "warnings": cascade.warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    })))
}

// ── DELETE /api/connection/:from/:to ───────────────────────────────────────

pub(crate) async fn delete_connection(
    State(state): State<Arc<MapState>>,
    Path((from, to)): Path<(String, String)>,
) -> impl IntoResponse {
    let map_state = state.clone();
    match tokio::task::spawn_blocking(move || remove_connection(map_state, from, to)).await {
        Ok(r) => r,
        Err(e) => Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

fn remove_connection(state: Arc<MapState>, from: String, to: String) -> ApiResult {
    let lock = state
        .store
        .try_lock()
        .map_err(|e| api_err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let mut ps = state.write_project_state();
    state
        .store
        .remove_connection(&lock, &mut ps, &from, &to)
        .map_err(|e| api_err(StatusCode::NOT_FOUND, e.to_string()))?;

    ws::broadcast(
        &state.ws_tx,
        &[WsEvent::EdgeRemoved {
            from,
            to,
            kind: EdgeKind::ConnectsTo.as_str().into(),
        }],
    );
    Ok(Json(json!({ "ok": true })))
}
