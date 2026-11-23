//! Cross-process advisory lock on `.trurlic/`.
//!
//! Every graph write runs under the exclusive lock on `.state/lock`, and
//! [`StoreLock`] is the proof a write method takes. The holder writes its PID
//! into the lock file so a timed-out waiter can name it. Every read loads
//! under the shared lock, which keeps writers out without excluding other
//! readers.
//!
//! Locks are std's `File::try_lock` and `try_lock_shared`, polled until
//! [`LOCK_TIMEOUT`]: the blocking calls have no timeout, so a hung holder
//! would hang every waiter. std maps each platform's contention error
//! (`EWOULDBLOCK`, `ERROR_LOCK_VIOLATION`) to [`TryLockError::WouldBlock`].

use std::fs::{self, File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::{Error, Result};

use super::Store;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Proof that this process holds the store's exclusive file lock: every
/// function that changes a file under `.trurlic/` takes `&StoreLock`, and a
/// [`Writer`](super::Writer) owns one. Dropping it releases the lock.
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

/// Proof that this process holds the store's shared file lock: no commit
/// lands while it lives. Dropping it releases the lock.
#[must_use = "dropping the lock immediately releases it"]
pub(super) struct SharedLock {
    _file: File,
}

impl Store {
    /// Acquire the exclusive lock, waiting up to 5 seconds for another
    /// holder to release it.
    pub(crate) fn lock(&self) -> Result<StoreLock> {
        let mut file = self.open_lock_file()?;
        poll(&mut file, &self.lock_path(), File::try_lock)?;
        Ok(StoreLock::claim(file))
    }

    /// Acquire the shared lock, waiting up to 5 seconds for a writer.
    pub(super) fn lock_shared(&self) -> Result<SharedLock> {
        let mut file = self.open_lock_file()?;
        poll(&mut file, &self.lock_path(), File::try_lock_shared)?;
        Ok(SharedLock { _file: file })
    }

    /// Read+write rather than append: Windows refuses to lock a handle
    /// opened for append only.
    fn open_lock_file(&self) -> Result<File> {
        let state_dir = self.state_dir();
        fs::create_dir_all(&state_dir).map_err(Error::io(&state_dir))?;
        let path = self.lock_path();
        File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(Error::io(&path))
    }
}

/// Try the file lock; on contention retry after [`LOCK_POLL_INTERVAL`],
/// until [`LOCK_TIMEOUT`].
fn poll(
    file: &mut File,
    path: &Path,
    try_lock: fn(&File) -> std::result::Result<(), TryLockError>,
) -> Result<()> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match try_lock(file) {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Error::LockTimeout {
                    timeout_secs: LOCK_TIMEOUT.as_secs(),
                    detail: holder_detail(file),
                });
            }
            Err(TryLockError::Error(source)) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
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
    fn shared_locks_coexist_and_exclude_a_writer() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let first = store.lock_shared().unwrap();
        let second = store.lock_shared().unwrap();

        let (acquired_tx, acquired_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| acquired_tx.send(store.lock()).unwrap());

            let still_waiting = acquired_rx.recv_timeout(LOCK_POLL_INTERVAL * 6);
            assert!(
                matches!(still_waiting, Err(mpsc::RecvTimeoutError::Timeout)),
                "writer acquired under a shared lock: {still_waiting:?}"
            );

            drop(first);
            drop(second);
            let acquired = acquired_rx.recv_timeout(LOCK_TIMEOUT).unwrap();
            assert!(acquired.is_ok(), "{acquired:?}");
        });
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
