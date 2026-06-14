//! Layout state: node positions and optimistic concurrency.
//!
//! `.trurlic/.state/layout.json` stores per-node positions and pinned
//! state. It is `.gitignore`d — layout is personal preference, not
//! architectural fact. The `version` field is an optimistic concurrency
//! counter: `PUT /api/layout` increments it and rejects stale writes
//! with `409 Conflict`.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LayoutState {
    pub version: u64,
    #[serde(default)]
    pub positions: BTreeMap<String, Position>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Position {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub pinned: bool,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            version: 1,
            positions: BTreeMap::new(),
        }
    }
}

// ── I/O ─────────────────────────────────────────────────────────────────────

pub(crate) fn layout_path(store_root: &Path) -> PathBuf {
    store_root.join(".state").join("layout.json")
}

pub(crate) fn load(store_root: &Path) -> LayoutState {
    let path = layout_path(store_root);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(e) if e.kind() == ErrorKind::NotFound => LayoutState::default(),
        Err(e) => {
            eprintln!("trurlic: failed to read layout.json: {e}");
            LayoutState::default()
        }
    }
}

pub(crate) fn save(store_root: &Path, state: &LayoutState) -> Result<(), String> {
    let path = layout_path(store_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("layout dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| format!("layout serialize: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("layout write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("layout rename: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store_root(tmp: &TempDir) -> PathBuf {
        let root = tmp.path().join(".trurlic");
        fs::create_dir_all(root.join(".state")).unwrap();
        root
    }

    #[test]
    fn load_returns_default_when_missing() {
        let tmp = TempDir::new().unwrap();
        let layout = load(&store_root(&tmp));
        assert_eq!(layout.version, 1);
        assert!(layout.positions.is_empty());
    }

    #[test]
    fn round_trip() {
        let tmp = TempDir::new().unwrap();
        let root = store_root(&tmp);

        let mut state = LayoutState::default();
        state.positions.insert(
            "auth".into(),
            Position {
                x: 100.5,
                y: 200.0,
                pinned: true,
            },
        );
        state.version = 3;

        save(&root, &state).unwrap();
        let loaded = load(&root);
        assert_eq!(loaded.version, 3);
        assert_eq!(loaded.positions.len(), 1);
        let pos = &loaded.positions["auth"];
        assert!((pos.x - 100.5).abs() < f64::EPSILON);
        assert!(pos.pinned);
    }

    #[test]
    fn load_recovers_from_corrupt_json() {
        let tmp = TempDir::new().unwrap();
        let root = store_root(&tmp);
        fs::write(layout_path(&root), "not json").unwrap();
        let layout = load(&root);
        assert_eq!(layout.version, 1);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("deep").join(".trurlic");
        save(&root, &LayoutState::default()).unwrap();
        assert!(layout_path(&root).exists());
    }
}
