//! `.trurl/` store operations.
//!
//! Handles all I/O with the `.trurl/` directory: reading, atomic writing,
//! locking, state loading, validation, and crash recovery.
//!
//! # Safety model
//!
//! - **Reads are lock-free.** Writes use atomic renames, so reads always see
//!   a consistent snapshot.
//! - **Writes require a [`StoreLock`].** Passed as proof parameter — the type
//!   system enforces that callers hold the lock before mutating state.
//! - **Atomic writes** go through `.state/tmp/` then `rename(2)`.
//! - **Crash recovery** cleans stale temp files on the next invocation.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::schema::{
    COMPONENTS_DIR, ComponentFile, DECISIONS_DIR, DecisionFile, FORMAT_VERSION, ProjectFile,
    STATE_DIR, STORE_DIR,
};
use crate::{Error, Result};

/// Advisory lock timeout (seconds).
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval while waiting for the lock.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ── Store ────────────────────────────────────────────────────────────────────

/// Handle to a `.trurl/` directory.
///
/// Read methods work without locking. Write methods require a [`StoreLock`]
/// passed as a proof parameter.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Walk up from `start` to find the nearest `.trurl/` directory.
    ///
    /// Canonicalizes the starting path, then checks each ancestor.
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = start.canonicalize()?;
        loop {
            let candidate = current.join(STORE_DIR);
            if candidate.is_dir() {
                return Ok(Self { root: candidate });
            }
            if !current.pop() {
                return Err(Error::StoreNotFound(start.to_path_buf()));
            }
        }
    }

    /// Create a `Store` at the given `.trurl/` path without checking existence.
    ///
    /// Used by `init` before the directory is created.
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    // ── Path helpers ─────────────────────────────────────────────────────

    /// Root `.trurl/` directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn components_dir(&self) -> PathBuf {
        self.root.join(COMPONENTS_DIR)
    }

    fn decisions_dir(&self) -> PathBuf {
        self.root.join(DECISIONS_DIR)
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    /// Path to `components/<name>.toml`.
    pub fn component_path(&self, name: &str) -> PathBuf {
        self.components_dir().join(format!("{name}.toml"))
    }

    /// Path to `decisions/<name>.toml`.
    pub fn decision_path(&self, name: &str) -> PathBuf {
        self.decisions_dir().join(format!("{name}.toml"))
    }

    // ── Locking ──────────────────────────────────────────────────────────

    /// Acquire an exclusive advisory lock on `.trurl/`.
    ///
    /// Times out after 5 seconds. The lock is released when the returned
    /// [`StoreLock`] is dropped.
    pub fn lock(&self) -> Result<StoreLock> {
        fs::create_dir_all(self.state_dir())?;

        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;

        let deadline = Instant::now() + LOCK_TIMEOUT;

        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(StoreLock { _file: file }),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::LockTimeout(LOCK_TIMEOUT.as_secs()));
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    // ── Reading ──────────────────────────────────────────────────────────

    /// Read and deserialize a TOML file.
    fn read_toml<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Read `project.toml`.
    pub fn read_project(&self) -> Result<ProjectFile> {
        self.read_toml(&self.root.join("project.toml"))
    }

    /// Read a component by name. Returns a clear error if it doesn't exist.
    pub fn read_component(&self, name: &str) -> Result<ComponentFile> {
        let path = self.component_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io(e)) if e.kind() == ErrorKind::NotFound => Err(Error::Validation(
                format!("component `{name}` does not exist"),
            )),
            Err(e) => Err(e),
        }
    }

    /// Read a decision by name. Returns a clear error if it doesn't exist.
    pub fn read_decision(&self, name: &str) -> Result<DecisionFile> {
        let path = self.decision_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io(e)) if e.kind() == ErrorKind::NotFound => Err(Error::Validation(
                format!("decision `{name}` does not exist"),
            )),
            Err(e) => Err(e),
        }
    }

    /// List all component names (sorted, without `.toml` extension).
    pub fn list_components(&self) -> Result<Vec<String>> {
        list_toml_stems(&self.components_dir())
    }

    /// List all decision names (sorted, without `.toml` extension).
    pub fn list_decisions(&self) -> Result<Vec<String>> {
        list_toml_stems(&self.decisions_dir())
    }

    /// Load the complete project state into memory.
    pub fn load_state(&self) -> Result<ProjectState> {
        let project = self.read_project()?;

        let mut components = BTreeMap::new();
        for name in self.list_components()? {
            let file: ComponentFile = self.read_toml(&self.component_path(&name))?;
            components.insert(name, file);
        }

        let mut decisions = BTreeMap::new();
        for name in self.list_decisions()? {
            let file: DecisionFile = self.read_toml(&self.decision_path(&name))?;
            decisions.insert(name, file);
        }

        Ok(ProjectState {
            project,
            components,
            decisions,
        })
    }

    // ── Version check ────────────────────────────────────────────────────

    /// Verify the store's format version is compatible with this CLI.
    ///
    /// Refuses on any mismatch — never silently misinterprets.
    pub fn check_version(&self) -> Result<()> {
        let project = self.read_project()?;
        let v = &project.trurl_version;
        if v == FORMAT_VERSION {
            return Ok(());
        }
        Err(Error::Validation(format!(
            ".trurl/ format version `{v}` is not compatible with this CLI \
             (expected `{FORMAT_VERSION}`)"
        )))
    }

    // ── Atomic writing ───────────────────────────────────────────────────

    /// Write `value` to `target` atomically via `.state/tmp/`.
    ///
    /// Serializes to TOML, writes to a temp file, validates by re-reading,
    /// then renames to the final path. Caller **must** hold a [`StoreLock`].
    pub fn write_atomic<T: Serialize + DeserializeOwned>(
        &self,
        _lock: &StoreLock,
        target: &Path,
        value: &T,
    ) -> Result<()> {
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        let filename = target
            .file_name()
            .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
        let tmp_path = tmp_dir.join(filename);

        let content = toml::to_string_pretty(value)?;

        // Write to temp location
        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(Error::Io(e));
        }

        // Validate by re-reading — fail-closed
        let readback = match fs::read_to_string(&tmp_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::Io(e));
            }
        };
        if let Err(e) = toml::from_str::<T>(&readback) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::TomlRead(e));
        }

        // Ensure parent directory exists
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic rename (POSIX guarantees atomicity for same-filesystem rename)
        if let Err(e) = fs::rename(&tmp_path, target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        Ok(())
    }

    /// Remove a file from the store. Caller **must** hold a [`StoreLock`].
    pub fn remove_file(&self, _lock: &StoreLock, target: &Path) -> Result<()> {
        Ok(fs::remove_file(target)?)
    }

    // ── Crash recovery ───────────────────────────────────────────────────

    /// Remove stale temp files left by interrupted writes.
    ///
    /// Returns the number of files cleaned. Called on startup of any command
    /// that reads `.trurl/` to detect interrupted writes.
    pub fn clean_stale_tmp(&self) -> Result<usize> {
        let tmp_dir = self.tmp_dir();
        let entries = match fs::read_dir(&tmp_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(Error::Io(e)),
        };

        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            if entry.path().is_file() {
                match fs::remove_file(entry.path()) {
                    Ok(()) => count += 1,
                    // Another process may have cleaned it concurrently
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        }
        Ok(count)
    }
}

// ── StoreLock ────────────────────────────────────────────────────────────────

/// RAII exclusive lock on `.trurl/`.
///
/// Acquired via [`Store::lock`], released when dropped (closing the file
/// descriptor releases the advisory flock). Passed to write methods as
/// compile-time proof that the caller holds the lock.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

// ── ProjectState ─────────────────────────────────────────────────────────────

/// Complete in-memory snapshot of `.trurl/`.
///
/// Keyed by filename stem (e.g. `"auth"`, `"error-strategy"`).
pub struct ProjectState {
    pub project: ProjectFile,
    pub components: BTreeMap<String, ComponentFile>,
    pub decisions: BTreeMap<String, DecisionFile>,
}

impl ProjectState {
    /// Validate referential integrity. Returns a list of issues (empty = clean).
    ///
    /// Checks performed:
    /// - Filename matches internal component name
    /// - `connects_to` entries reference existing components
    /// - No self-connections
    /// - No duplicate connections
    /// - `decision.component` references existing component or `"project"`
    /// - `decision.supersedes` references existing decision
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        for (filename, comp) in &self.components {
            let name = &comp.component.name;

            // Filename must match internal name
            if filename != name {
                issues.push(format!(
                    "component file `{filename}.toml` has internal name `{name}`"
                ));
            }

            let mut seen = std::collections::HashSet::new();
            for target in &comp.component.connects_to {
                // Must reference existing component
                if !self.components.contains_key(target) {
                    issues.push(format!(
                        "component `{filename}` connects to `{target}` which does not exist"
                    ));
                }
                // No self-connections
                if target == filename {
                    issues.push(format!("component `{filename}` connects to itself"));
                }
                // No duplicate connections
                if !seen.insert(target) {
                    issues.push(format!(
                        "component `{filename}` has duplicate connection to `{target}`"
                    ));
                }
            }
        }

        for (filename, dec) in &self.decisions {
            let comp = &dec.decision.component;
            if comp != "project" && !self.components.contains_key(comp) {
                issues.push(format!(
                    "decision `{filename}` references component `{comp}` which does not exist"
                ));
            }

            let sup = &dec.decision.supersedes;
            if !sup.is_empty() && !self.decisions.contains_key(sup) {
                issues.push(format!(
                    "decision `{filename}` supersedes `{sup}` which does not exist"
                ));
            }
        }

        issues
    }
}

// ── Validation helpers ───────────────────────────────────────────────────────

/// Check whether a name is valid kebab-case.
///
/// Rules: non-empty, lowercase ASCII letters + digits + hyphens only,
/// no leading/trailing/consecutive hyphens.
pub fn is_valid_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// List `.toml` file stems in a directory (sorted). Returns empty on `NotFound`.
fn list_toml_stems(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_toml = path.extension().is_some_and(|ext| ext == "toml");
        if is_toml {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort_unstable();
    Ok(names)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        Component, ComponentFile, Decision, DecisionFile, FORMAT_VERSION, Project, ProjectFile,
    };
    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;

    /// Create a minimal valid `.trurl/` directory and return a `Store` over it.
    fn setup_store(dir: &Path) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let project = ProjectFile {
            trurl_version: FORMAT_VERSION.into(),
            project: Project {
                name: "test-project".into(),
                description: "A test project".into(),
            },
        };
        fs::write(
            root.join("project.toml"),
            toml::to_string_pretty(&project).unwrap(),
        )
        .unwrap();

        Store::at(root)
    }

    fn sample_component(name: &str) -> ComponentFile {
        ComponentFile {
            component: Component {
                name: name.into(),
                description: format!("The {name} component"),
                connects_to: vec![],
            },
        }
    }

    fn sample_decision(name: &str, component: &str) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: format!("Choice for {name}"),
                reason: format!("Reason for {name}"),
                alternatives: vec![],
                created: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
                supersedes: String::new(),
            },
        }
    }

    // ── is_valid_kebab_case ──────────────────────────────────────────────

    #[test]
    fn kebab_valid_names() {
        assert!(is_valid_kebab_case("auth"));
        assert!(is_valid_kebab_case("rate-limiter"));
        assert!(is_valid_kebab_case("mcp-server"));
        assert!(is_valid_kebab_case("a"));
        assert!(is_valid_kebab_case("component1"));
        assert!(is_valid_kebab_case("my-app-v2"));
    }

    #[test]
    fn kebab_rejects_invalid() {
        assert!(!is_valid_kebab_case(""));
        assert!(!is_valid_kebab_case("-leading"));
        assert!(!is_valid_kebab_case("trailing-"));
        assert!(!is_valid_kebab_case("double--hyphen"));
        assert!(!is_valid_kebab_case("UpperCase"));
        assert!(!is_valid_kebab_case("has_underscore"));
        assert!(!is_valid_kebab_case("has space"));
        assert!(!is_valid_kebab_case("has.dot"));
        assert!(!is_valid_kebab_case("special!char"));
        assert!(!is_valid_kebab_case("über"));
    }

    // ── discover ─────────────────────────────────────────────────────────

    #[test]
    fn discover_finds_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        setup_store(tmp.path());

        let store = Store::discover(tmp.path()).unwrap();
        assert_eq!(store.root(), tmp.path().join(STORE_DIR));
    }

    #[test]
    fn discover_finds_in_parent() {
        let tmp = TempDir::new().unwrap();
        setup_store(tmp.path());

        let nested = tmp.path().join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        let store = Store::discover(&nested).unwrap();
        assert_eq!(store.root(), tmp.path().join(STORE_DIR));
    }

    #[test]
    fn discover_fails_when_absent() {
        let tmp = TempDir::new().unwrap();
        let err = Store::discover(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::StoreNotFound(_)));
    }

    // ── lock ─────────────────────────────────────────────────────────────

    #[test]
    fn lock_acquire_and_release() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        {
            let _lock = store.lock().unwrap();
            assert!(store.lock_path().exists());
        }
        // Lock released on drop — lock file remains but is unlocked.
    }

    // ── read / write round-trip ──────────────────────────────────────────

    #[test]
    fn write_and_read_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let path = store.component_path("auth");
        store.write_atomic(&lock, &path, &comp).unwrap();

        let read_back = store.read_component("auth").unwrap();
        assert_eq!(comp, read_back);
    }

    #[test]
    fn write_and_read_decision() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("error-strategy", "project");
        let path = store.decision_path("error-strategy");
        store.write_atomic(&lock, &path, &dec).unwrap();

        let read_back = store.read_decision("error-strategy").unwrap();
        assert_eq!(dec, read_back);
    }

    #[test]
    fn read_missing_component_gives_clear_error() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let err = store.read_component("nonexistent").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("nonexistent")),
            other => panic!("expected Validation error, got: {other}"),
        }
    }

    #[test]
    fn read_missing_decision_gives_clear_error() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let err = store.read_decision("nonexistent").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("nonexistent")),
            other => panic!("expected Validation error, got: {other}"),
        }
    }

    // ── list / load_state ────────────────────────────────────────────────

    #[test]
    fn list_components_sorted() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        for name in &["database", "auth", "rate-limiter"] {
            let comp = sample_component(name);
            store
                .write_atomic(&lock, &store.component_path(name), &comp)
                .unwrap();
        }

        let names = store.list_components().unwrap();
        assert_eq!(names, vec!["auth", "database", "rate-limiter"]);
    }

    #[test]
    fn list_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        assert!(store.list_components().unwrap().is_empty());
        assert!(store.list_decisions().unwrap().is_empty());
    }

    #[test]
    fn load_state_reads_everything() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let auth = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 1);
        assert_eq!(state.decisions.len(), 1);
        assert!(state.components.contains_key("auth"));
        assert!(state.decisions.contains_key("token-format"));
    }

    // ── check_version ────────────────────────────────────────────────────

    #[test]
    fn check_version_passes_on_match() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        store.check_version().unwrap();
    }

    #[test]
    fn check_version_rejects_mismatch() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let project = ProjectFile {
            trurl_version: "99.0.0".into(),
            project: Project {
                name: "future".into(),
                description: "From the future".into(),
            },
        };
        fs::write(
            root.join("project.toml"),
            toml::to_string_pretty(&project).unwrap(),
        )
        .unwrap();

        let store = Store::at(root);
        let err = store.check_version().unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("99.0.0"));
                assert!(msg.contains(FORMAT_VERSION));
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── ProjectState::validate ───────────────────────────────────────────

    #[test]
    fn validate_clean_state() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut auth = sample_component("auth");
        auth.component.connects_to = vec!["database".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_dangling_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut comp = sample_component("auth");
        comp.component.connects_to = vec!["nonexistent".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("nonexistent"));
    }

    #[test]
    fn validate_catches_self_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut comp = sample_component("auth");
        comp.component.connects_to = vec!["auth".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("connects to itself")));
    }

    #[test]
    fn validate_catches_duplicate_connection() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        let mut auth = sample_component("auth");
        auth.component.connects_to = vec!["database".into(), "database".into()];
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("duplicate connection")));
    }

    #[test]
    fn validate_catches_dangling_decision_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("stale-decision", "deleted-component");
        store
            .write_atomic(&lock, &store.decision_path("stale-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("deleted-component") && i.contains("does not exist"))
        );
    }

    #[test]
    fn validate_allows_project_wide_decisions() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("error-strategy", "project");
        store
            .write_atomic(&lock, &store.decision_path("error-strategy"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_dangling_supersedes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("new-choice", "project");
        dec.decision.supersedes = "ghost".into();
        store
            .write_atomic(&lock, &store.decision_path("new-choice"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.contains("ghost")));
    }

    #[test]
    fn validate_catches_filename_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Write component with internal name that differs from filename
        let comp = sample_component("wrong-name");
        store
            .write_atomic(&lock, &store.component_path("actual-file"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("actual-file") && i.contains("wrong-name"))
        );
    }

    // ── crash recovery ───────────────────────────────────────────────────

    #[test]
    fn clean_stale_tmp_removes_leftovers() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        fs::create_dir_all(&tmp_dir).unwrap();
        fs::write(tmp_dir.join("stale.toml"), "leftover").unwrap();
        fs::write(tmp_dir.join("another.toml"), "leftover").unwrap();

        let count = store.clean_stale_tmp().unwrap();
        assert_eq!(count, 2);

        // Second call finds nothing
        assert_eq!(store.clean_stale_tmp().unwrap(), 0);
    }

    #[test]
    fn clean_stale_tmp_no_tmp_dir() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        assert_eq!(store.clean_stale_tmp().unwrap(), 0);
    }

    // ── atomic write guarantees ──────────────────────────────────────────

    #[test]
    fn atomic_write_leaves_no_tmp_on_success() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        // tmp dir should be empty after successful write
        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after atomic write");
        }
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(STORE_DIR);
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        // Don't create components/ — write_atomic should handle it
        let store = Store::at(root);
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        assert!(store.component_path("auth").exists());
    }

    // ── remove_file ──────────────────────────────────────────────────────

    #[test]
    fn remove_file_deletes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let path = store.component_path("auth");
        store.write_atomic(&lock, &path, &comp).unwrap();
        assert!(path.exists());

        store.remove_file(&lock, &path).unwrap();
        assert!(!path.exists());
    }
}
