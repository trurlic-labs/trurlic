//! Atomic write operations, batch commit, and crash recovery.
//!
//! All mutations flow through `.state/tmp/` then `rename(2)`. Batch writes
//! stage all temp files first, then rename in sequence.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::{Store, StoreLock};

// ── PendingWrite ─────────────────────────────────────────────────────────────

/// A file write staged for batch commit.
///
/// Created via [`Store::prepare_write`], executed via [`Store::commit_batch`].
pub struct PendingWrite {
    pub(super) target: PathBuf,
    pub(super) content: String,
}

// ── Store write methods ─────────────────────────────────────────────────────

impl Store {
    /// Write `value` to `target` atomically via `.state/tmp/`.
    ///
    /// Serializes to TOML, writes to a temp file, validates by deserializing
    /// back from disk, then renames to the final path. Caller **must** hold
    /// a [`StoreLock`].
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

        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(Error::Io(e));
        }

        // Validate written file by deserializing back — catches partial
        // writes, encoding corruption, and serialization round-trip issues.
        let readback = match fs::read_to_string(&tmp_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::Io(e));
            }
        };
        if let Err(e) = toml::from_str::<T>(&readback) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Validation(format!(
                "write verification failed: written file does not deserialize: {e}"
            )));
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        if let Err(e) = fs::rename(&tmp_path, target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        Ok(())
    }

    /// Serialize a value to TOML and verify the round-trip.
    ///
    /// Returns a [`PendingWrite`] for use with [`commit_batch`](Self::commit_batch).
    /// The content is deserialized back to `T` at this stage so that type-safe
    /// verification happens while the type is still known; `commit_batch`
    /// then verifies filesystem-level integrity via byte-compare.
    pub fn prepare_write<T: Serialize + DeserializeOwned>(
        &self,
        target: &Path,
        value: &T,
    ) -> Result<PendingWrite> {
        let content = toml::to_string_pretty(value)?;
        toml::from_str::<T>(&content).map_err(|e| {
            Error::Validation(format!("serialization round-trip verification failed: {e}"))
        })?;
        Ok(PendingWrite {
            target: target.to_path_buf(),
            content,
        })
    }

    /// Execute a batch of writes and removes as a two-phase commit.
    ///
    /// Phase 1: write all content to `.state/tmp/`.
    /// Phase 2: verify each temp file (byte-compare; type-safe check was in `prepare_write`).
    /// Phase 3: rename all temp files to final paths (each atomic on POSIX).
    /// Phase 4: remove old files.
    ///
    /// Caller **must** hold a [`StoreLock`].
    pub fn commit_batch(
        &self,
        _lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: Vec<PathBuf>,
    ) -> Result<()> {
        if writes.is_empty() && removes.is_empty() {
            return Ok(());
        }

        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        // Phase 1: Write all to tmp
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(writes.len());

        for (i, write) in writes.iter().enumerate() {
            let filename = write
                .target
                .file_name()
                .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
            let tmp_name = format!("{i}_{}", filename.to_string_lossy());
            let tmp_path = tmp_dir.join(tmp_name);

            if let Err(e) = fs::write(&tmp_path, &write.content) {
                cleanup_tmp_files(&staged);
                return Err(Error::Io(e));
            }
            staged.push((tmp_path, write.target.clone()));
        }

        // Phase 2: Verify write integrity — type-safe deserialization already
        // happened in prepare_write; this byte-compare catches filesystem-level
        // corruption (partial writes, bitflips) on the validated content.
        for (i, (tmp_path, _)) in staged.iter().enumerate() {
            let readback = match fs::read_to_string(tmp_path) {
                Ok(s) => s,
                Err(e) => {
                    cleanup_tmp_files(&staged);
                    return Err(Error::Io(e));
                }
            };
            if readback != writes[i].content {
                cleanup_tmp_files(&staged);
                return Err(Error::Validation(
                    "batch write verification failed: content mismatch".into(),
                ));
            }
        }

        // Ensure parent directories exist before renaming
        for (_, target) in &staged {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
        }

        // Phase 3: Rename all to final paths
        for (i, (tmp_path, target)) in staged.iter().enumerate() {
            if let Err(e) = fs::rename(tmp_path, target) {
                for (remaining, _) in staged.iter().skip(i + 1) {
                    let _ = fs::remove_file(remaining);
                }
                return Err(Error::Io(e));
            }
        }

        // Phase 4: Remove old files
        for path in &removes {
            fs::remove_file(path)?;
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
    /// that reads `.trurl/`.
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
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        }
        Ok(count)
    }
}

/// Best-effort cleanup of staged temp files on batch failure.
fn cleanup_tmp_files(staged: &[(PathBuf, PathBuf)]) {
    for (tmp_path, _) in staged {
        let _ = fs::remove_file(tmp_path);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::testing::*;
    use tempfile::TempDir;

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

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after atomic write");
        }
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(crate::store::STORE_DIR);
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        let store = Store::at(root);
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        assert!(store.component_path("auth").exists());
    }

    // ── commit_batch ─────────────────────────────────────────────────────

    #[test]
    fn commit_batch_writes_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp1 = sample_component("auth");
        let comp2 = sample_component("database");

        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp1)
                .unwrap(),
            store
                .prepare_write(&store.component_path("database"), &comp2)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, vec![]).unwrap();

        let read1 = store.read_component("auth").unwrap();
        assert_eq!(read1, comp1);
        let read2 = store.read_component("database").unwrap();
        assert_eq!(read2, comp2);
    }

    #[test]
    fn commit_batch_writes_and_removes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let old = sample_component("old-name");
        store
            .write_atomic(&lock, &store.component_path("old-name"), &old)
            .unwrap();

        let new = sample_component("new-name");
        let writes = vec![
            store
                .prepare_write(&store.component_path("new-name"), &new)
                .unwrap(),
        ];
        let removes = vec![store.component_path("old-name")];

        store.commit_batch(&lock, writes, removes).unwrap();

        assert!(store.component_path("new-name").exists());
        assert!(!store.component_path("old-name").exists());
    }

    #[test]
    fn commit_batch_leaves_no_tmp_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, vec![]).unwrap();

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after batch commit");
        }
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
