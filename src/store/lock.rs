//! Cross-process advisory lock on `.trurlic/`.
//!
//! Every graph write runs under the exclusive lock on `.state/lock`, and
//! [`StoreLock`] is the proof a write method takes. The holder writes its PID
//! into the lock file so a timed-out waiter can name it.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::{Error, Result};

use super::Store;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
#[must_use = "dropping the lock immediately releases it"]
pub struct StoreLock {
    _file: File,
}

impl Store {
    /// Acquire an exclusive advisory lock on `.trurlic/`.
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
                            None => "another trurlic process may be running".into(),
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

    /// Non-blocking lock attempt. Returns immediately with an error if
    /// the lock is held by another process. Used by the map API to avoid
    /// stalling the tokio runtime (and all WebSocket/HTTP reads) while
    /// waiting for a long-running CLI operation to release the lock.
    pub fn try_lock(&self) -> Result<StoreLock> {
        use std::io::{Seek, SeekFrom, Write};

        fs::create_dir_all(self.state_dir())?;

        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;

        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = write!(file, "{}", std::process::id());
                Ok(StoreLock { _file: file })
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => Err(Error::LockTimeout {
                timeout_secs: 0,
                detail: "store is locked by another process — try again shortly".into(),
            }),
            Err(e) => Err(Error::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use tempfile::TempDir;

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
}
