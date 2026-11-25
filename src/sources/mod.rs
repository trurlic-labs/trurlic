//! The shell around the code index: it walks and reads the code root,
//! reads and fills the index's cache through the [`Store`], and hands the
//! facts to `code` as data. Its entry point is [`resolve_anchors`], which
//! no command calls yet but the benchmark; it is a module of its own
//! because `mcp` and `commands` are both to read it, and neither owns
//! the other. A caller holds no lock of the store while it runs: the
//! cache's write takes the exclusive lock when it is free, and the
//! caller's own shared lock would keep it busy.
//!
//! A file is read in one bounded read, refused past
//! [`MAX_SOURCE_FILE_BYTES`] and when it is no regular file, and indexed
//! from the cache when an entry holds its key. Indexing runs in parallel;
//! a file that vanishes between the walk and the read is absent. The
//! misses are written to the cache after the call's own work, under the
//! exclusive lock when it is free: a busy lock skips the write, since the
//! cache is never authoritative.

mod cache;
mod walk;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

use rayon::prelude::*;

use crate::code::{self, CodeFacts, Entry, FileFault, FileKind, Indexed, ReadFault, Resolution};
use crate::console::diag;
use crate::store::{Anchor, ProjectState, Store, SymbolsKey};

use walk::Probe;

/// The largest source file the shell reads: the largest of any kind the
/// index reads (`FileKind::max_bytes`), which refuses one over its kind's.
pub(crate) const MAX_SOURCE_FILE_BYTES: u64 = code::MAX_SOURCE_BYTES as u64;

/// What each anchor of `state`'s decisions names in the tree at `store`'s
/// code root.
#[cfg_attr(
    not(any(test, feature = "bench")),
    expect(
        dead_code,
        reason = "the anchor findings of `status` and `check` are the first callers"
    )
)]
pub(crate) fn resolve_anchors<'s>(
    store: &Store,
    state: &'s ProjectState,
) -> BTreeMap<&'s Anchor, Resolution> {
    let anchors: BTreeSet<&Anchor> = state
        .decisions
        .values()
        .flat_map(|decision| &decision.anchors)
        .collect();
    let facts = gather(store, anchors.iter().map(|anchor| anchor.file()));
    anchors
        .into_iter()
        .map(|anchor| (anchor, facts.resolve(anchor)))
        .collect()
}

/// The facts of the tree at `store`'s code root: every source the walk
/// takes, and what is at each of `anchored`.
fn gather<'a>(store: &Store, anchored: impl Iterator<Item = &'a str>) -> CodeFacts {
    let root = store.code_root();
    let walked: BTreeMap<String, FileKind> = walk::tree_sources(root).into_iter().collect();
    let (mut entries, anchored) = probe_anchored(root, anchored, &walked);
    // clone: the facts tell the files the walk took from those an anchor
    // brought in.
    let walked_paths: BTreeSet<String> = walked.keys().cloned().collect();
    let mut to_read = walked;
    to_read.extend(anchored);
    let read: Vec<(String, FileKind, Option<Sourced>)> = to_read
        .into_par_iter()
        .map(|(path, kind)| {
            let read = read_and_index(store, &root.join(&path), kind);
            (path, kind, read)
        })
        .collect();
    let mut live = BTreeSet::new();
    // By key: two files of one content are one entry.
    let mut misses = BTreeMap::new();
    for (path, kind, read) in read {
        let index = match read {
            None => continue,
            Some(Sourced::Fault(fault)) => Err(FileFault::Read(fault)),
            Some(Sourced::Indexed { key, indexed, miss }) => {
                live.insert(key);
                if let Some(bytes) = miss {
                    misses.insert(key, bytes);
                }
                indexed.map_err(FileFault::Index)
            }
        };
        entries.insert(path, Entry::File { kind, index });
    }
    fill_cache(store, &misses, &live);
    CodeFacts::new(entries, &walked_paths)
}

/// What is at each of `anchored` under `root`: the entries of the
/// directories and of the files no resolver reads, and the files to read,
/// with the manifests above each anchored Rust file the walk does not
/// take, which its crate is read from.
fn probe_anchored<'a>(
    root: &Path,
    anchored: impl Iterator<Item = &'a str>,
    walked: &BTreeMap<String, FileKind>,
) -> (BTreeMap<String, Entry>, BTreeMap<String, FileKind>) {
    let mut entries = BTreeMap::new();
    let mut to_read = BTreeMap::new();
    for path in anchored {
        match (walk::probe(root, path), FileKind::of(path)) {
            (Probe::Absent, _) => {}
            (Probe::Directory, _) => {
                entries.insert(path.to_owned(), Entry::Directory);
            }
            (Probe::Other, Some(kind)) => {
                if kind == FileKind::Rust && !walked.contains_key(path) {
                    to_read.extend(walk::manifests_above(root, path));
                }
                to_read.insert(path.to_owned(), kind);
            }
            (Probe::Other, None) => {
                entries.insert(path.to_owned(), Entry::Unindexed);
            }
        }
    }
    (entries, to_read)
}

/// What reading a file gave.
enum Sourced {
    Fault(ReadFault),
    /// Its index under `key`, with the cache entry to write when the cache
    /// held none.
    Indexed {
        key: SymbolsKey,
        indexed: Indexed,
        miss: Option<Vec<u8>>,
    },
}

/// Read the file at `path` and index it, from the cache when it holds the
/// index; `None` when the file is gone.
fn read_and_index(store: &Store, path: &Path, kind: FileKind) -> Option<Sourced> {
    let bytes = match read_source(path) {
        Ok(bytes) => bytes,
        Err(Gone) => return None,
        Err(Faulty(fault)) => return Some(Sourced::Fault(fault)),
    };
    let key = cache::key(kind, &bytes);
    let cached = store
        .read_symbols(key, cache::max_entry_bytes(kind, bytes.len()))
        .and_then(|entry| cache::decode(key, &entry, bytes.len()));
    Some(match cached {
        Some(indexed) => Sourced::Indexed {
            key,
            indexed,
            miss: None,
        },
        None => {
            let indexed = code::index(kind, &bytes);
            let miss = Some(cache::encode(key, &indexed));
            Sourced::Indexed { key, indexed, miss }
        }
    })
}

/// Why a source file gave no bytes.
enum Unread {
    Gone,
    Faulty(ReadFault),
}
use Unread::{Faulty, Gone};

/// The bytes of the file at `path`, symlinks followed: refused unopened
/// when it is no regular file, so a FIFO never blocks the read, and
/// unread past [`MAX_SOURCE_FILE_BYTES`].
fn read_source(path: &Path) -> Result<Vec<u8>, Unread> {
    let unread = |e: std::io::Error| match e.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => Gone,
        kind => Faulty(ReadFault::Unreadable(kind)),
    };
    let metadata = fs::metadata(path).map_err(unread)?;
    let too_large = |bytes| {
        Faulty(ReadFault::TooLarge {
            bytes,
            max: MAX_SOURCE_FILE_BYTES,
        })
    };
    if !metadata.is_file() {
        return Err(Faulty(ReadFault::NotAFile));
    }
    if metadata.len() > MAX_SOURCE_FILE_BYTES {
        return Err(too_large(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)
        .map_err(unread)?
        .take(MAX_SOURCE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unread)?;
    match u64::try_from(bytes.len()) {
        Ok(read) if read <= MAX_SOURCE_FILE_BYTES => Ok(bytes),
        Ok(read) => Err(too_large(read)),
        Err(_) => Err(too_large(u64::MAX)),
    }
}

/// Write `misses` to the cache when the exclusive lock is free, and bound
/// the cache by `live`, the keys this call read. A failure costs the next
/// call a re-index, so it is reported and the call goes on.
fn fill_cache(store: &Store, misses: &BTreeMap<SymbolsKey, Vec<u8>>, live: &BTreeSet<SymbolsKey>) {
    if misses.is_empty() {
        return;
    }
    let written = store.try_lock().and_then(|lock| match lock {
        Some(lock) => store.write_symbols(&lock, misses, live),
        None => Ok(()),
    });
    if let Err(error) = written {
        diag!("warning: the code index's cache was not written: {error}");
    }
}

#[cfg(test)]
mod tests;
