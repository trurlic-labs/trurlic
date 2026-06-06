//! `.trurl/` store — the persistence layer for Trurl's decision data.
//!
//! [`Store`] is the handle to a `.trurl/` directory. Read methods are
//! lock-free (writes use atomic renames). Write methods require a
//! [`StoreLock`] passed as a proof parameter.

pub mod schema;

mod state;
mod write;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

// Re-exports — public API surface of the store module.
pub use schema::{
    COMPONENTS_DIR, ComponentFile, DECISIONS_DIR, DecisionFile, FORMAT_VERSION, ProjectFile,
    STATE_DIR, STORE_DIR,
};
pub use state::{ProjectState, is_valid_kebab_case};

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
    pub(super) root: PathBuf,
}

impl Store {
    /// Walk up from `start` to find the nearest `.trurl/` directory.
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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn components_dir(&self) -> PathBuf {
        self.root.join(COMPONENTS_DIR)
    }

    pub(crate) fn decisions_dir(&self) -> PathBuf {
        self.root.join(DECISIONS_DIR)
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    pub fn component_path(&self, name: &str) -> PathBuf {
        self.components_dir().join(format!("{name}.toml"))
    }

    pub fn decision_path(&self, name: &str) -> PathBuf {
        self.decisions_dir().join(format!("{name}.toml"))
    }

    // ── Locking ──────────────────────────────────────────────────────────

    /// Acquire an exclusive advisory lock on `.trurl/`.
    ///
    /// Times out after 5 seconds. The lock is released when the returned
    /// [`StoreLock`] is dropped.
    pub fn lock(&self) -> Result<StoreLock> {
        use std::io::{Read, Seek, SeekFrom, Write};

        fs::create_dir_all(self.state_dir())?;

        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;

        let deadline = Instant::now() + LOCK_TIMEOUT;

        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = file.set_len(0);
                    let _ = file.seek(SeekFrom::Start(0));
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(StoreLock { _file: file });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let mut contents = String::new();
                        let _ = file.seek(SeekFrom::Start(0));
                        let _ = file.read_to_string(&mut contents);
                        let holder_pid = contents.trim().parse::<u32>().ok();

                        let detail = match holder_pid {
                            Some(pid) => format!("possibly held by PID {pid}"),
                            None => "another trurl process may be running".into(),
                        };
                        return Err(Error::LockTimeout {
                            timeout_secs: LOCK_TIMEOUT.as_secs(),
                            detail,
                        });
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    // ── Reading ──────────────────────────────────────────────────────────

    fn read_toml<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn read_project(&self) -> Result<ProjectFile> {
        self.read_toml(&self.root.join("project.toml"))
    }

    /// Read a component by name. Returns a clear error if it doesn't exist.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    pub fn list_components(&self) -> Result<Vec<String>> {
        state::list_toml_stems(&self.components_dir())
    }

    pub fn list_decisions(&self) -> Result<Vec<String>> {
        state::list_toml_stems(&self.decisions_dir())
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
    pub fn check_version(&self) -> Result<()> {
        let project = self.read_project()?;
        let stored = &project.trurl_version;
        if stored == FORMAT_VERSION {
            return Ok(());
        }
        match compare_versions(stored, FORMAT_VERSION) {
            Ordering::Greater => Err(Error::Validation(format!(
                ".trurl/ format version `{stored}` is newer than this CLI \
                 (expected `{FORMAT_VERSION}`). Please upgrade trurl."
            ))),
            _ => Err(Error::Validation(format!(
                ".trurl/ format version `{stored}` is older than this CLI \
                 (expected `{FORMAT_VERSION}`). A format migration may be needed."
            ))),
        }
    }
}

// ── StoreLock ────────────────────────────────────────────────────────────────

/// RAII exclusive lock on `.trurl/`.
///
/// Acquired via [`Store::lock`], released when dropped.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

// ── Version comparison ──────────────────────────────────────────────────────

fn compare_versions(a: &str, b: &str) -> Ordering {
    let parse = |v: &str| -> (u32, u32, u32) {
        let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        let patch = parts.next().unwrap_or(0);
        (major, minor, patch)
    };
    parse(a).cmp(&parse(b))
}

// ── Test helpers ────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::store::schema::{
        Component, ComponentFile, Decision, DecisionFile, Project, ProjectFile,
    };
    use chrono::{TimeZone, Utc};

    /// Create a minimal valid `.trurl/` directory and return a `Store` over it.
    pub fn setup_store(dir: &Path) -> Store {
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

    pub fn setup_store_with_version(dir: &Path, version: &str) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let project = ProjectFile {
            trurl_version: version.into(),
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

    pub fn sample_component(name: &str) -> ComponentFile {
        ComponentFile {
            component: Component {
                name: name.into(),
                description: format!("The {name} component"),
                connects_to: vec![],
            },
        }
    }

    pub fn sample_decision(name: &str, component: &str) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: format!("Choice for {name}"),
                reason: format!("Reason for {name}"),
                alternatives: vec![],
                created: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
                supersedes: None,
            },
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::cmp::Ordering;
    use tempfile::TempDir;

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
    }

    #[test]
    fn lock_writes_pid_to_lock_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let _lock = store.lock().unwrap();

        let content = fs::read_to_string(store.lock_path()).unwrap();
        let pid: u32 = content
            .trim()
            .parse()
            .expect("lock file should contain PID");
        assert_eq!(pid, std::process::id());
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
            other => panic!("expected Validation, got: {other}"),
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
    fn check_version_rejects_newer_format() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "99.0.0");

        let err = store.check_version().unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("99.0.0"));
                assert!(msg.contains("newer"), "should mention 'newer': {msg}");
                assert!(msg.contains("upgrade"), "should suggest upgrade: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn check_version_rejects_older_format() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.0.1");

        let err = store.check_version().unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("0.0.1"));
                assert!(msg.contains("older"), "should mention 'older': {msg}");
                assert!(msg.contains("migration"), "should mention migration: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── compare_versions ─────────────────────────────────────────────────

    #[test]
    fn compare_versions_equal() {
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
    }

    #[test]
    fn compare_versions_major_dominates() {
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.9.9", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn compare_versions_minor() {
        assert_eq!(compare_versions("0.2.0", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.0.9", "0.1.0"), Ordering::Less);
    }

    #[test]
    fn compare_versions_patch() {
        assert_eq!(compare_versions("0.1.1", "0.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.1.1"), Ordering::Less);
    }

    #[test]
    fn compare_versions_malformed_defaults_to_zero() {
        assert_eq!(compare_versions("abc", "0.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
    }
}
