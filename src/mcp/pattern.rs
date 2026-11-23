//! `remove_pattern`: the MCP path to [`Writer::remove_pattern`], shared with
//! `trurlic remove pattern`.

use serde_json::Value;

use crate::store::Writer;

use super::write::require_str;

pub(super) fn remove_pattern(writer: &mut Writer<'_>, args: &Value) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    writer.remove_pattern(name).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "name": name, "removed": true }))
}
