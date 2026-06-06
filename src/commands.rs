//! Command handlers for the Trurl CLI.
//!
//! Each public function corresponds to a CLI subcommand. All take a working
//! directory to enable testing without mutating process-global state.

use std::fs;
use std::io::Write;
use std::path::Path;

use chrono::Utc;

use crate::schema::{
    COMPONENTS_DIR, Component, ComponentFile, DECISIONS_DIR, Decision, DecisionFile,
    FORMAT_VERSION, Project, ProjectFile, STATE_DIR, STORE_DIR,
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

/// Maximum slug length (well under filesystem limits, readable in listings).
const MAX_SLUG_LEN: usize = 60;

/// Convert a free-form choice string into a kebab-case filename stem.
///
/// Lowercase, replace non-alphanumeric runs with single hyphens,
/// trim edges, truncate at a word boundary.
fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut prev_hyphen = true; // suppress leading hyphen

    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            slug.push('-');
            prev_hyphen = true;
        }
    }

    // Trim trailing hyphen
    while slug.ends_with('-') {
        slug.pop();
    }

    // Truncate at word boundary if too long
    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        if let Some(last_hyphen) = slug.rfind('-') {
            slug.truncate(last_hyphen);
        }
        while slug.ends_with('-') {
            slug.pop();
        }
    }

    if slug.is_empty() {
        slug.push_str("decision");
    }

    slug
}

/// Find a unique decision filename stem, appending `-2`, `-3`, ... on collision.
fn unique_decision_stem(store: &Store, base: &str) -> String {
    if !store.decision_path(base).exists() {
        return base.to_string();
    }
    for n in 2u32.. {
        let candidate = format!("{base}-{n}");
        if !store.decision_path(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!()
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

// ── remove decision ──────────────────────────────────────────────────────────

/// Remove a decision. Warns if other decisions supersede it (broken chain).
pub fn remove_decision(cwd: &Path, name: &str) -> Result<()> {
    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if !state.decisions.contains_key(name) {
        return Err(Error::Validation(format!(
            "decision `{name}` does not exist"
        )));
    }

    let dependents: Vec<&str> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.supersedes == name)
        .map(|(n, _)| n.as_str())
        .collect();

    if !dependents.is_empty() {
        eprintln!(
            "warning: supersede chain broken — these decisions reference `{name}`: {}",
            dependents.join(", ")
        );
    }

    store.remove_file(&lock, &store.decision_path(name))?;
    println!("Removed decision `{name}`");
    Ok(())
}

// ── remove component ─────────────────────────────────────────────────────────

/// Remove a component. Refuses if any decisions reference it.
/// Cleans up incoming connections from other components.
pub fn remove_component(cwd: &Path, name: &str) -> Result<()> {
    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if !state.components.contains_key(name) {
        return Err(Error::Validation(format!(
            "component `{name}` does not exist"
        )));
    }

    // Refuse if decisions reference this component
    let referencing: Vec<&str> = state
        .decisions
        .iter()
        .filter(|(_, d)| d.decision.component == name)
        .map(|(n, _)| n.as_str())
        .collect();

    if !referencing.is_empty() {
        return Err(Error::Validation(format!(
            "cannot remove component `{name}`: referenced by decisions: {}",
            referencing.join(", ")
        )));
    }

    // Remove from other components' connects_to
    for (comp_name, comp) in &state.components {
        if comp_name == name {
            continue;
        }
        if comp.component.connects_to.iter().any(|t| t == name) {
            let mut updated = comp.clone();
            updated.component.connects_to.retain(|t| t != name);
            store.write_atomic(&lock, &store.component_path(comp_name), &updated)?;
        }
    }

    store.remove_file(&lock, &store.component_path(name))?;
    println!("Removed component `{name}`");
    Ok(())
}

// ── rename component ─────────────────────────────────────────────────────────

/// Rename a component, updating all references atomically.
///
/// Multi-file transaction: writes all updated files, then removes the old
/// component file. On crash mid-operation, `trurl check` detects and reports
/// the inconsistency.
pub fn rename_component(cwd: &Path, old: &str, new: &str) -> Result<()> {
    if !store::is_valid_kebab_case(new) {
        return Err(Error::InvalidName(new.into()));
    }

    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if !state.components.contains_key(old) {
        return Err(Error::Validation(format!(
            "component `{old}` does not exist"
        )));
    }
    if state.components.contains_key(new) {
        return Err(Error::Validation(format!(
            "component `{new}` already exists"
        )));
    }

    // Write new component file with updated name
    let old_comp = &state.components[old];
    let mut renamed = old_comp.clone();
    renamed.component.name = new.into();
    store.write_atomic(&lock, &store.component_path(new), &renamed)?;

    // Update connects_to references in all other components
    for (comp_name, comp) in &state.components {
        if comp_name == old {
            continue;
        }
        if comp.component.connects_to.iter().any(|t| t == old) {
            let mut updated = comp.clone();
            for target in &mut updated.component.connects_to {
                if target == old {
                    *target = new.into();
                }
            }
            store.write_atomic(&lock, &store.component_path(comp_name), &updated)?;
        }
    }

    // Update decision component references
    for (dec_name, dec) in &state.decisions {
        if dec.decision.component == old {
            let mut updated = dec.clone();
            updated.decision.component = new.into();
            store.write_atomic(&lock, &store.decision_path(dec_name), &updated)?;
        }
    }

    // Remove old component file last (all references already point to new)
    store.remove_file(&lock, &store.component_path(old))?;
    println!("Renamed component `{old}` → `{new}`");
    Ok(())
}

// ── decide ───────────────────────────────────────────────────────────────────

/// Record a quick decision without the full Socratic flow.
pub fn decide(
    cwd: &Path,
    component: &str,
    choice: &str,
    reason: &str,
    supersedes: Option<&str>,
) -> Result<()> {
    let store = open_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;

    if component != "project" && !state.components.contains_key(component) {
        return Err(Error::Validation(format!(
            "component `{component}` does not exist"
        )));
    }

    if let Some(sup) = supersedes {
        if !state.decisions.contains_key(sup) {
            return Err(Error::Validation(format!(
                "decision `{sup}` does not exist (cannot supersede)"
            )));
        }
    }

    let stem = unique_decision_stem(&store, &slugify(choice));

    let decision = DecisionFile {
        decision: Decision {
            component: component.into(),
            choice: choice.into(),
            reason: reason.into(),
            alternatives: vec![],
            created: Utc::now(),
            supersedes: supersedes.unwrap_or_default().into(),
        },
    };

    store.write_atomic(&lock, &store.decision_path(&stem), &decision)?;
    println!("Recorded decision `{stem}`");
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

    // ── slugify ──────────────────────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Use Redis"), "use-redis");
        assert_eq!(slugify("JWT with DPoP binding"), "jwt-with-dpop-binding");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Result<T, AppError>"), "result-t-apperror");
        assert_eq!(
            slugify("429 + retry-after header"),
            "429-retry-after-header"
        );
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("one   two---three"), "one-two-three");
        assert_eq!(slugify("---leading"), "leading");
        assert_eq!(slugify("trailing---"), "trailing");
    }

    #[test]
    fn slugify_truncates_at_word_boundary() {
        let long = "a ".repeat(100);
        let slug = slugify(&long);
        assert!(slug.len() <= MAX_SLUG_LEN);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "decision");
        assert_eq!(slugify("!!!"), "decision");
    }

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
        fs::write(tmp.path().join(".gitignore"), "/target/").unwrap();
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

    // ── remove decision ──────────────────────────────────────────────────

    #[test]
    fn remove_decision_deletes_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None).unwrap();

        remove_decision(tmp.path(), "use-jwt").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_decisions().unwrap().is_empty());
    }

    #[test]
    fn remove_decision_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_decision(tmp.path(), "ghost").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_decision_warns_on_broken_supersede_chain() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        decide(tmp.path(), "auth", "Session cookies", "Simple", None).unwrap();
        decide(
            tmp.path(),
            "auth",
            "JWT tokens",
            "Stateless",
            Some("session-cookies"),
        )
        .unwrap();

        // Removing the superseded decision succeeds but warns
        remove_decision(tmp.path(), "session-cookies").unwrap();

        // The superseding decision still exists with a dangling reference
        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-tokens").unwrap();
        assert_eq!(dec.decision.supersedes, "session-cookies");
    }

    // ── remove component ─────────────────────────────────────────────────

    #[test]
    fn remove_component_deletes_file() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        remove_component(tmp.path(), "auth").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(store.list_components().unwrap().is_empty());
    }

    #[test]
    fn remove_component_rejects_nonexistent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = remove_component(tmp.path(), "ghost").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_component_refuses_if_decisions_reference_it() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None).unwrap();

        let err = remove_component(tmp.path(), "auth").unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("cannot remove"));
                assert!(msg.contains("use-jwt"));
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn remove_component_cleans_up_incoming_connections() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        add_component(tmp.path(), "cache").unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "cache", "database").unwrap();

        remove_component(tmp.path(), "database").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let auth = store.read_component("auth").unwrap();
        assert!(auth.component.connects_to.is_empty());
        let cache = store.read_component("cache").unwrap();
        assert!(cache.component.connects_to.is_empty());
        check(tmp.path()).unwrap();
    }

    // ── rename component ─────────────────────────────────────────────────

    #[test]
    fn rename_component_updates_file_and_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        assert!(!store.component_path("auth").exists());
        let comp = store.read_component("authentication").unwrap();
        assert_eq!(comp.component.name, "authentication");
    }

    #[test]
    fn rename_component_rejects_nonexistent_old() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = rename_component(tmp.path(), "ghost", "new").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn rename_component_rejects_existing_new() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "auth2").unwrap();

        let err = rename_component(tmp.path(), "auth", "auth2").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn rename_component_rejects_invalid_new_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        assert!(matches!(
            rename_component(tmp.path(), "auth", "NotKebab").unwrap_err(),
            Error::InvalidName(_)
        ));
    }

    #[test]
    fn rename_component_updates_connections() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        add_component(tmp.path(), "database").unwrap();
        add_connection(tmp.path(), "auth", "database").unwrap();
        add_connection(tmp.path(), "database", "auth").unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let authn = store.read_component("authentication").unwrap();
        assert_eq!(authn.component.connects_to, vec!["database"]);
        let db = store.read_component("database").unwrap();
        assert_eq!(db.component.connects_to, vec!["authentication"]);
        check(tmp.path()).unwrap();
    }

    #[test]
    fn rename_component_updates_decision_references() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless", None).unwrap();
        decide(tmp.path(), "auth", "Use Redis", "Fast sessions", None).unwrap();

        rename_component(tmp.path(), "auth", "authentication").unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        for name in store.list_decisions().unwrap() {
            let dec = store.read_decision(&name).unwrap();
            assert_eq!(dec.decision.component, "authentication");
        }
        check(tmp.path()).unwrap();
    }

    // ── decide ───────────────────────────────────────────────────────────

    #[test]
    fn decide_records_component_decision() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        decide(tmp.path(), "auth", "JWT with DPoP", "Stateless", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-with-dpop").unwrap();
        assert_eq!(dec.decision.component, "auth");
        assert_eq!(dec.decision.choice, "JWT with DPoP");
        assert_eq!(dec.decision.reason, "Stateless");
    }

    #[test]
    fn decide_records_project_wide() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        decide(
            tmp.path(),
            "project",
            "Fail-closed on writes",
            "Never silently succeed with wrong data",
            None,
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let names = store.list_decisions().unwrap();
        assert_eq!(names.len(), 1);

        let dec = store.read_decision(&names[0]).unwrap();
        assert_eq!(dec.decision.component, "project");
    }

    #[test]
    fn decide_rejects_nonexistent_component() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = decide(tmp.path(), "ghost", "x", "y", None).unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn decide_rejects_nonexistent_supersede_target() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let err = decide(tmp.path(), "auth", "x", "y", Some("ghost")).unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn decide_supersedes_existing() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        decide(tmp.path(), "auth", "Session cookies", "Simple", None).unwrap();
        decide(
            tmp.path(),
            "auth",
            "JWT tokens",
            "Stateless",
            Some("session-cookies"),
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt-tokens").unwrap();
        assert_eq!(dec.decision.supersedes, "session-cookies");
    }

    #[test]
    fn decide_deduplicates_filename() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        decide(tmp.path(), "auth", "Use Redis", "Fast", None).unwrap();
        decide(tmp.path(), "auth", "Use Redis", "Also for sessions", None).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let names = store.list_decisions().unwrap();
        assert_eq!(names, vec!["use-redis", "use-redis-2"]);
    }

    #[test]
    fn decide_sets_timestamp() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth").unwrap();

        let before = Utc::now();
        decide(tmp.path(), "auth", "JWT", "Stateless", None).unwrap();
        let after = Utc::now();

        let store = Store::discover(tmp.path()).unwrap();
        let dec = store.read_decision("jwt").unwrap();
        assert!(dec.decision.created >= before);
        assert!(dec.decision.created <= after);
    }

    // ── status ───────────────────────────────────────────────────────────

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

    // ── full bootstrap with mutations ────────────────────────────────────

    #[test]
    fn full_lifecycle() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        // Bootstrap
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

        // Decisions
        decide(
            tmp.path(),
            "project",
            "Rust single binary",
            "No runtime deps",
            None,
        )
        .unwrap();
        decide(
            tmp.path(),
            "decision-store",
            "TOML with serde",
            "Git-diffable",
            None,
        )
        .unwrap();
        decide(tmp.path(), "cli", "clap derive", "Type-safe", None).unwrap();

        check(tmp.path()).unwrap();

        // Rename
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

        // Remove decision then component
        remove_decision(tmp.path(), "clap-derive").unwrap();
        remove_component(tmp.path(), "cli").unwrap();
        check(tmp.path()).unwrap();

        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 4);
        assert_eq!(state.decisions.len(), 2);
        assert!(state.validate().is_empty());
    }
}
