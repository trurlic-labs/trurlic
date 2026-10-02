//! Files that survive a crash: staged bytes are flushed before a rename
//! puts them in place, and a directory is flushed after its entries change.
//!
//! A rename is atomic but not durable. Without the flushes, a power loss
//! can keep the rename and lose the data it pointed at, which leaves an
//! empty node file behind.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;

use super::Store;

impl Store {
    /// A fresh path in `.state/tmp/`, named after this process and a
    /// counter. A name is never used twice, so no write stages onto a file
    /// that a crashed write of another process left behind.
    pub(super) fn temp_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.tmp_dir().join(format!("{}-{n}", std::process::id()))
    }

    /// Write `bytes` to a fresh temp file and flush them to disk. The
    /// caller creates `.state/tmp/` and renames or removes the file.
    pub(super) fn stage(&self, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.temp_path();
        let written = File::create_new(&path).and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_data()
        });
        match written {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = fs::remove_file(&path);
                Err(e.into())
            }
        }
    }
}

/// Flush `dir`'s entries, so the renames and removals in it survive a
/// power loss.
#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// std opens no directory handle on Windows, so there is nothing to flush;
/// renames there are as durable as NTFS makes them.
#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use tempfile::TempDir;

    #[test]
    fn temp_paths_never_repeat_and_name_the_process() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let first = store.temp_path();
        let second = store.temp_path();

        assert_ne!(first, second);
        let pid = format!("{}-", std::process::id());
        assert!(
            first
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&pid)
        );
    }

    #[test]
    fn stage_writes_the_bytes_to_a_new_temp_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        fs::create_dir_all(store.tmp_dir()).unwrap();

        let staged = store.stage(b"generation = 1\n").unwrap();

        assert_eq!(staged.parent(), Some(store.tmp_dir().as_path()));
        assert_eq!(fs::read(&staged).unwrap(), b"generation = 1\n");
    }
}
