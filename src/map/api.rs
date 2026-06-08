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

use crate::store::schema::{EdgeKind, NodeKind};
use crate::store::{self};

use super::diff::WsEvent;
use super::layout::Position;
use super::{MapState, ws};

// ── Request bodies ─────────────────────────────────────────────────────────

/// Maximum byte length for any single text field from the map API.
/// Matches the MCP server's `MAX_TEXT_ARG_BYTES` bound — same store,
/// same limits, regardless of entry point.
const MAX_FIELD_BYTES: usize = 50_000;

/// Maximum number of tags on a single decision.
const MAX_TAGS: usize = 100;

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
pub(crate) struct AmendDecision {
    choice: Option<String>,
    reason: Option<String>,
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(crate) struct PutLayout {
    positions: std::collections::BTreeMap<String, Position>,
    layout_version: u64,
}

// ── Input validation ──────────────────────────────────────────────────────

fn check_field_len(field: &str, value: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if value.len() > MAX_FIELD_BYTES {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("`{field}` exceeds {MAX_FIELD_BYTES} byte limit"),
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
    let graph = &ps.graph;

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
            json!({
                "name": name,
                "component": d.component,
                "choice": d.choice,
                "reason": d.reason,
                "tags": d.tags,
                "created": d.created.to_rfc3339(),
                "alternatives": d.alternatives,
            })
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
        eprintln!("trurl: layout save failed: {e}");
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
        eprintln!("trurl: layout save failed: {e}");
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

    if !store::is_valid_kebab_case(&body.name) {
        return Err(api_err(StatusCode::BAD_REQUEST, "invalid kebab-case name"));
    }
    if store::is_reserved_node_name(&body.name) {
        return Err(api_err(StatusCode::BAD_REQUEST, "reserved name"));
    }

    let mut ps = state.write_project_state();

    if ps.components.contains_key(&body.name) {
        return Err(api_err(StatusCode::CONFLICT, "component already exists"));
    }
    if ps.decisions.contains_key(&body.name) || ps.patterns.contains_key(&body.name) {
        return Err(api_err(
            StatusCode::CONFLICT,
            "name conflicts with an existing decision or pattern",
        ));
    }

    let comp = crate::store::schema::ComponentFile {
        component: crate::store::schema::Component {
            name: body.name.clone(),
            description: body.description,
        },
    };

    let write = state
        .store
        .prepare_write(&state.store.component_path(&body.name), &comp)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let hash = write.content_hash();

    // Checkpoint for rollback — O(1) since all mutations are appends.
    let checkpoint = ps.graph_checkpoint();

    ps.graph_index.nodes.push(crate::store::schema::NodeEntry {
        name: body.name.clone(),
        kind: crate::store::schema::NodeKind::Component,
        tags: vec![],
        hash,
    });
    ps.components.insert(body.name.clone(), comp);

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            ps.rollback_graph(checkpoint);
            ps.components.remove(&body.name);
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    if let Err(e) = state
        .store
        .commit_with_graph(&lock, vec![write], vec![], &ps)
    {
        ps.rollback_graph(checkpoint);
        ps.components.remove(&body.name);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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

    let mut ps = state.write_project_state();

    if !ps.components.contains_key(&body.from) {
        return Err(api_err(StatusCode::NOT_FOUND, "source component not found"));
    }
    if !ps.components.contains_key(&body.to) {
        return Err(api_err(StatusCode::NOT_FOUND, "target component not found"));
    }
    if body.from == body.to {
        return Err(api_err(StatusCode::BAD_REQUEST, "self-connection"));
    }
    let dup = ps
        .graph_index
        .edges
        .iter()
        .any(|e| e.from == body.from && e.to == body.to && e.kind == EdgeKind::ConnectsTo);
    if dup {
        return Err(api_err(StatusCode::CONFLICT, "connection already exists"));
    }

    let checkpoint = ps.graph_checkpoint();

    ps.graph_index.edges.push(crate::store::schema::EdgeEntry {
        from: body.from.clone(),
        to: body.to.clone(),
        kind: EdgeKind::ConnectsTo,
    });

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            ps.rollback_graph(checkpoint);
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    if let Err(e) = state.store.commit_with_graph(&lock, vec![], vec![], &ps) {
        ps.rollback_graph(checkpoint);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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
    Json(body): Json<AmendDecision>,
) -> ApiResult {
    let map_state = state.clone();
    tokio::task::spawn_blocking(move || amend_decision(map_state, name, body))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn amend_decision(state: Arc<MapState>, name: String, body: AmendDecision) -> ApiResult {
    if body.choice.is_none() && body.reason.is_none() && body.tags.is_none() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "at least one of choice, reason, or tags required",
        ));
    }
    if let Some(ref c) = body.choice {
        check_field_len("choice", c)?;
        if c.trim().is_empty() {
            return Err(api_err(StatusCode::BAD_REQUEST, "choice must not be empty"));
        }
    }
    if let Some(ref r) = body.reason {
        check_field_len("reason", r)?;
        if r.trim().is_empty() {
            return Err(api_err(StatusCode::BAD_REQUEST, "reason must not be empty"));
        }
    }
    if let Some(ref t) = body.tags {
        if t.len() > MAX_TAGS {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                format!("tags exceeds {MAX_TAGS} item limit"),
            ));
        }
        for tag in t {
            check_field_len("tags[]", tag)?;
        }
    }

    let mut ps = state.write_project_state();

    // Build the amended decision as a separate value. State is not mutated
    // until prepare_write succeeds, so a serialization failure leaves the
    // in-memory graph clean.
    let old_dec = ps
        .decisions
        .get(&name)
        .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "decision not found"))?
        .clone();

    let mut amended = old_dec.clone();
    if let Some(ref c) = body.choice {
        amended.decision.choice.clone_from(c);
    }
    if let Some(ref r) = body.reason {
        amended.decision.reason.clone_from(r);
    }
    if let Some(ref t) = body.tags {
        amended.decision.tags.clone_from(t);
    }

    let write = state
        .store
        .prepare_write(&state.store.decision_path(&name), &amended)
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let hash = write.content_hash();

    // Mutate state. Save only the affected node fields for rollback.
    ps.decisions.insert(name.clone(), amended);
    let old_hash = ps.update_node_hash(&name, hash);
    let old_tags = if let Some(ref t) = body.tags {
        ps.graph_index
            .nodes
            .iter_mut()
            .find(|n| n.name == name)
            .map(|n| std::mem::replace(&mut n.tags, t.clone()))
    } else {
        None
    };

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            ps.decisions.insert(name.clone(), old_dec.clone());
            if let Some(h) = old_hash.clone() {
                ps.update_node_hash(&name, h);
            }
            if let Some(t) = old_tags.clone()
                && let Some(n) = ps.graph_index.nodes.iter_mut().find(|n| n.name == name)
            {
                n.tags = t;
            }
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    if let Err(e) = state
        .store
        .commit_with_graph(&lock, vec![write], vec![], &ps)
    {
        if let Some(h) = old_hash {
            ps.update_node_hash(&name, h);
        }
        if let Some(t) = old_tags
            && let Some(n) = ps.graph_index.nodes.iter_mut().find(|n| n.name == name)
        {
            n.tags = t;
        }
        ps.decisions.insert(name, old_dec);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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
    let mut ps = state.write_project_state();

    if !ps.components.contains_key(&name) {
        return Err(api_err(StatusCode::NOT_FOUND, "component not found"));
    }

    let cascade = ps.graph.check_component_cascade(&name);
    if cascade.is_blocked() {
        return Err(api_err(StatusCode::CONFLICT, cascade.blocker_summary()));
    }

    let comp_snapshot = ps.components.remove(&name);
    let removed = ps.remove_graph_node(&name);

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            if let Some(comp) = comp_snapshot {
                ps.components.insert(name.clone(), comp);
            }
            ps.restore_graph_node(removed);
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    let removes = vec![state.store.component_path(&name)];
    if let Err(e) = state.store.commit_with_graph(&lock, vec![], removes, &ps) {
        if let Some(comp) = comp_snapshot {
            ps.components.insert(name, comp);
        }
        ps.restore_graph_node(removed);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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
    let mut ps = state.write_project_state();

    if !ps.decisions.contains_key(&name) {
        return Err(api_err(StatusCode::NOT_FOUND, "decision not found"));
    }

    let cascade = ps.graph.check_decision_cascade(&name);
    if cascade.is_blocked() {
        return Err(api_err(StatusCode::CONFLICT, cascade.blocker_summary()));
    }

    let dec_snapshot = ps.decisions.remove(&name);
    let removed = ps.remove_graph_node(&name);

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            if let Some(dec) = dec_snapshot {
                ps.decisions.insert(name.clone(), dec);
            }
            ps.restore_graph_node(removed);
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    let removes = vec![state.store.decision_path(&name)];
    if let Err(e) = state.store.commit_with_graph(&lock, vec![], removes, &ps) {
        if let Some(dec) = dec_snapshot {
            ps.decisions.insert(name, dec);
        }
        ps.restore_graph_node(removed);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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
    let mut ps = state.write_project_state();

    let existed = ps
        .graph_index
        .edges
        .iter()
        .any(|e| e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo);
    if !existed {
        return Err(api_err(StatusCode::NOT_FOUND, "connection not found"));
    }

    // Save the edge being removed for rollback — one EdgeEntry, not the full index.
    let removed_edge = crate::store::schema::EdgeEntry {
        from: from.clone(),
        to: to.clone(),
        kind: EdgeKind::ConnectsTo,
    };

    ps.graph_index
        .edges
        .retain(|e| !(e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo));

    let lock = match state.store.lock() {
        Ok(lock) => lock,
        Err(e) => {
            ps.graph_index.edges.push(removed_edge);
            return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    if let Err(e) = state.store.commit_with_graph(&lock, vec![], vec![], &ps) {
        ps.graph_index.edges.push(removed_edge);
        return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    ps.rebuild_graph();

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
