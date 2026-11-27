//! What a call reads of a tree: the files its walk and its anchors name,
//! read and indexed in parallel, and what the reads give the cache. Every
//! read of a call, of any tree, goes through one [`Reads`], so a file is
//! judged against the same blank sets whichever read takes it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::code::{self, Entry, FileFault, FileKind};
use crate::console::diag;
use crate::store::{Store, SymbolsKey};

use super::cache::Content;
use super::indexer::Indexer;
use super::lexical::Cached;
use super::source::{Sourced, read_and_index, read_source};
use super::walk::{self, Probe};

/// What is at each of `paths` in the tree at `root`, `None` where nothing
/// is.
pub(super) fn read_paths(
    store: &Store,
    root: &Path,
    paths: BTreeSet<String>,
    reads: &mut Reads,
) -> BTreeMap<String, Option<Entry>> {
    let anchored = paths.iter().map(String::as_str);
    let (mut found, _) = read_tree(store, root, Scope::Anchored, anchored, reads);
    paths
        .into_iter()
        .map(|path| {
            let entry = found.remove(&path);
            (path, entry)
        })
        .collect()
}

/// Which files a gather reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// Every source the walk of the calling tree took (the reads'), once a
    /// call, the anchored paths, and the manifests above an anchored Rust
    /// file the walk does not take.
    Tree,
    /// The anchored paths alone.
    Anchored,
}

/// What is in the tree at `root` by `scope`, by path, and which paths the
/// walk took. Its cache misses and the keys it read go to `reads`.
pub(super) fn read_tree<'a>(
    store: &Store,
    root: &Path,
    scope: Scope,
    anchored: impl Iterator<Item = &'a str>,
    reads: &mut Reads,
) -> (BTreeMap<String, Entry>, BTreeSet<String>) {
    let walked = match scope {
        Scope::Tree => std::mem::take(&mut reads.walked),
        Scope::Anchored => BTreeMap::new(),
    };
    let (mut entries, anchored) = probe_anchored(root, anchored, &walked, scope);
    // clone: the facts tell the files the walk took from those an anchor
    // brought in.
    let walked_paths: BTreeSet<String> = walked.keys().cloned().collect();
    let mut to_read = walked;
    to_read.extend(anchored);
    let to_read: Vec<(String, FileKind)> = to_read.into_iter().collect();
    let cached = Cached {
        store,
        misses: &reads.misses,
    };
    let read = reads.indexer.map(&to_read, |worker, (path, kind)| {
        read_and_index(&cached, worker, (root, path, *kind))
    });
    let read = read.unwrap_or_else(|fault| {
        to_read
            .iter()
            .map(|_| Some(Sourced::Fault(fault)))
            .collect()
    });
    for ((path, kind), read) in to_read.into_iter().zip(read) {
        if let Some(entry) = reads.take(kind, read) {
            entries.insert(path, entry);
        }
    }
    (entries, walked_paths)
}

/// What is at each of `anchored` under `root`: the entries of the
/// directories and of the files no resolver reads, and the files to read,
/// with, for `Scope::Tree`, the manifests above each anchored Rust file
/// the walk does not take, which its crate is read from.
fn probe_anchored<'a>(
    root: &Path,
    anchored: impl Iterator<Item = &'a str>,
    walked: &BTreeMap<String, FileKind>,
    scope: Scope,
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
                let outside = kind == FileKind::Rust && !walked.contains_key(path);
                if outside && scope == Scope::Tree {
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

/// What a call's reads share: the calling tree's walk, until its read
/// takes it; the indexer its files are indexed through, which holds the
/// calling tree's blank sets; and what the reads give the cache, the
/// entries it lacked, by key, so two files of one content are one entry,
/// and every key read, which bounds the cache. One call fills it once,
/// with the keys of every tree it read, so a few paths of another worktree
/// never prune the entries of the calling tree.
pub(super) struct Reads {
    walked: BTreeMap<String, FileKind>,
    pub(super) indexer: Indexer,
    pub(super) misses: BTreeMap<SymbolsKey, Vec<u8>>,
    live: BTreeSet<SymbolsKey>,
}

impl Reads {
    /// The reads of a call in the tree at `store`'s code root: its walk,
    /// and its headers read for the indexer's blank sets. The one maker of
    /// an indexer, so no file is ever indexed against other sets.
    pub(super) fn of_tree(store: &Store) -> Self {
        let root = store.code_root();
        let mut reads = Self {
            walked: walk::tree_sources(root).into_iter().collect(),
            indexer: Indexer::new(),
            misses: BTreeMap::new(),
            live: BTreeSet::new(),
        };
        read_headers(store, &mut reads, root);
        reads
    }

    /// The entry of a file of `kind` that `read` gave, the cache entries
    /// it used noted; `None` for a file gone.
    pub(super) fn take(&mut self, kind: FileKind, read: Option<Sourced>) -> Option<Entry> {
        let (lines, index) = match read? {
            Sourced::Fault(fault) => (None, Err(FileFault::Read(fault))),
            Sourced::Unindexed { lines, fault } => (Some(lines), Err(FileFault::Read(fault))),
            Sourced::Indexed {
                lines,
                indexed,
                index,
                lexed,
            } => {
                for used in std::iter::once(index).chain(lexed) {
                    self.note(used.key, used.miss);
                }
                (Some(lines), (*indexed).map_err(FileFault::Index))
            }
        };
        Some(Entry::File { kind, lines, index })
    }

    fn note(&mut self, key: SymbolsKey, miss: Option<Vec<u8>>) {
        self.live.insert(key);
        if let Some(bytes) = miss {
            self.misses.insert(key, bytes);
        }
    }

    /// Write the misses when the exclusive lock is free. A failure costs
    /// the next call a re-index, so it is reported and the call goes on.
    pub(super) fn write(&self, store: &Store) {
        if self.misses.is_empty() {
            return;
        }
        let written = store.try_lock().and_then(|lock| match lock {
            Some(lock) => store.write_symbols(&lock, &self.misses, &self.live),
            None => Ok(()),
        });
        if let Err(error) = written {
            diag!("warning: the code index's cache was not written: {error}");
        }
    }
}

/// Read the headers of the tree at `root` that `reads` walked, each in a
/// bounded read and within its kind's size, and give their `#define`s to
/// its indexer as the tree's blank sets. A header that cannot be read
/// gives none, as one that vanished; a system that starts no thread leaves
/// the sets empty, and the files' own reads then fail alike.
fn read_headers(store: &Store, reads: &mut Reads, root: &Path) {
    let headers: Vec<(String, FileKind)> = reads
        .walked
        .iter()
        .filter(|(path, kind)| kind.blanks_macros() && code::is_header(path))
        // clone: the sets name each header by its path apart from the walk.
        .map(|(path, kind)| (path.clone(), *kind))
        .collect();
    let cached = Cached {
        store,
        misses: &reads.misses,
    };
    let read = reads.indexer.map(&headers, |_, (path, kind)| {
        let bytes = read_source(&root.join(path)).ok()?;
        if bytes.len() > kind.max_bytes() {
            return None;
        }
        Some(cached.lexed_or_lex(Content::of(&bytes), &bytes))
    });
    let Ok(read) = read else {
        diag!("warning: the C and C++ macros to blank were not read: no thread started");
        return;
    };
    let mut defines = Vec::with_capacity(read.len());
    for ((path, _), read) in headers.into_iter().zip(read) {
        let Some(found) = read else {
            continue;
        };
        reads.note(found.key, found.miss);
        defines.push((path, found.lexed.into_defines()));
    }
    reads.indexer.take_headers(&defines);
}
