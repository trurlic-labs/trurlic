use std::path::Path;

use crate::store::graph::{InMemoryGraph, Severity};
use crate::store::{self, format_code_refs};
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

pub fn query_file(cwd: &Path, path: &str) -> Result<()> {
    let normalized = store::normalize_file_query(path)?;
    let (_store, state) = open_store(cwd)?;

    let graph = state.graph();
    let matches = graph.decisions_for_file(&normalized);

    if matches.is_empty() {
        println!("No decisions reference `{normalized}`.");
        return Ok(());
    }

    println!("{} decision(s) constrain `{normalized}`:\n", matches.len());

    for (name, dec) in &matches {
        let attr_suffix = match dec.decision.attribution {
            store::schema::Attribution::Agent => " (agent — unreviewed)",
            store::schema::Attribution::User => "",
        };
        println!(
            "  [{component}] {name}{attr_suffix}",
            component = dec.decision.component
        );
        println!("    {}", dec.decision.choice);
        let matching_refs = InMemoryGraph::matching_refs_for_decision(dec, &normalized);
        if !matching_refs.is_empty() {
            let refs_vec: Vec<_> = matching_refs.into_iter().cloned().collect();
            println!("    refs: {}", format_code_refs(&refs_vec));
        }
        println!();
    }

    Ok(())
}

pub fn check(cwd: &Path, rebuild: bool) -> Result<()> {
    if rebuild {
        return check_rebuild(cwd);
    }

    let store = discover_store(cwd)?;

    // Phase 1: verify hashes against the raw graph.toml before load_state
    // reconciles them. This surfaces files edited outside Trurlic.
    let hash_issues = store.verify_hashes()?;

    // Phase 2: load (which reconciles) and run structural validation.
    let state = store.load_state()?;
    let structural_issues = state.validate();

    let all_issues: Vec<_> = hash_issues.iter().chain(structural_issues.iter()).collect();

    if all_issues.is_empty() {
        println!(".trurlic/ is consistent");
        return Ok(());
    }

    let error_count = all_issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    for issue in &all_issues {
        let prefix = match issue.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        eprintln!("  {prefix}: {}", issue.message);
    }

    if error_count > 0 {
        Err(Error::CheckFailed(error_count))
    } else {
        Ok(())
    }
}

/// Force-rebuild `graph.toml` from node files.
///
/// Deletes the existing index and reconstructs it from the node directories.
/// Only `BelongsTo` edges can be inferred from `decision.component`; all other
/// edge types (ConnectsTo, DependsOn, Constrains, MemberOf, AppliesTo) are
/// non-inferable and will be lost.
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
    let node_count = state.graph_index.nodes.len();
    let edge_count = state.graph_index.edges.len();
    let issues = state.validate();

    // Move graph_index into the commit; state is consumed.
    store.commit_batch(&lock, vec![], vec![], Some(state.graph_index))?;

    println!("Rebuilt graph.toml from node files: {node_count} nodes, {edge_count} edges");
    let error_count = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();

    if issues.is_empty() {
        println!(".trurlic/ is consistent");
        Ok(())
    } else {
        for issue in &issues {
            let prefix = match issue.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            eprintln!("  {prefix}: {}", issue.message);
        }
        if error_count > 0 {
            Err(Error::CheckFailed(error_count))
        } else {
            Ok(())
        }
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
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[], &[]).unwrap();

        check(tmp.path(), true).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "use-jwt" && e.to == "auth" && e.kind == EdgeKind::BelongsTo)
        );
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::ConnectsTo)
        );
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "database"));
    }

    #[test]
    fn check_rebuild_handles_missing_graph_toml() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        fs::remove_file(store.graph_path()).unwrap();

        check(tmp.path(), true).unwrap();

        let state = store.load_state().unwrap();
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "project"));
    }

    // ── verify_hashes ──────────────────────────────────────────────────

    #[test]
    fn verify_hashes_clean_state() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", &[], &[]).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let issues = store.verify_hashes().unwrap();
        assert!(issues.is_empty(), "clean state should have no hash issues");
    }

    #[test]
    fn verify_hashes_detects_modified_file() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();

        // Tamper with the component file after it's been indexed.
        let path = store.component_path("auth");
        fs::write(
            &path,
            "[component]\nname = \"auth\"\ndescription = \"tampered\"\n",
        )
        .unwrap();

        let issues = store.verify_hashes().unwrap();
        assert!(
            issues.iter().any(|i| i.node.as_deref() == Some("auth")
                && i.message.contains("content changed")),
            "should detect the modified file: {issues:?}"
        );
    }

    #[test]
    fn verify_hashes_detects_missing_file() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();

        // Delete the component file but leave graph.toml intact.
        fs::remove_file(store.component_path("auth")).unwrap();

        let issues = store.verify_hashes().unwrap();
        assert!(
            issues.iter().any(|i| i.node.as_deref() == Some("auth")
                && i.message.contains("missing or unreadable")),
            "should detect the missing file: {issues:?}"
        );
    }

    #[test]
    fn verify_hashes_warns_when_graph_toml_missing() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        fs::remove_file(store.graph_path()).unwrap();

        let issues = store.verify_hashes().unwrap();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("graph.toml is missing")),
            "should warn about missing graph.toml: {issues:?}"
        );
    }

    #[test]
    fn check_reports_hash_mismatches_as_warnings() {
        use std::fs;

        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let path = store.component_path("auth");
        fs::write(
            &path,
            "[component]\nname = \"auth\"\ndescription = \"tampered\"\n",
        )
        .unwrap();

        // check should succeed (warnings only, no errors) but the
        // tampered file will be reported.
        check(tmp.path(), false).unwrap();
    }

    #[test]
    fn check_rebuild_preserves_decision_tags() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        // Record a decision with tags via MCP write path.
        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        store
            .record_decision(
                &lock,
                &mut state,
                crate::store::RecordDecisionParams {
                    component: "auth",
                    choice: "Use JWT",
                    reason: "Stateless",
                    alternatives: &[],
                    depends_on: &[],
                    constrains: &[],
                    tags: &["security".into(), "auth".into()],
                    attribution: crate::store::schema::Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();
        drop(lock);

        // Verify tags are in the decision file.
        let dec = store.read_decision("use-jwt").unwrap();
        assert_eq!(dec.decision.tags, vec!["security", "auth"]);

        // Rebuild graph.toml from scratch.
        check(tmp.path(), true).unwrap();

        // Tags must survive the rebuild because they live in the decision file.
        let state = store.load_state().unwrap();
        let node = state
            .graph_index
            .nodes
            .iter()
            .find(|n| n.name == "use-jwt")
            .expect("decision node must exist after rebuild");
        assert_eq!(
            node.tags,
            vec!["security", "auth"],
            "tags must survive --rebuild"
        );
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
            &[],
            &[],
        )
        .unwrap();
        decide(
            tmp.path(),
            "decision-store",
            "TOML with serde",
            "Git-diffable",
            &[],
            &[],
        )
        .unwrap();
        decide(tmp.path(), "cli", "clap derive", "Type-safe", &[], &[]).unwrap();

        check(tmp.path(), false).unwrap();

        rename_component(tmp.path(), "conversation", "design-engine").unwrap();
        check(tmp.path(), false).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        assert!(
            state.graph_index.edges.iter().any(|e| e.from == "cli"
                && e.to == "design-engine"
                && e.kind == EdgeKind::ConnectsTo)
        );
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
