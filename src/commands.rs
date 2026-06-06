//! Command handlers for the Trurl CLI.
//!
//! Each public function corresponds to a CLI subcommand. All take a working
//! directory to enable testing without mutating process-global state.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::schema::{
    COMPONENTS_DIR, Component, ComponentFile, DECISIONS_DIR, FORMAT_VERSION, Project, ProjectFile,
    STATE_DIR, STORE_DIR,
};
use crate::store::{self, Store};
use crate::{Error, Result};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Open an existing store with version check and crash recovery.
fn open_store(cwd: &Path) -> Result<Store> {
    let store = Store::discover(cwd)?;
    store.check_version()?;
    let stale = store.clean_stale_tmp()?;
    if stale > 0 {
        eprintln!("warning: cleaned {stale} stale temp file(s) from interrupted write");
    }
    Ok(store)
}

// ── init ─────────────────────────────────────────────────────────────────────

/// Create a new `.trurl/` directory in `cwd`.
pub fn init(cwd: &Path) -> Result<()> {
    let root = cwd.join(STORE_DIR);
    if root.exists() {
        return Err(Error::StoreExists(root));
    }

    fs::create_dir_all(root.join(COMPONENTS_DIR))?;
    fs::create_dir_all(root.join(DECISIONS_DIR))?;
    fs::create_dir_all(root.join(STATE_DIR).join("sessions"))?;
    fs::create_dir_all(root.join(STATE_DIR).join("tmp"))?;

    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let project = ProjectFile {
        trurl_version: FORMAT_VERSION.into(),
        project: Project {
            name,
            description: String::new(),
        },
    };

    fs::write(root.join("project.toml"), toml::to_string_pretty(&project)?)?;

    append_gitignore(cwd)?;
    println!("Initialized .trurl/");
    Ok(())
}

/// Ensure `.trurl/.state/` is in `.gitignore` (create or append).
fn append_gitignore(cwd: &Path) -> Result<()> {
    let path = cwd.join(".gitignore");
    let entry = ".trurl/.state/";

    if path.exists() {
        let content = fs::read_to_string(&path)?;
        if content.lines().any(|line| line.trim() == entry) {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        if !content.is_empty() && !content.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "{entry}")?;
    } else {
        fs::write(&path, format!("{entry}\n"))?;
    }
    Ok(())
}

// ── add component ────────────────────────────────────────────────────────────

/// Add a new component to `.trurl/`.
pub fn add_component(cwd: &Path, name: &str) -> Result<()> {
    if !store::is_valid_kebab_case(name) {
        return Err(Error::InvalidName(name.into()));
    }

    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if state.components.contains_key(name) {
        return Err(Error::Validation(format!(
            "component `{name}` already exists"
        )));
    }

    let comp = ComponentFile {
        component: Component {
            name: name.into(),
            description: String::new(),
            connects_to: vec![],
        },
    };

    store.write_atomic(&lock, &store.component_path(name), &comp)?;
    println!("Added component `{name}`");
    Ok(())
}

// ── add connection ───────────────────────────────────────────────────────────

/// Connect two existing components (directional: from → to).
pub fn add_connection(cwd: &Path, from: &str, to: &str) -> Result<()> {
    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if !state.components.contains_key(from) {
        return Err(Error::Validation(format!(
            "component `{from}` does not exist"
        )));
    }
    if !state.components.contains_key(to) {
        return Err(Error::Validation(format!(
            "component `{to}` does not exist"
        )));
    }
    if from == to {
        return Err(Error::Validation(format!(
            "component `{from}` cannot connect to itself"
        )));
    }

    let mut comp = state.components[from].clone();
    if comp.component.connects_to.iter().any(|t| t == to) {
        return Err(Error::Validation(format!(
            "connection `{from}` → `{to}` already exists"
        )));
    }

    comp.component.connects_to.push(to.into());
    store.write_atomic(&lock, &store.component_path(from), &comp)?;
    println!("Connected `{from}` → `{to}`");
    Ok(())
}

// ── status ───────────────────────────────────────────────────────────────────

/// Print project summary: component count, decision count, any issues.
pub fn status(cwd: &Path) -> Result<()> {
    let store = open_store(cwd)?;
    let state = store.load_state()?;

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

// ── check ────────────────────────────────────────────────────────────────────

/// Validate `.trurl/` internal consistency.
pub fn check(cwd: &Path) -> Result<()> {
    let store = open_store(cwd)?;
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── init ─────────────────────────────────────────────────────────────

    #[test]
    fn init_creates_directory_structure() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let root = tmp.path().join(STORE_DIR);
        assert!(root.join("project.toml").is_file());
        assert!(root.join(COMPONENTS_DIR).is_dir());
        assert!(root.join(DECISIONS_DIR).is_dir());
        assert!(root.join(STATE_DIR).is_dir());
        assert!(root.join(STATE_DIR).join("sessions").is_dir());
        assert!(root.join(STATE_DIR).join("tmp").is_dir());
    }

    #[test]
    fn init_writes_valid_project_toml() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(STORE_DIR).join("project.toml")).unwrap();
        let project: ProjectFile = toml::from_str(&content).unwrap();
        assert_eq!(project.trurl_version, FORMAT_VERSION);
    }

    #[test]
    fn init_refuses_if_exists() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        let err = init(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::StoreExists(_)));
    }

    #[test]
    fn init_creates_gitignore_when_absent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains(".trurl/.state/"));
    }

    #[test]
    fn init_appends_to_existing_gitignore() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.starts_with("/target/\n"));
        assert!(content.contains(".trurl/.state/"));
    }

    #[test]
    fn init_appends_newline_before_entry_if_missing() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/").unwrap(); // no trailing newline
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains("/target/\n.trurl/.state/\n"));
    }

    #[test]
    fn init_does_not_duplicate_gitignore_entry() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".trurl/.state/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".trurl/.state/").count(), 1);
    }

    // ── add component ────────────────────────────────────────────────────

    #[test]
    fn add_component_creates_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let path = tmp
            .path()
            .join(STORE_DIR)
            .join(COMPONENTS_DIR)
            .join("auth.toml");
        assert!(path.is_file());

        let file: ComponentFile = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.component.name, "auth");
    }

    #[test]
    fn add_component_rejects_invalid_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        assert!(matches!(
            add_component(tmp.path(), "NotKebab").unwrap_err(),
            Error::InvalidName(_)
        ));
        assert!(matches!(
            add_component(tmp.path(), "").unwrap_err(),
            Error::InvalidName(_)
        ));
        assert!(matches!(
            add_component(tmp.path(), "-leading").unwrap_err(),
            Error::InvalidName(_)
        ));
    }

    #[test]
    fn add_component_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let err = add_component(tmp.path(), "auth").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── add connection ───────────────────────────────────────────────────

    #[test]
    fn add_connection_links_components() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let comp = store.read_component("auth").unwrap();
        assert_eq!(comp.component.connects_to, vec!["database"]);
    }

    #[test]
    fn add_connection_rejects_nonexistent_from() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let err = add_connection(tmp.path(), "ghost", "auth").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_nonexistent_to() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let err = add_connection(tmp.path(), "auth", "ghost").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_self() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let err = add_connection(tmp.path(), "auth", "auth").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("cannot connect to itself")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn add_connection_rejects_duplicate() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();

        let err = add_connection(tmp.path(), "auth", "database").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── status ───────────────────────────────────────────────────────────

    #[test]
    fn status_on_empty_project() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        status(tmp.path()).unwrap(); // should not panic or error
    }

    #[test]
    fn status_after_adding_components() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        status(tmp.path()).unwrap();
    }

    // ── check ────────────────────────────────────────────────────────────

    #[test]
    fn check_passes_on_clean_state() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        check(tmp.path()).unwrap();
    }

    #[test]
    fn check_catches_hand_edited_corruption() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        // Simulate hand-editing: add a dangling connection
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

    // ── full bootstrap sequence ──────────────────────────────────────────

    #[test]
    fn bootstrap_sequence() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        add_component(tmp.path(), "decision-store").unwrap();
        add_component(tmp.path(), "cli").unwrap();
        add_component(tmp.path(), "mcp-server").unwrap();
        add_component(tmp.path(), "conversation").unwrap();
        add_component(tmp.path(), "map-server").unwrap();

        add_connection(tmp.path(), "cli", "decision-store").unwrap();
        add_connection(tmp.path(), "cli", "mcp-server").unwrap();
        add_connection(tmp.path(), "cli", "conversation").unwrap();
        add_connection(tmp.path(), "cli", "map-server").unwrap();
        add_connection(tmp.path(), "mcp-server", "decision-store").unwrap();
        add_connection(tmp.path(), "conversation", "decision-store").unwrap();
        add_connection(tmp.path(), "map-server", "decision-store").unwrap();

        check(tmp.path()).unwrap();
        status(tmp.path()).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 5);
        assert!(state.validate().is_empty());
    }
}
