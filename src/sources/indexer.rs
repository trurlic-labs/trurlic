//! The threads the shell indexes files on. Each [`Indexer::map`] starts a
//! pool of workers and ends it as it returns, and [`Worker::index`] is the
//! shell's one call of [`code::index`].
//!
//! A parse takes a fixed call stack and allocates in proportion to its
//! file, a little over 2 KiB a byte in the costliest shapes measured,
//! since a refused parse runs on to the runtime's next check (`code::work`);
//! and a thread keeps the memory it freed until it ends. So a worker
//! indexes a file up to [`MAX_BYTES_INDEXED_BY_A_WORKER`] itself and starts
//! a thread for a larger one, whose memory goes back when that thread
//! ends, and the files indexed at once hold at most
//! [`MAX_BYTES_INDEXED_AT_ONCE`] bytes between them. Every thread has a
//! stack of [`WORKER_STACK_BYTES`].
//!
//! A C or C++ file is indexed against the part of its tree's blank sets
//! it writes, the sets a fact of the tree's headers: the indexer holds the
//! calling tree's, taken once a call before any file is indexed, and every
//! file the call indexes, in that tree, another worktree or git's history,
//! is read against them.

use std::error::Error;
use std::io::{self, ErrorKind};
use std::panic;
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, Scope, ScopedJoinHandle};

use rayon::ThreadPool;
use rayon::prelude::*;

use crate::code::{self, BlankMacros, Defines, FileKind, FileMacros, Indexed, ReadFault};

/// The stack of every thread a file is indexed on: a parse takes a fixed
/// stack whatever its file, and the index's own walks keep theirs on the
/// heap, so this is a margin over what the index of a file at the cap
/// takes, measured in a test build.
const WORKER_STACK_BYTES: usize = 1024 * 1024;

/// The largest file a worker indexes itself; a larger one is indexed on a
/// thread of its own. A worker keeps what its largest parse allocated,
/// about 16 MiB at this size in the costliest shapes measured.
const MAX_BYTES_INDEXED_BY_A_WORKER: usize = 8 * 1024;

/// The bytes of the files indexed at once, between them, so the parses
/// under way allocate what that many bytes of the costliest shapes take,
/// about 2 GiB as measured. A file larger than it is indexed alone.
const MAX_BYTES_INDEXED_AT_ONCE: usize = 1024 * 1024;

/// The indexes of one call, the room their files share, and the calling
/// tree's blank set.
pub(super) struct Indexer {
    room: Room,
    tree: BlankMacros,
}

impl Indexer {
    pub(super) fn new() -> Self {
        Self {
            room: Room {
                free: Mutex::new(MAX_BYTES_INDEXED_AT_ONCE),
                freed: Condvar::new(),
            },
            tree: BlankMacros::default(),
        }
    }

    /// Take the tree's blank sets from `headers`, the paths and the
    /// `#define`s of the calling tree's headers.
    pub(super) fn take_headers(&mut self, headers: &[(String, Defines)]) {
        self.tree = BlankMacros::of(
            headers
                .iter()
                .map(|(path, defines)| (path.as_str(), defines)),
        );
    }

    /// What `each` gives of each of `items`, in their order, run in
    /// parallel on a pool started for the call: as many workers as rayon's
    /// own pool has (`RAYON_NUM_THREADS`), and no more than `items`.
    /// [`ReadFault::NoThread`] when the system refuses a worker; a panic of
    /// `each` is the caller's.
    pub(super) fn map<T: Sync, R: Send>(
        &self,
        items: &[T],
        each: impl Fn(&Worker<'_>, &T) -> R + Sync,
    ) -> Result<Vec<R>, ReadFault> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let worker = Worker {
            room: &self.room,
            tree: &self.tree,
        };
        let threads = rayon::current_num_threads().min(items.len());
        let pool = pool(threads, WORKER_STACK_BYTES)?;
        Ok(pool.install(|| items.par_iter().map(|item| each(&worker, item)).collect()))
    }
}

/// A pool of `threads` workers of `stack` bytes each;
/// [`ReadFault::NoThread`] when the system refuses one.
fn pool(threads: usize, stack: usize) -> Result<ThreadPool, ReadFault> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .stack_size(stack)
        .build()
        .map_err(|error| {
            // A pool's build fails only where the spawn of a thread does.
            let spawn = error.source().and_then(|e| e.downcast_ref::<io::Error>());
            ReadFault::NoThread(spawn.map_or(ErrorKind::Other, io::Error::kind))
        })
}

/// A thread of `stack` bytes in `scope` that runs `work`;
/// [`ReadFault::NoThread`] when the system refuses it.
fn start<'s, 'e, R: Send + 'e>(
    scope: &'s Scope<'s, 'e>,
    stack: usize,
    work: impl FnOnce() -> R + Send + 's,
) -> Result<ScopedJoinHandle<'s, R>, ReadFault> {
    thread::Builder::new()
        .stack_size(stack)
        .spawn_scoped(scope, work)
        .map_err(|error| ReadFault::NoThread(error.kind()))
}

/// A worker of an [`Indexer`], which indexes.
pub(super) struct Worker<'i> {
    room: &'i Room,
    tree: &'i BlankMacros,
}

impl Worker<'_> {
    /// The calling tree's blank set.
    pub(super) const fn tree(&self) -> &BlankMacros {
        self.tree
    }

    /// The index of `bytes`, a file of `kind` that blanks `macros`, built
    /// once the room holds them: by the worker itself up to
    /// [`MAX_BYTES_INDEXED_BY_A_WORKER`], on a thread of its own past it;
    /// [`ReadFault::NoThread`] when the system refuses that thread.
    pub(super) fn index(
        &self,
        kind: FileKind,
        bytes: &[u8],
        macros: &FileMacros,
    ) -> Result<Indexed, ReadFault> {
        let _taken = self.room.take(bytes.len());
        if bytes.len() <= MAX_BYTES_INDEXED_BY_A_WORKER {
            return Ok(code::index(kind, bytes, macros));
        }
        thread::scope(|scope| {
            let parse = start(scope, WORKER_STACK_BYTES, || {
                code::index(kind, bytes, macros)
            })?;
            match parse.join() {
                Ok(indexed) => Ok(indexed),
                Err(payload) => panic::resume_unwind(payload),
            }
        })
    }
}

/// The bytes the files being indexed may still take. Whoever holds bytes
/// of it only indexes, and waits on no other thread but one it started,
/// so a take always ends.
struct Room {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Room {
    /// `bytes` of the room, the whole room at most, once they are free;
    /// given back when the guard drops.
    fn take(&self, bytes: usize) -> Taken<'_> {
        let bytes = bytes.min(MAX_BYTES_INDEXED_AT_ONCE);
        // Nothing panics while it holds the lock, so a poisoned lock still
        // holds a true count.
        let free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
        let mut free = self
            .freed
            .wait_while(free, |free| *free < bytes)
            .unwrap_or_else(PoisonError::into_inner);
        *free -= bytes;
        Taken { room: self, bytes }
    }
}

/// Bytes taken of a [`Room`].
struct Taken<'r> {
    room: &'r Room,
    bytes: usize,
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        let mut free = self
            .room
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *free += self.bytes;
        self.room.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_take_past_the_free_room_waits_until_it_is_given_back() {
        let indexer = Indexer::new();
        let room = &indexer.room;
        let held = room.take(MAX_BYTES_INDEXED_AT_ONCE);
        let (sent, taken) = mpsc::channel();

        thread::scope(|scope| {
            // Past the room, a take waits for all of it.
            scope.spawn(move || {
                drop(room.take(4 * MAX_BYTES_INDEXED_AT_ONCE));
                sent.send(()).unwrap();
            });
            let waited = taken.recv_timeout(Duration::from_millis(200)).is_err();
            drop(held);
            let returned = taken.recv_timeout(Duration::from_secs(10)).is_ok();
            if !returned {
                // Free the take still waiting, so the scope ends and the
                // test fails rather than hangs.
                *room.free.lock().unwrap() = usize::MAX;
                room.freed.notify_all();
            }

            assert!(waited);
            assert!(returned);
        });
    }

    #[test]
    fn a_thread_the_system_refuses_is_no_thread() {
        // No address space holds a stack of 4 EiB.
        let stack = 1 << 62;

        assert!(matches!(pool(1, stack), Err(ReadFault::NoThread(_))));
        let started = thread::scope(|scope| start(scope, stack, || ()).map(|_| ()));
        assert!(matches!(started, Err(ReadFault::NoThread(_))));
    }
}
