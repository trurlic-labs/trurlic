use std::path::Path;

use crate::{Error, Result};

use super::{discover_store, open_store};

pub fn status(cwd: &Path) -> Result<()> {
    let (_store, state) = open_store(cwd)?;

    let project_wide = state
        .decisions
        .values()
        .filter(|d| d.decision.component == "project")
        .count();

    let edge_count = state.graph_index.edges.len();

    println!("project: {}", state.project.project.name);
    println!("components: {}", state.components.len());
    println!(
        "decisions: {} ({} project-wide)",
        state.decisions.len(),
        project_wide
    );
    println!("patterns: {}", state.patterns.len());
    println!("edges: {edge_count}");

    let issues = state.validate();
    if !issues.is_empty() {
        println!("issues: {}", issues.len());
    }

    Ok(())
}

pub fn check(cwd: &Path, rebuild: bool) -> Result<()> {
    if rebuild {
        return check_rebuild(cwd);
    }

    let store = discover_store(cwd)?;
    let state = store.load_state()?;
    let issues = state.validate();

    if issues.is_empty() {
        println!(".trurl/ is consistent");
        Ok(())
    } else {
        for issue in &issues {
            eprintln!("  {issue}");
        }
        Err(Error::Validation(format!(
            "{} consistency issue(s) found",
            issues.len()
        )))
    }
}

/// Force-rebuild `graph.toml` from node files.
///
/// Deletes the existing index and reconstructs it from the node directories.
/// Only `BelongsTo` edges can be inferred from `decision.component`; all other
/// edge types (ConnectsTo, DependsOn, Constrains, Supersedes, MemberOf,
/// AppliesTo) are non-inferable and will be lost.
fn check_rebuild(cwd: &Path) -> Result<()> {
    let store = discover_store(cwd)?;
    let lock = store.lock()?;

    let graph_path = store.graph_path();
    if graph_path.exists() {
        store.remove_file(&lock, &graph_path)?;
    }

    // load_state infers BelongsTo edges from decision.component fields.
    // Non-inferable edges (ConnectsTo, DependsOn, etc.) are not recovered.
    let state = store.load_state()?;
    store.commit_batch(&lock, vec![], vec![], Some(&state.graph_index))?;

    println!(
        "Rebuilt graph.toml from node files: {} nodes, {} edges",
        state.graph_index.nodes.len(),
        state.graph_index.edges.len(),
    );

    let issues = state.validate();
    if issues.is_empty() {
        println!(".trurl/ is consistent");
        Ok(())
    } else {
        for issue in &issues {
            eprintln!("  {issue}");
        }
        Err(Error::Validation(format!(
            "{} consistency issue(s) found after rebuild",
            issues.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, add_connection, decide, init};
    use crate::store::Store;
    use crate::store::schema::EdgeKind;
    use tempfile::TempDir;

    #[test]
    fn status_on_empty_project() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        status(tmp.path()).unwrap();
    }

    #[test]
    fn status_after_adding_components() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        status(tmp.path()).unwrap();
    }

    #[test]
    fn check_passes_on_clean_state() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        check(tmp.path(), false).unwrap();
    }

    // ── check --rebuild ─────────────────────────────────────────────────

    #[test]
    fn check_rebuild_rebuilds_graph() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        add_component(tmp.path(), "database", None).unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None, &[]).unwrap();

        // Rebuild drops non-inferable edges (ConnectsTo).
        check(tmp.path(), true).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        // BelongsTo edge re-inferred from decision.component.
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "use-jwt" && e.to == "auth" && e.kind == EdgeKind::BelongsTo)
        );

        // ConnectsTo is non-inferable — lost after rebuild.
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::ConnectsTo)
        );

        // All node files still present.
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "database"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "use-jwt"));
    }

    #[test]
    fn check_rebuild_handles_missing_graph_toml() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        // Manually delete graph.toml before rebuild.
        let store = Store::discover(tmp.path()).unwrap();
        fs::remove_file(store.graph_path()).unwrap();

        check(tmp.path(), true).unwrap();

        let state = store.load_state().unwrap();
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "project"));
    }

    // ── full lifecycle ───────────────────────────────────────────────────

    #[test]
    fn full_lifecycle() {
        use crate::commands::*;
        use crate::store::Store;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        add_component(tmp.path(), "decision-store", None).unwrap();
        add_component(tmp.path(), "cli", None).unwrap();
        add_component(tmp.path(), "mcp-server", None).unwrap();
        add_component(tmp.path(), "conversation", None).unwrap();
        add_component(tmp.path(), "map-server", None).unwrap();
        add_connection(tmp.path(), "cli", "decision-store").unwrap();
        add_connection(tmp.path(), "cli", "mcp-server").unwrap();
        add_connection(tmp.path(), "cli", "conversation").unwrap();
        add_connection(tmp.path(), "cli", "map-server").unwrap();
        add_connection(tmp.path(), "mcp-server", "decision-store").unwrap();
        add_connection(tmp.path(), "conversation", "decision-store").unwrap();
        add_connection(tmp.path(), "map-server", "decision-store").unwrap();

        decide(
            tmp.path(),
            "project",
            "Rust single binary",
            "No runtime deps",
            None,
            &[],
        )
        .unwrap();
        decide(
            tmp.path(),
            "decision-store",
            "TOML with serde",
            "Git-diffable",
            None,
            &[],
        )
        .unwrap();
        decide(tmp.path(), "cli", "clap derive", "Type-safe", None, &[]).unwrap();

        check(tmp.path(), false).unwrap();

        rename_component(tmp.path(), "conversation", "design-engine").unwrap();
        check(tmp.path(), false).unwrap();

        // Verify connections are now edges in the graph
        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        // cli → design-engine edge should exist (renamed from conversation)
        assert!(
            state.graph_index.edges.iter().any(|e| e.from == "cli"
                && e.to == "design-engine"
                && e.kind == EdgeKind::ConnectsTo)
        );

        // Old name should be gone
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "conversation" || e.to == "conversation")
        );

        remove_decision(tmp.path(), "clap-derive").unwrap();
        remove_component(tmp.path(), "cli").unwrap();
        check(tmp.path(), false).unwrap();

        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 4);
        assert_eq!(state.decisions.len(), 2);
        assert!(state.validate().is_empty());
    }
}
