use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use crate::store::schema::{
    COMPONENTS_DIR, ComponentFile, DECISIONS_DIR, DecisionFile, EdgeEntry, FORMAT_VERSION,
    GRAPH_FILE, PATTERNS_DIR, PatternFile, ProjectFile,
};
use crate::store::{Store, StoreLock};
use crate::{Error, Result};

use super::DryRun;

/// `trurlic migrate` — upgrade an on-disk store to the current `FORMAT_VERSION`:
/// re-serialize every node to the latest schema and drop edges whose kind the
/// schema retired. `DryRun::Yes` reports the plan without writing.
pub fn migrate(cwd: &Path, dry_run: DryRun) -> Result<()> {
    let store = Store::discover(cwd)?;
    store.clean_stale_tmp()?;

    let old_version = store.read_project()?.trurlic_version;

    match crate::store::compare_versions(&old_version, FORMAT_VERSION) {
        Ordering::Greater => {
            return Err(Error::Validation(format!(
                ".trurlic/ format version `{old_version}` is newer than this CLI \
                 (expected `{FORMAT_VERSION}`). Please upgrade trurlic."
            )));
        }
        Ordering::Equal => {
            println!("Already up to date (format version {FORMAT_VERSION}).");
            return Ok(());
        }
        Ordering::Less => {}
    }

    let root = store.root();

    if dry_run == DryRun::Yes {
        return print_dry_run(root, &old_version);
    }

    let lock = store.lock()?;

    // Re-check version under lock to close the TOCTOU window: another process
    // could have migrated between the initial unlocked read and lock
    // acquisition. Doing it before the backup avoids a wasted copy on that race.
    if crate::store::compare_versions(&store.read_project()?.trurlic_version, FORMAT_VERSION)
        != Ordering::Less
    {
        drop(lock);
        println!("Already up to date (format version {FORMAT_VERSION}).");
        return Ok(());
    }

    // Snapshot under the lock so no concurrent CLI/MCP/map write can tear the
    // backup — a copy taken without the lock could pair a freshly written node
    // file with a pre-write graph.toml, defeating its purpose as a recovery point.
    let backup_name = format!(
        ".trurlic-backup-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    );
    let backup_path = root
        .parent()
        .ok_or_else(|| Error::Validation("store root has no parent directory".into()))?
        .join(&backup_name);
    if let Err(e) = copy_dir_recursive(root, &backup_path) {
        let _ = fs::remove_dir_all(&backup_path);
        return Err(e);
    }

    let stripped_edges = apply_migration(root, &lock, &store)?;
    drop(lock);

    if stripped_edges > 0 {
        println!("Removed {stripped_edges} graph edge(s) not recognized by the current schema.");
    }
    println!("Migrated from {old_version} \u{2192} {FORMAT_VERSION}, backup at {backup_name}/");
    Ok(())
}

/// Rewrite every node file into the current schema, strip graph edges the
/// current schema can no longer parse, stamp the new format version into
/// `project.toml`, and rebuild `graph.toml` last as the commit point. Returns
/// the number of retired edges removed.
///
/// Ordering is load-bearing:
/// 1. Node files (and `project.toml`) are rewritten into the current format
///    *before* graph.toml is rebuilt, because graph.toml stores a content hash
///    per node — including `project`. Bumping the version after the rebuild
///    would leave that hash stale.
/// 2. Retired edges are stripped *before* the version bump, so even if the run
///    is interrupted right after the bump the graph still parses with a typed
///    read: the store stays loadable (at worst with stale hashes a later write
///    refreshes) rather than bricked on an unknown edge kind.
/// 3. graph.toml is written last, per the storage spec's commit-point rule.
fn apply_migration(root: &Path, lock: &StoreLock, store: &Store) -> Result<usize> {
    round_trip_dir::<ComponentFile>(root, COMPONENTS_DIR, lock, store)?;
    round_trip_dir::<DecisionFile>(root, DECISIONS_DIR, lock, store)?;
    round_trip_dir::<PatternFile>(root, PATTERNS_DIR, lock, store)?;

    // Drop edges whose kind no longer exists in the schema (e.g. `supersedes`,
    // retired in 0.4.0). These live only in the compiled index, never in node
    // files, and a typed read rejects the whole graph on one unknown variant —
    // so this loose pre-pass must run before the typed rebuild below.
    let stripped = sanitize_graph_edges(root, lock, store)?;

    write_migrated_version(root, lock, store)?;

    // Rebuild graph.toml with fresh BLAKE3 hashes from the finalized node files
    // (project.toml included). A plain round-trip of the old graph.toml would
    // leave stale hashes for any file whose content changed (new default fields,
    // the version bump).
    let state = store.load_state()?;
    store.write_atomic(lock, &store.graph_path(), &state.graph_index)?;

    Ok(stripped)
}

/// Remove edges from graph.toml whose `kind` no longer deserializes into a
/// current [`EdgeEntry`], returning how many were dropped.
///
/// A typed `GraphIndex` read rejects the entire file on a single unknown edge
/// variant, so this parses loosely as a `toml::Table`, keeps only the edges the
/// current schema understands, and rewrites the file when any were dropped.
/// Returns `0` when graph.toml is absent or already clean.
fn sanitize_graph_edges(root: &Path, lock: &StoreLock, store: &Store) -> Result<usize> {
    let path = root.join(GRAPH_FILE);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut index: toml::Table = toml::from_str(&content)?;
    let Some(toml::Value::Array(edges)) = index.get_mut("edges") else {
        return Ok(0);
    };

    let before = edges.len();
    edges.retain(|edge| edge.clone().try_into::<EdgeEntry>().is_ok());
    let stripped = before - edges.len();
    if stripped == 0 {
        return Ok(0);
    }

    store.write_atomic(lock, &path, &index)?;
    Ok(stripped)
}

/// Count graph edges the current schema can no longer parse, without a typed
/// read that a single retired edge would reject outright. Used by the dry-run
/// preview, which must not mutate the store.
fn count_retired_edges(graph_content: &str) -> Result<usize> {
    let index: toml::Table = toml::from_str(graph_content)?;
    let Some(toml::Value::Array(edges)) = index.get("edges") else {
        return Ok(0);
    };
    Ok(edges
        .iter()
        .filter(|edge| (*edge).clone().try_into::<EdgeEntry>().is_err())
        .count())
}

/// Stamp the current [`FORMAT_VERSION`] into `project.toml`. This is the
/// migration commit point — see [`apply_migration`] for why it runs last.
fn write_migrated_version(root: &Path, lock: &StoreLock, store: &Store) -> Result<()> {
    let path = root.join("project.toml");
    let mut project: ProjectFile = toml::from_str(&fs::read_to_string(&path)?)?;
    project.trurlic_version = FORMAT_VERSION.into();
    store.write_atomic(lock, &path, &project)?;
    Ok(())
}

fn round_trip_dir<T>(
    root: &Path,
    subdir: &str,
    lock: &crate::store::StoreLock,
    store: &Store,
) -> Result<()>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let dir = root.join(subdir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::Io(e)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let value: T = toml::from_str(&content)?;
        store.write_atomic(lock, &path, &value)?;
    }
    Ok(())
}

fn print_dry_run(root: &Path, old_version: &str) -> Result<()> {
    println!("Dry run: would migrate from {old_version} \u{2192} {FORMAT_VERSION}");
    println!();

    let mut count = 0;

    let path = root.join("project.toml");
    let content = fs::read_to_string(&path)?;
    let mut project: ProjectFile = toml::from_str(&content)?;
    project.trurlic_version = FORMAT_VERSION.into();
    let new_content = toml::to_string_pretty(&project)?;
    if content != new_content {
        println!("  would update: project.toml");
        count += 1;
    }

    let node_changes = count_changed_files::<ComponentFile>(root, COMPONENTS_DIR)?
        + count_changed_files::<DecisionFile>(root, DECISIONS_DIR)?
        + count_changed_files::<PatternFile>(root, PATTERNS_DIR)?;
    count += node_changes;

    let graph_path = root.join(GRAPH_FILE);
    if graph_path.exists() {
        let content = fs::read_to_string(&graph_path)?;
        // Parse loosely: a retired edge (e.g. `supersedes`) makes a typed read
        // fail on the very store this command exists to repair, so the dry-run
        // must not attempt one.
        let retired = count_retired_edges(&content)?;
        // graph.toml is rewritten whenever a node file changed (hash refresh) or
        // a retired edge is stripped.
        if retired > 0 {
            println!("  would strip {retired} retired graph edge(s) and rewrite: {GRAPH_FILE}");
            count += 1;
        } else if node_changes > 0 {
            println!("  would update: {GRAPH_FILE}");
            count += 1;
        }
    }

    if count == 0 {
        println!("  no files would change (aside from version bump)");
    } else {
        println!();
        println!("{count} file(s) would be updated.");
    }

    Ok(())
}

fn count_changed_files<T>(root: &Path, subdir: &str) -> Result<usize>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let dir = root.join(subdir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut count = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let value: T = toml::from_str(&content)?;
        let new_content = toml::to_string_pretty(&value)?;
        if content != new_content {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            println!("  would update: {subdir}/{name}");
            count += 1;
        }
    }
    Ok(count)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Skip symlinks rather than follow them out of the store tree; warn
            // so an incomplete backup is never silent.
            eprintln!(
                "warning: backup skipped symlink {} — copy its target manually if needed",
                src_path.display()
            );
            continue;
        }
        if ft.is_dir() {
            // Skip .state/ — it contains transient data (tmp files, locks, sessions).
            if entry.file_name() == ".state" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, decide, init};
    use crate::store::schema::FORMAT_VERSION;
    use crate::store::testing::setup_store_with_version;
    use tempfile::TempDir;

    #[test]
    fn migrate_already_current_is_noop() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        migrate(tmp.path(), DryRun::No).unwrap();
    }

    #[test]
    fn migrate_rejects_newer_version() {
        let tmp = TempDir::new().unwrap();
        setup_store_with_version(tmp.path(), "99.0.0");

        let err = migrate(tmp.path(), DryRun::No).unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("newer"), "should mention 'newer': {msg}");
                assert!(msg.contains("upgrade"), "should suggest upgrade: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn migrate_old_version_updates_to_current() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Use JWT tokens",
            "Stateless auth",
            &[],
            &[],
        )
        .unwrap();

        // Downgrade the version to simulate an old store.
        let store = Store::discover(tmp.path()).unwrap();
        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::No).unwrap();

        // Verify version is updated.
        let project = store.read_project().unwrap();
        assert_eq!(project.trurlic_version, FORMAT_VERSION);

        // Verify the store loads successfully with current code.
        store.check_version().unwrap();
        let state = store.load_state().unwrap();
        assert!(state.components.contains_key("auth"));
        assert!(state.decisions.contains_key("use-jwt-tokens"));
    }

    #[test]
    fn migrate_creates_backup() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();

        // Downgrade.
        let store = Store::discover(tmp.path()).unwrap();
        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::No).unwrap();

        // Find the backup directory.
        let backups: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(".trurlic-backup-"))
            })
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup directory expected");

        // Backup should contain project.toml with the OLD version.
        let backup_project_path = backups[0].path().join("project.toml");
        let backup_content = fs::read_to_string(backup_project_path).unwrap();
        let backup_project: ProjectFile = toml::from_str(&backup_content).unwrap();
        assert_eq!(backup_project.trurlic_version, "0.1.0");

        // Backup should contain the component file.
        assert!(
            backups[0]
                .path()
                .join("components")
                .join("auth.toml")
                .exists()
        );

        // Backup should NOT contain .state/ (transient data).
        assert!(!backups[0].path().join(".state").exists());
    }

    #[test]
    fn migrate_dry_run_does_not_modify_files() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();

        // Downgrade.
        let store = Store::discover(tmp.path()).unwrap();
        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::Yes).unwrap();

        // Version should still be old.
        let project = store.read_project().unwrap();
        assert_eq!(project.trurlic_version, "0.1.0");

        // No backup directory should exist.
        let backups: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(".trurlic-backup-"))
            })
            .collect();
        assert!(backups.is_empty(), "dry run must not create backup");
    }

    #[test]
    fn migrate_fills_in_defaults_for_missing_fields() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Use JWT tokens",
            "Stateless auth",
            &[],
            &[],
        )
        .unwrap();

        // Downgrade and strip the attribution field from the decision file
        // to simulate an older format that didn't have it.
        let store = Store::discover(tmp.path()).unwrap();
        let dec_path = store.decision_path("use-jwt-tokens");
        let content = fs::read_to_string(&dec_path).unwrap();
        let stripped = content
            .lines()
            .filter(|line| !line.starts_with("attribution"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&dec_path, &stripped).unwrap();

        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::No).unwrap();

        // The decision file should now have the attribution field with default.
        let dec_content = fs::read_to_string(&dec_path).unwrap();
        assert!(
            dec_content.contains("attribution"),
            "migration should fill in default attribution field"
        );

        // Should load cleanly with correct hashes.
        store.check_version().unwrap();
        let hash_issues = store.verify_hashes().unwrap();
        assert!(
            hash_issues.is_empty(),
            "graph.toml hashes must match updated files: {hash_issues:?}"
        );
        let state = store.load_state().unwrap();
        assert!(state.decisions.contains_key("use-jwt-tokens"));
    }

    #[test]
    fn migrate_handles_semantically_equal_version() {
        // VERSION "0.3" is textually different from "0.3.0" but semantically
        // equivalent via compare_versions. This must not panic.
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let mut project = store.read_project().unwrap();
        // Truncate to just major.minor — semantically equal to FORMAT_VERSION.
        let parts: Vec<&str> = FORMAT_VERSION.split('.').collect();
        if parts.len() >= 2 && parts.get(2) == Some(&"0") {
            project.trurlic_version = format!("{}.{}", parts[0], parts[1]);
        } else {
            project.trurlic_version = FORMAT_VERSION.to_string();
        }
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        // Must not panic — should treat as "already up to date".
        let result = migrate(tmp.path(), DryRun::No);
        assert!(result.is_ok());
    }

    #[test]
    fn migrate_empty_store() {
        // A store with zero components/decisions/patterns should migrate cleanly.
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::No).unwrap();

        store.check_version().unwrap();
        let hash_issues = store.verify_hashes().unwrap();
        assert!(
            hash_issues.is_empty(),
            "graph.toml hashes must match: {hash_issues:?}"
        );
    }

    #[test]
    fn migrate_strips_retired_supersedes_edge() {
        // A store written by 0.2.0 (format 0.3.0) could carry a `supersedes`
        // edge in graph.toml — an edge kind removed in 0.4.0. A typed load
        // rejects the whole file on it, so migrate must strip it and the store
        // must open afterward.
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Old choice",
            "Superseded later",
            &[],
            &[],
        )
        .unwrap();
        decide(
            tmp.path(),
            "auth",
            "New choice",
            "Replaces the old one",
            &[],
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();

        // Inject a legacy supersedes edge and downgrade the version, mimicking a
        // graph.toml this CLI can no longer parse with a typed read.
        let graph_path = store.graph_path();
        let mut graph = fs::read_to_string(&graph_path).unwrap();
        graph.push_str(
            "\n[[edges]]\nfrom = \"new-choice\"\nto = \"old-choice\"\nkind = \"supersedes\"\n",
        );
        fs::write(&graph_path, &graph).unwrap();

        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.3.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        // A typed load must fail before migration (proves the fixture is real).
        assert!(
            store.load_state().is_err(),
            "supersedes edge should break a typed load pre-migration"
        );

        migrate(tmp.path(), DryRun::No).unwrap();

        // After migration the store opens and the retired edge is gone.
        store.check_version().unwrap();
        let state = store.load_state().unwrap();
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "new-choice" && e.to == "old-choice"),
            "supersedes edge must be stripped"
        );
        assert!(state.decisions.contains_key("old-choice"));
        assert!(state.decisions.contains_key("new-choice"));
        let hash_issues = store.verify_hashes().unwrap();
        assert!(hash_issues.is_empty(), "hashes must match: {hash_issues:?}");
    }

    #[test]
    fn migrate_dry_run_previews_retired_edge_without_crashing_or_mutating() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Authentication")).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Old choice",
            "Superseded later",
            &[],
            &[],
        )
        .unwrap();
        decide(
            tmp.path(),
            "auth",
            "New choice",
            "Replaces the old one",
            &[],
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();
        let graph_path = store.graph_path();
        let mut graph = fs::read_to_string(&graph_path).unwrap();
        graph.push_str(
            "\n[[edges]]\nfrom = \"new-choice\"\nto = \"old-choice\"\nkind = \"supersedes\"\n",
        );
        fs::write(&graph_path, &graph).unwrap();

        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.3.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        // A typed dry-run parse would choke on the retired edge — the very store
        // migrate exists to repair. The loose preview must succeed instead.
        migrate(tmp.path(), DryRun::Yes).unwrap();

        // A dry run writes nothing: the version and the retired edge both remain.
        assert_eq!(store.read_project().unwrap().trurlic_version, "0.3.0");
        assert!(
            fs::read_to_string(&graph_path)
                .unwrap()
                .contains("supersedes"),
            "dry run must not mutate the store"
        );
    }

    #[test]
    fn migrate_with_patterns() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Auth module")).unwrap();
        add_component(tmp.path(), "api", Some("API module")).unwrap();
        decide(tmp.path(), "auth", "JWT tokens", "Stateless", &[], &[]).unwrap();
        decide(tmp.path(), "api", "REST API", "Standard", &[], &[]).unwrap();

        // Create a pattern file directly (store::record_pattern is pub(crate)
        // in the private `write` module, so we write TOML by hand).
        let store = Store::discover(tmp.path()).unwrap();
        let pat_dir = store.root().join(PATTERNS_DIR);
        fs::create_dir_all(&pat_dir).unwrap();
        let pat_content = r#"[pattern]
name = "auth-pattern"
description = "Authentication pattern"
"#;
        fs::write(pat_dir.join("auth-pattern.toml"), pat_content).unwrap();

        // Downgrade version to force migration.
        let mut project = store.read_project().unwrap();
        project.trurlic_version = "0.1.0".into();
        let lock = store.lock().unwrap();
        store
            .write_atomic(&lock, &store.root().join("project.toml"), &project)
            .unwrap();
        drop(lock);

        migrate(tmp.path(), DryRun::No).unwrap();

        store.check_version().unwrap();
        let state = store.load_state().unwrap();
        assert!(state.patterns.contains_key("auth-pattern"));
    }
}
