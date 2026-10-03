use std::path::Path;

use crate::console::{diag, out};
use crate::store::graph::{InMemoryGraph, Severity};
use crate::store::{self, format_code_refs};
use crate::{Error, Result};

use super::{discover_store, open_store};

/// `trurlic status` — print a one-screen summary of the graph: component,
/// decision (with project-wide count), pattern, and edge totals, plus a
/// consistency-issue count when the graph does not validate cleanly.
pub fn status(cwd: &Path) -> Result<()> {
    let (_store, state) = open_store(cwd)?;

    let project_wide = state
        .decisions
        .values()
        .filter(|d| d.decision.component == "project")
        .count();

    let edge_count = state.graph_index.edges.len();

    out!("project: {}", state.project.project.name)?;
    out!("components: {}", state.components.len())?;
    out!(
        "decisions: {} ({} project-wide)",
        state.decisions.len(),
        project_wide
    )?;
    out!("patterns: {}", state.patterns.len())?;
    out!("edges: {edge_count}")?;

    let issues = state.validate();
    if !issues.is_empty() {
        out!("issues: {}", issues.len())?;
    }

    Ok(())
}

/// `trurlic query file <path>` — list every decision whose `code_refs`
/// reference `path` (exact file match or directory prefix), with attribution
/// and the matching refs. The query path is normalized and traversal-checked
/// at the trust boundary before lookup.
pub fn query_file(cwd: &Path, path: &str) -> Result<()> {
    let normalized = store::normalize_file_query(path)?;
    let (_store, state) = open_store(cwd)?;

    let graph = state.graph();
    let matches = graph.decisions_for_file(&normalized);

    if matches.is_empty() {
        out!("No decisions reference `{normalized}`.")?;
        return Ok(());
    }

    out!("{} decision(s) constrain `{normalized}`:\n", matches.len())?;

    for (name, dec) in &matches {
        let attr_suffix = match dec.decision.attribution {
            store::schema::Attribution::Agent => " (agent — unreviewed)",
            store::schema::Attribution::User => "",
        };
        out!(
            "  [{component}] {name}{attr_suffix}",
            component = dec.decision.component
        )?;
        out!("    {}", dec.decision.choice)?;
        let matching_refs = InMemoryGraph::matching_refs_for_decision(dec, &normalized);
        if !matching_refs.is_empty() {
            let refs_vec: Vec<_> = matching_refs.into_iter().cloned().collect();
            out!("    refs: {}", format_code_refs(&refs_vec))?;
        }
        out!()?;
    }

    Ok(())
}

/// `trurlic check` — verify content hashes against `graph.toml`, then validate
/// full graph integrity, exiting non-zero on any error.
pub(crate) fn check(cwd: &Path) -> Result<()> {
    let store = discover_store(cwd)?;

    // Phase 1: verify hashes against the raw graph.toml before load_state
    // reconciles them. This surfaces files edited outside Trurlic.
    let hash_issues = store.verify_hashes()?;

    // Phase 2: load (which reconciles) and run structural validation.
    let state = store.load_state()?;
    let structural_issues = state.validate();

    let all_issues: Vec<_> = hash_issues.iter().chain(structural_issues.iter()).collect();

    if all_issues.is_empty() {
        out!(".trurlic/ is consistent")?;
        return Ok(());
    }

    let error_count = all_issues
        .iter()
        .filter(|i| i.severity() == Severity::Error)
        .count();
    for issue in &all_issues {
        let prefix = match issue.severity() {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        diag!("  {prefix}: {}", issue.message);
    }

    if error_count > 0 {
        Err(Error::CheckFailed(error_count))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, add_connection, decide, init};
    use crate::store::Store;
    use crate::store::graph::IssueKind;
    use crate::store::schema::EdgeKind;
    use tempfile::TempDir;

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
        check(tmp.path()).unwrap();
    }

    // ── verify_hashes ──────────────────────────────────────────────────

    #[test]
    fn verify_hashes_clean_state() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless.", &[], &[]).unwrap();

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
            issues
                .iter()
                .any(|i| i.kind == IssueKind::HashMismatch && i.subject == "auth"),
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
            issues
                .iter()
                .any(|i| i.kind == IssueKind::NodeFileMissing && i.subject == "auth"),
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
            issues.iter().any(|i| i.kind == IssueKind::IndexMissing),
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
        check(tmp.path()).unwrap();
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
        decide(tmp.path(), "cli", "clap derive", "Typed flags", &[], &[]).unwrap();

        check(tmp.path()).unwrap();

        rename_component(tmp.path(), "conversation", "design-engine").unwrap();
        check(tmp.path()).unwrap();

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
        check(tmp.path()).unwrap();

        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 4);
        assert_eq!(state.decisions.len(), 2);
        assert!(state.validate().is_empty());
    }
}
