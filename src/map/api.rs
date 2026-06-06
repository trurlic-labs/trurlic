//! REST API handlers for the map server.
//!
//! All write operations follow the same pattern as CLI commands:
//! lock → load → mutate → validate → write atomically → release.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::store::schema::{Component, ComponentFile};
use crate::store::{Store, is_valid_kebab_case};

use super::AppState;

// ── Error type ────────────────────────────────────────────────────────────

pub(super) enum ApiError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<crate::Error> for ApiError {
    fn from(e: crate::Error) -> Self {
        match e {
            crate::Error::Validation(m) => Self::BadRequest(m),
            crate::Error::InvalidName(n) => Self::BadRequest(format!("invalid name: {n}")),
            crate::Error::LockTimeout { .. } => Self::Internal(e.to_string()),
            other => Self::Internal(other.to_string()),
        }
    }
}

// ── GET /api/state ────────────────────────────────────────────────────────

pub(super) async fn get_state(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let store = Store::at(state.store_root.to_path_buf());
    let project = store.load_state().map_err(ApiError::from)?;

    let components: Vec<Value> = project
        .components
        .iter()
        .map(|(name, c)| {
            serde_json::json!({
                "name": name,
                "description": c.component.description,
                "connects_to": c.component.connects_to,
            })
        })
        .collect();

    let decisions: Vec<Value> = project
        .decisions
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name,
                "component": d.decision.component,
                "choice": d.decision.choice,
                "reason": d.decision.reason,
                "alternatives": d.decision.alternatives,
                "created": d.decision.created.to_rfc3339(),
                "supersedes": d.decision.supersedes,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "project": {
            "name": project.project.project.name,
            "description": project.project.project.description,
        },
        "components": components,
        "decisions": decisions,
    })))
}

// ── POST /api/components ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AddComponentReq {
    name: String,
    #[serde(default)]
    description: String,
}

pub(super) async fn add_component(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddComponentReq>,
) -> Result<StatusCode, ApiError> {
    if !is_valid_kebab_case(&req.name) {
        return Err(ApiError::BadRequest(
            "name must be kebab-case (lowercase, digits, hyphens)".into(),
        ));
    }

    let store = Store::at(state.store_root.to_path_buf());
    let lock = store.lock().map_err(ApiError::from)?;
    let mut project = store.load_state().map_err(ApiError::from)?;

    if project.components.contains_key(&req.name) {
        return Err(ApiError::Conflict(format!(
            "component `{}` already exists",
            req.name
        )));
    }

    let comp = ComponentFile {
        component: Component {
            name: req.name.clone(),
            description: req.description,
            connects_to: vec![],
        },
    };

    project.components.insert(req.name.clone(), comp.clone());
    let issues = project.validate();
    if !issues.is_empty() {
        return Err(ApiError::BadRequest(issues.join("; ")));
    }

    store
        .write_atomic(&lock, &store.component_path(&req.name), &comp)
        .map_err(ApiError::from)?;

    Ok(StatusCode::CREATED)
}

// ── DELETE /api/components/:name ──────────────────────────────────────────

pub(super) async fn remove_component(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let store = Store::at(state.store_root.to_path_buf());
    let lock = store.lock().map_err(ApiError::from)?;
    let project = store.load_state().map_err(ApiError::from)?;

    if !project.components.contains_key(&name) {
        return Err(ApiError::NotFound(format!(
            "component `{name}` does not exist"
        )));
    }

    // Refuse if decisions reference this component.
    let refs: Vec<&str> = project
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == name)
        .map(|(n, _)| n.as_str())
        .collect();

    if !refs.is_empty() {
        return Err(ApiError::Conflict(format!(
            "cannot remove `{name}`: referenced by decisions: {}",
            refs.join(", ")
        )));
    }

    store
        .remove_file(&lock, &store.component_path(&name))
        .map_err(ApiError::from)?;

    // Also clean up incoming connections from other components.
    for (other_name, other_comp) in &project.components {
        if other_comp.component.connects_to.contains(&name) {
            let mut updated = other_comp.clone();
            updated.component.connects_to.retain(|t| t != &name);
            store
                .write_atomic(&lock, &store.component_path(other_name), &updated)
                .map_err(ApiError::from)?;
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

// ── POST /api/connections ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct AddConnectionReq {
    from: String,
    to: String,
}

pub(super) async fn add_connection(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddConnectionReq>,
) -> Result<StatusCode, ApiError> {
    if req.from == req.to {
        return Err(ApiError::BadRequest("self-connections not allowed".into()));
    }

    let store = Store::at(state.store_root.to_path_buf());
    let lock = store.lock().map_err(ApiError::from)?;
    let project = store.load_state().map_err(ApiError::from)?;

    let comp = project
        .components
        .get(&req.from)
        .ok_or_else(|| ApiError::NotFound(format!("component `{}` does not exist", req.from)))?;

    if !project.components.contains_key(&req.to) {
        return Err(ApiError::NotFound(format!(
            "component `{}` does not exist",
            req.to
        )));
    }

    if comp.component.connects_to.contains(&req.to) {
        return Err(ApiError::Conflict("connection already exists".into()));
    }

    let mut updated = comp.clone();
    updated.component.connects_to.push(req.to.clone());

    store
        .write_atomic(&lock, &store.component_path(&req.from), &updated)
        .map_err(ApiError::from)?;

    Ok(StatusCode::CREATED)
}

// ── DELETE /api/connections/:from/:to ─────────────────────────────────────

pub(super) async fn remove_connection(
    State(state): State<Arc<AppState>>,
    Path((from, to)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let store = Store::at(state.store_root.to_path_buf());
    let lock = store.lock().map_err(ApiError::from)?;
    let project = store.load_state().map_err(ApiError::from)?;

    let comp = project
        .components
        .get(&from)
        .ok_or_else(|| ApiError::NotFound(format!("component `{from}` does not exist")))?;

    if !comp.component.connects_to.contains(&to) {
        return Err(ApiError::NotFound(format!(
            "connection {from} → {to} does not exist"
        )));
    }

    let mut updated = comp.clone();
    updated.component.connects_to.retain(|t| t != &to);

    store
        .write_atomic(&lock, &store.component_path(&from), &updated)
        .map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}
