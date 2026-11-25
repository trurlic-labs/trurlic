//! The threads the shell indexes files on. A call starts its workers and
//! ends them as it returns, and [`Worker::index`] is the shell's one call
//! of [`code::index`].
//!
//! tree-sitter releases the stack of an ambiguous parse recursively, as
//! deep as the file runs, so a parse needs stack in proportion to its
//! file, and a thread keeps the memory its parses freed until it ends. A
//! worker indexes a file up to [`code::MAX_BYTES_INDEXED_BY_A_WORKER`]
//! itself, on its stack of [`code::WORKER_STACK_BYTES`], and starts a
//! thread of [`code::PARSE_STACK_BYTES`] for a larger one, whose memory
//! goes back when that thread ends. The files indexed at once hold at most
//! [`code::MAX_BYTES_INDEXED_AT_ONCE`] bytes between them.

use std::num::NonZeroUsize;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, Scope, ScopedJoinHandle};

use crate::code::{self, FileKind, Indexed, ReadFault};

/// The indexes of one call, and the room their files share.
pub(super) struct Indexer {
    room: Room,
}

impl Indexer {
    pub(super) fn new() -> Self {
        Self {
            room: Room {
                free: Mutex::new(code::MAX_BYTES_INDEXED_AT_ONCE),
                freed: Condvar::new(),
            },
        }
    }

    /// What `each` gives of each of `items`, in their order, run in
    /// parallel on workers started for the call, one a core;
    /// [`ReadFault::NoThread`] when the system starts none. A panic of
    /// `each` is the caller's.
    pub(super) fn map<T: Sync, R: Send>(
        &self,
        items: &[T],
        each: impl Fn(&Worker<'_>, &T) -> R + Sync,
    ) -> Result<Vec<R>, ReadFault> {
        let next = AtomicUsize::new(0);
        let worker = Worker { room: &self.room };
        let work = || {
            let mut done = Vec::new();
            loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(at) else {
                    return done;
                };
                done.push((at, each(&worker, item)));
            }
        };
        let cores = thread::available_parallelism().map_or(1, NonZeroUsize::get);
        thread::scope(|scope| {
            let first = start(scope, code::WORKER_STACK_BYTES, work)?;
            // A worker the system refuses leaves its share to the others.
            let others = (1..cores.min(items.len()))
                .map_while(|_| start(scope, code::WORKER_STACK_BYTES, work).ok());
            let mut done: Vec<(usize, R)> = Vec::with_capacity(items.len());
            for started in std::iter::once(first).chain(others.collect::<Vec<_>>()) {
                done.extend(joined(started));
            }
            done.sort_unstable_by_key(|(at, _)| *at);
            Ok(done.into_iter().map(|(_, value)| value).collect())
        })
    }
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

/// What the thread `started` gave; its panic, the caller's.
fn joined<R>(started: ScopedJoinHandle<'_, R>) -> R {
    match started.join() {
        Ok(value) => value,
        Err(payload) => panic::resume_unwind(payload),
    }
}

/// A worker of an [`Indexer`], which indexes.
pub(super) struct Worker<'i> {
    room: &'i Room,
}

impl Worker<'_> {
    /// The index of `bytes`, a file of `kind`, built once the room holds
    /// them: by the worker itself up to
    /// [`code::MAX_BYTES_INDEXED_BY_A_WORKER`], on a thread of its own past
    /// it; [`ReadFault::NoThread`] when the system refuses that thread.
    pub(super) fn index(&self, kind: FileKind, bytes: &[u8]) -> Result<Indexed, ReadFault> {
        let _taken = self.room.take(bytes.len());
        if bytes.len() <= code::MAX_BYTES_INDEXED_BY_A_WORKER {
            return Ok(code::index(kind, bytes));
        }
        thread::scope(|scope| {
            let parse = start(scope, code::PARSE_STACK_BYTES, || code::index(kind, bytes))?;
            Ok(joined(parse))
        })
    }
}

/// The bytes the files being indexed may still take.
struct Room {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Room {
    /// `bytes` of the room, the whole room at most, once they are free;
    /// given back when the guard drops.
    fn take(&self, bytes: usize) -> Taken<'_> {
        let bytes = bytes.min(code::MAX_BYTES_INDEXED_AT_ONCE);
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

    const ROOM: usize = code::MAX_BYTES_INDEXED_AT_ONCE;

    /// Whether a take of `bytes` from `room` returns within a second,
    /// tried on a thread of its own; the take is given back at once, and
    /// one still waiting then is let through, the room left as it was.
    fn takes(room: &Room, bytes: usize) -> bool {
        let (sent, received) = mpsc::channel();
        let returned = thread::scope(|scope| {
            scope.spawn(move || {
                drop(room.take(bytes));
                sent.send(()).unwrap();
            });
            let returned = received.recv_timeout(Duration::from_secs(1)).is_ok();
            if !returned {
                *room.free.lock().unwrap() += ROOM;
                room.freed.notify_all();
            }
            returned
        });
        if !returned {
            *room.free.lock().unwrap() -= ROOM;
        }
        returned
    }

    #[test]
    fn bytes_the_room_lacks_wait_and_a_file_past_the_room_takes_all_of_it() {
        let indexer = Indexer::new();
        let room = &indexer.room;
        let half = room.take(ROOM / 2);
        let other = room.take(ROOM / 2);

        assert!(!takes(room, 1));
        drop(half);
        assert!(takes(room, ROOM / 2));
        drop(other);
        assert!(takes(room, 4 * ROOM));
    }
}
