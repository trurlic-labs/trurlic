//! `remove_pattern`: the MCP path to [`Store::remove_pattern`], shared with
//! `trurlic remove pattern`.

use serde_json::Value;

use crate::store::{ProjectState, Store, StoreLock};

use super::write::require_str;

pub(super) fn remove_pattern(
    store: &Store,
    lock: &StoreLock,
    state: &mut ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    store
        .remove_pattern(lock, state, name)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "name": name, "removed": true }))
}
