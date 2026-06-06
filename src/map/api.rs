//! REST API handlers for the map server.
//!
//! All write operations follow the same pattern as CLI commands:
//! lock → load → mutate in memory → validate → write atomically → release.

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
            crate::Error::LockTimeout { timeout_secs, .. } => {
                Self::Internal(format!("could not acquire lock within {timeout_secs}s"))
            }
            other => {
                // Log full detail to stderr for the operator; return
                // a generic message to the client to avoid leaking
                // internal paths or implementation details.
                eprintln!("trurl map: internal error: {other}");
                Self::Internal("internal server error".into())
            }
        }
    }
}

// ── GET /api/state ────────────────────────────────────────────────────────

pub(super) async fn get_state(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let store = Store::at(state.store_root.to_path_buf());
    let project = store.load_state().map_err(ApiError::from)?;
    Ok(Json(super::serialize_state(&project)))
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
    let mut project = store.load_state().map_err(ApiError::from)?;

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

    // Collect components that connect TO the removed component.
    let affected: Vec<String> = project
        .components
        .iter()
        .filter(|(comp_name, comp)| {
            *comp_name != &name && comp.component.connects_to.iter().any(|t| t == &name)
        })
        .map(|(comp_name, _)| comp_name.clone())
        .collect();

    // Mutate state in memory.
    project.components.remove(&name);
    for comp in project.components.values_mut() {
        comp.component.connects_to.retain(|t| t != &name);
    }

    // Validate mutated state.
    let issues = project.validate();
    if !issues.is_empty() {
        return Err(ApiError::BadRequest(issues.join("; ")));
    }

    // Atomic batch commit: write updated connection files + remove component.
    let mut writes = Vec::new();
    for comp_name in &affected {
        writes.push(
            store
                .prepare_write(
                    &store.component_path(comp_name),
                    &project.components[comp_name.as_str()],
                )
                .map_err(ApiError::from)?,
        );
    }
    let removes = vec![store.component_path(&name)];
    store
        .commit_batch(&lock, writes, removes)
        .map_err(ApiError::from)?;

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
    let mut project = store.load_state().map_err(ApiError::from)?;

    if !project.components.contains_key(&req.from) {
        return Err(ApiError::NotFound(format!(
            "component `{}` does not exist",
            req.from
        )));
    }
    if !project.components.contains_key(&req.to) {
        return Err(ApiError::NotFound(format!(
            "component `{}` does not exist",
            req.to
        )));
    }
    if project.components[&req.from]
        .component
        .connects_to
        .contains(&req.to)
    {
        return Err(ApiError::Conflict("connection already exists".into()));
    }

    // Mutate in memory, validate full state, then write.
    let comp = project
        .components
        .get_mut(&req.from)
        .ok_or_else(|| ApiError::NotFound(format!("component `{}` does not exist", req.from)))?;
    comp.component.connects_to.push(req.to.clone());

    let issues = project.validate();
    if !issues.is_empty() {
        return Err(ApiError::BadRequest(issues.join("; ")));
    }

    store
        .write_atomic(
            &lock,
            &store.component_path(&req.from),
            &project.components[&req.from],
        )
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
    let mut project = store.load_state().map_err(ApiError::from)?;

    let comp = project
        .components
        .get_mut(&from)
        .ok_or_else(|| ApiError::NotFound(format!("component `{from}` does not exist")))?;

    if !comp.component.connects_to.contains(&to) {
        return Err(ApiError::NotFound(format!(
            "connection {from} → {to} does not exist"
        )));
    }

    comp.component.connects_to.retain(|t| t != &to);

    let issues = project.validate();
    if !issues.is_empty() {
        return Err(ApiError::BadRequest(issues.join("; ")));
    }

    store
        .write_atomic(
            &lock,
            &store.component_path(&from),
            &project.components[&from],
        )
        .map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}
