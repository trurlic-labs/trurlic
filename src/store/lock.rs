//! Cross-process advisory lock on `.trurlic/`.
//!
//! Every graph write runs under the exclusive lock on `.state/lock`, and
//! [`StoreLock`] is the proof a write method takes. The holder writes its PID
//! into the lock file so a timed-out waiter can name it.
//!
//! The lock is std's `File::try_lock`, polled until [`LOCK_TIMEOUT`]: the
//! blocking `File::lock` has no timeout, so a hung holder would hang every
//! writer. std maps each platform's contention error (`EWOULDBLOCK`,
//! `ERROR_LOCK_VIOLATION`) to [`TryLockError::WouldBlock`].

use std::fs::{self, File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use crate::{Error, Result};

use super::Store;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Proof that this process holds the store's exclusive file lock. Write
/// methods take `&StoreLock`; dropping it releases the lock.
#[derive(Debug)]
#[must_use = "dropping the lock immediately releases it"]
pub struct StoreLock {
    _file: File,
}

impl StoreLock {
    /// Wrap the file whose lock this process now holds, and record its PID.
    /// The PID is a diagnostic only, so a failed write still yields the lock.
    fn claim(mut file: File) -> Self {
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = write!(file, "{}", std::process::id());
        Self { _file: file }
    }
}

impl Store {
    /// Acquire an exclusive advisory lock on `.trurlic/`, waiting up to
    /// 5 seconds for another holder to release it. The lock is released
    /// when the returned [`StoreLock`] is dropped.
    pub fn lock(&self) -> Result<StoreLock> {
        let mut file = self.open_lock_file()?;
        let deadline = Instant::now() + LOCK_TIMEOUT;

        loop {
            match file.try_lock() {
                Ok(()) => return Ok(StoreLock::claim(file)),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::LockTimeout {
                        timeout_secs: LOCK_TIMEOUT.as_secs(),
                        detail: holder_detail(&mut file),
                    });
                }
                Err(TryLockError::Error(e)) => return Err(Error::Io(e)),
            }
        }
    }

    /// Non-blocking lock attempt. Returns immediately with an error if
    /// the lock is held by another process. Used by the map API to avoid
    /// stalling the tokio runtime (and all WebSocket/HTTP reads) while
    /// waiting for a long-running CLI operation to release the lock.
    pub fn try_lock(&self) -> Result<StoreLock> {
        let file = self.open_lock_file()?;
        match file.try_lock() {
            Ok(()) => Ok(StoreLock::claim(file)),
            Err(TryLockError::WouldBlock) => Err(Error::LockTimeout {
                timeout_secs: 0,
                detail: "store is locked by another process, try again shortly".into(),
            }),
            Err(TryLockError::Error(e)) => Err(Error::Io(e)),
        }
    }

    /// Read+write rather than append: Windows refuses to lock a handle
    /// opened for append only.
    fn open_lock_file(&self) -> Result<File> {
        fs::create_dir_all(self.state_dir())?;
        Ok(File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?)
    }
}

/// Name the holder from the PID it wrote. On Windows the holder's lock also
/// blocks this read, so the detail falls back to the generic message.
fn holder_detail(file: &mut File) -> String {
    let mut contents = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut contents);
    match contents.trim().parse::<u32>() {
        Ok(pid) => format!("possibly held by PID {pid}"),
        Err(_) => "another trurlic process may be running".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use std::sync::mpsc;
    use tempfile::TempDir;

    // Two handles on one lock file contend like two processes: flock and
    // LockFileEx locks belong to the open file, not to the process.
    #[test]
    fn second_lock_waits_for_the_holder_then_acquires() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let held = store.lock().unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                started_tx.send(()).unwrap();
                acquired_tx.send(store.lock()).unwrap();
            });
            started_rx.recv().unwrap();

            // Several poll intervals: a waiter that failed fast instead of
            // polling would have reported by now.
            let still_waiting = acquired_rx.recv_timeout(LOCK_POLL_INTERVAL * 6);
            assert!(
                matches!(still_waiting, Err(mpsc::RecvTimeoutError::Timeout)),
                "second handle returned while the lock was held: {still_waiting:?}"
            );

            drop(held);
            let acquired = acquired_rx.recv_timeout(LOCK_TIMEOUT).unwrap();
            assert!(acquired.is_ok(), "{acquired:?}");
        });
    }

    #[test]
    fn try_lock_refuses_while_held() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let _held = store.lock().unwrap();

        let err = store.try_lock().unwrap_err();
        assert!(matches!(
            err,
            Error::LockTimeout {
                timeout_secs: 0,
                ..
            }
        ));
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
