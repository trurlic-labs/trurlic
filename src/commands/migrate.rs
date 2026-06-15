use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use crate::store::Store;
use crate::store::schema::{
    COMPONENTS_DIR, ComponentFile, DECISIONS_DIR, DecisionFile, FORMAT_VERSION, GRAPH_FILE,
    GraphIndex, PATTERNS_DIR, PatternFile, ProjectFile,
};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

pub fn migrate(cwd: &Path, dry_run: DryRun) -> Result<()> {
    let store = Store::discover(cwd)?;
    store.clean_stale_tmp()?;

    let project = store.read_project()?;
    let old_version = &project.trurlic_version;

    if old_version == FORMAT_VERSION {
        println!("Already up to date (format version {FORMAT_VERSION}).");
        return Ok(());
    }

    match crate::store::compare_versions(old_version, FORMAT_VERSION) {
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
        print_dry_run(root, old_version)?;
        return Ok(());
    }

    let backup_name = format!(
        ".trurlic-backup-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S")
    );
    let project_root = root
        .parent()
        .ok_or_else(|| Error::Validation("store root has no parent directory".into()))?;
    let backup_path = project_root.join(&backup_name);
    copy_dir_recursive(root, &backup_path)?;

    let lock = store.lock()?;

    round_trip_project(root, &lock, &store)?;
    round_trip_dir::<ComponentFile>(root, COMPONENTS_DIR, &lock, &store)?;
    round_trip_dir::<DecisionFile>(root, DECISIONS_DIR, &lock, &store)?;
    round_trip_dir::<PatternFile>(root, PATTERNS_DIR, &lock, &store)?;

    // Rebuild graph.toml with fresh BLAKE3 hashes from the updated node
    // files. A plain round-trip of the old graph.toml would leave stale
    // hashes for any file whose content changed (e.g. new default fields).
    let state = store.load_state()?;
    store.write_atomic(&lock, &store.graph_path(), &state.graph_index)?;

    drop(lock);

    println!("Migrated from {old_version} \u{2192} {FORMAT_VERSION}, backup at {backup_name}/");
    Ok(())
}

fn round_trip_project(root: &Path, lock: &crate::store::StoreLock, store: &Store) -> Result<()> {
    let path = root.join("project.toml");
    let content = fs::read_to_string(&path)?;
    let mut project: ProjectFile = toml::from_str(&content)?;
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
        let index: GraphIndex = toml::from_str(&content)?;
        let new_content = toml::to_string_pretty(&index)?;
        // Node file content changes cause hash updates in graph.toml,
        // even when the GraphIndex structure itself is unchanged.
        if content != new_content || node_changes > 0 {
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
            None,
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
            None,
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
    fn migrate_with_patterns() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", Some("Auth module")).unwrap();
        add_component(tmp.path(), "api", Some("API module")).unwrap();
        decide(tmp.path(), "auth", "JWT tokens", "Stateless", None, &[]).unwrap();
        decide(tmp.path(), "api", "REST API", "Standard", None, &[]).unwrap();

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
