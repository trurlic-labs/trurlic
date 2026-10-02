//! `remove_pattern`: the MCP path to [`Store::remove_pattern`], shared with
//! `trurlic remove pattern`.

use serde_json::Value;

use crate::store::{ProjectState, Store};

use super::write::require_str;

pub(crate) fn remove_pattern(
    store: &Store,
    state: &mut ProjectState,
    args: &Value,
) -> Result<Value, String> {
    let name = require_str(args, "name")?;
    let lock = store.lock().map_err(|e| e.to_string())?;
    store
        .remove_pattern(&lock, state, name)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "name": name, "removed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::write::{record_decision, record_pattern};
    use crate::store::testing::setup_store_with_components;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn remove_pattern_tool_removes_pattern_and_reports_unknown_names() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state) = setup_store_with_components(tmp.path(), &[("auth", "Auth")]);
        for choice in ["Use JWT", "Rotate keys"] {
            let args = json!({
                "component": "auth",
                "choice": choice,
                "reason": "Reasoning long enough",
                "attribution": "user",
            });
            record_decision(&store, &mut state, &args).unwrap();
        }
        let args = json!({
            "name": "Token hygiene",
            "description": "Coordinated token handling",
            "decisions": ["use-jwt", "rotate-keys"],
        });
        let slug = record_pattern(&store, &mut state, &args).unwrap()["name"]
            .as_str()
            .unwrap()
            .to_owned();

        let removed = remove_pattern(&store, &mut state, &json!({ "name": slug })).unwrap();
        assert_eq!(removed["removed"], true);
        assert!(!state.patterns.contains_key(slug.as_str()));

        let err = remove_pattern(&store, &mut state, &json!({ "name": slug })).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }
}
