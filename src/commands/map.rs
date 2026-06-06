//! `trurl map` — start the interactive architecture map.

use std::path::Path;

use crate::Result;

use super::discover_store;

pub fn map(cwd: &Path) -> Result<()> {
    let store = discover_store(cwd)?;
    let state = store.load_state()?;

    let issues = state.validate();
    if !issues.is_empty() {
        eprintln!(
            "warning: .trurl/ has {} consistency issue(s) — run `trurl check`",
            issues.len()
        );
    }

    eprintln!(
        "trurl: loading {} ({} components, {} decisions)",
        state.project.project.name,
        state.components.len(),
        state.decisions.len(),
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::Error::Validation(format!("failed to start runtime: {e}")))?;

    rt.block_on(crate::map::start_server(store.root()))
}
