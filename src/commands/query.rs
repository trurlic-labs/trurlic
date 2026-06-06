//! Read-only query commands: `status` and `check`.

use std::path::Path;

use crate::{Error, Result};

use super::{discover_store, open_store};

/// Print project summary: component count, decision count, any issues.
pub fn status(cwd: &Path) -> Result<()> {
    let (_store, state) = open_store(cwd)?;

    let project_wide = state
        .decisions
        .values()
        .filter(|d| d.decision.component == "project")
        .count();

    println!("project: {}", state.project.project.name);
    println!("components: {}", state.components.len());
    println!(
        "decisions: {} ({} project-wide)",
        state.decisions.len(),
        project_wide
    );

    let issues = state.validate();
    if !issues.is_empty() {
        println!("issues: {}", issues.len());
    }

    Ok(())
}

/// Validate `.trurl/` internal consistency.
pub fn check(cwd: &Path) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, add_connection, init};
    use crate::store::{COMPONENTS_DIR, ComponentFile, STORE_DIR};
    use std::fs;
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
        check(tmp.path()).unwrap();
    }

    #[test]
    fn check_catches_hand_edited_corruption() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();

        let path = tmp
            .path()
            .join(STORE_DIR)
            .join(COMPONENTS_DIR)
            .join("auth.toml");
        let mut comp: ComponentFile = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        comp.component.connects_to.push("nonexistent".into());
        fs::write(&path, toml::to_string_pretty(&comp).unwrap()).unwrap();

        let err = check(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
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

        check(tmp.path()).unwrap();

        rename_component(tmp.path(), "conversation", "design-engine").unwrap();
        check(tmp.path()).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let cli = store.read_component("cli").unwrap();
        assert!(
            cli.component
                .connects_to
                .contains(&"design-engine".to_string())
        );
        assert!(
            !cli.component
                .connects_to
                .contains(&"conversation".to_string())
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
