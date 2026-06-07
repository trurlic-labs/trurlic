use std::path::Path;

use crate::Result;

use super::discover_store;

pub fn serve(cwd: &Path) -> Result<()> {
    let store = discover_store(cwd)?;
    let state = store.load_state()?;

    let issues = state.validate();
    let error_count = issues
        .iter()
        .filter(|i| i.severity == crate::store::graph::Severity::Error)
        .count();
    if error_count > 0 {
        eprintln!("warning: .trurl/ has {error_count} consistency issue(s) — run `trurl check`");
    }

    eprintln!(
        "trurl: serving {} ({} components, {} decisions, {} patterns)",
        state.project.project.name,
        state.components.len(),
        state.decisions.len(),
        state.patterns.len(),
    );

    crate::mcp::run_server(store, state)
}
