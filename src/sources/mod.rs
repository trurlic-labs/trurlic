//! The shell around the code index and the anchor findings: it walks and
//! reads the code root, and, for `check`, the files of the anchors
//! missing in it that the other live worktrees changed since their line
//! forked from the calling tree's, and what git's history says of the
//! stale anchors' files and their repairs; reads and fills the index's
//! cache through the [`Store`]; and hands the facts to `code` and `lint`
//! as data. `mcp` (`status`, `apply`) and `commands` (`check`) stand on
//! it. A caller holds no lock of the store while it runs: the cache's
//! write takes the exclusive lock when it is free, and the caller's own
//! shared lock would keep it busy.
//!
//! A file is read in one bounded read, refused past
//! [`MAX_SOURCE_FILE_BYTES`] and when it is no regular file, and indexed
//! from the cache when an entry holds its key; a C or C++ file's key holds
//! the part of the tree's blank sets it writes, which its lexical pass,
//! cached by content, says (`lexical.rs`). Every read of a call, of the
//! calling tree, another worktree or git's history, and the recheck of a
//! repair under the write lock, goes through one [`Reads`], made by one
//! function from the calling tree's walk and headers, so a file is judged
//! against the same sets whichever read takes it. Indexing runs in
//! parallel, on threads the call starts and ends (`indexer.rs`); a file
//! that vanishes between the walk and the read is absent. The misses of
//! every tree a call reads are written to
//! the cache once, after
//! the call's own work, under the exclusive lock when it is free: a busy
//! lock skips the write, since the cache is never authoritative.

mod blobs;
mod cache;
mod indexer;
mod lexical;
mod renames;
mod repair;
mod walk;
mod worktrees;

pub(crate) use worktrees::UnreadWorktree;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

use crate::code::{
    self, CodeFacts, Entry, FileFault, FileKind, FileMacros, Indexed, ReadFault, Resolution,
};
use crate::console::diag;
use crate::lint::anchors::{self, Asked, Checked, Decisions, Elsewhere, Finding, Here, Unasked};
use crate::store::{Anchor, Decision, Store, SymbolsKey};

use blobs::Blobs;
use cache::Content;
use indexer::{Indexer, Worker};
use lexical::Cached;
use walk::Probe;

/// The largest source file the shell reads: the largest of any kind the
/// index reads (`FileKind::max_bytes`), which refuses one over its kind's.
pub(crate) const MAX_SOURCE_FILE_BYTES: u64 = code::MAX_SOURCE_BYTES as u64;

/// The anchor findings `check` reports, and the other worktrees git
/// could not say enough of to read.
#[derive(Debug, Default)]
pub(crate) struct CheckedFindings<'s> {
    pub(crate) findings: Vec<Finding<'s, Checked>>,
    pub(crate) unread: Vec<UnreadWorktree>,
}

/// The anchor findings of `decisions` that `check` reports, judged in the
/// tree at `store`'s code root and, for an anchor of a decision ever
/// accepted that is missing there, in the repository's other worktrees
/// that changed its file since their line forked from the calling tree's.
/// Decisions with no anchor read no file.
pub(crate) fn checked_findings<'s>(
    store: &Store,
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> CheckedFindings<'s> {
    let decisions = anchored(decisions);
    if decisions.is_empty() {
        return CheckedFindings::default();
    }
    let mut reads = Reads::of_tree(store);
    let facts = calling_tree(store, &decisions, &mut reads);
    let here = Here::of(&decisions, &facts);
    let mut asked = Asked::new(here.unresolved(&decisions));
    let unread = elsewhere(store, &facts, &mut asked, &mut reads);
    let asked = repair::with_history(store, &facts, &here, asked, &mut reads);
    reads.write(store);
    CheckedFindings {
        findings: anchors::findings(&decisions, &here, &asked),
        unread,
    }
}

/// The anchor findings of `decisions` that `status` reports, judged in
/// the tree at `store`'s code root alone: the project's git is asked
/// nothing. Decisions with no anchor read no file.
pub(crate) fn tree_findings<'s>(
    store: &Store,
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> Vec<Finding<'s, Unasked>> {
    let decisions = anchored(decisions);
    if decisions.is_empty() {
        return Vec::new();
    }
    let mut reads = Reads::of_tree(store);
    let facts = calling_tree(store, &decisions, &mut reads);
    reads.write(store);
    anchors::unasked(&decisions, &Here::of(&decisions, &facts))
}

/// Keep of `repairs`, a map from stale anchor to repair read before the
/// write lock, each that still holds in the tree at `store`'s code root,
/// read under the lock from the files the repairs name alone, as the tree
/// read judges them, with no git: its stale anchor names nothing there and
/// its repair resolves. Give each repair dropped, by its stale anchor,
/// with why.
pub(crate) fn still_holding(
    store: &Store,
    repairs: &mut BTreeMap<Anchor, Anchor>,
) -> BTreeMap<Anchor, String> {
    if repairs.is_empty() {
        return BTreeMap::new();
    }
    let paths: BTreeSet<&str> = repairs
        .iter()
        .flat_map(|(from, to)| [from.file(), to.file()])
        .collect();
    let facts = read_alone(store, paths);
    let mut dropped = BTreeMap::new();
    repairs.retain(|from, to| {
        let stale = matches!(
            facts.resolve(from),
            Resolution::FileMissing | Resolution::SymbolMissing
        );
        let holds = stale && matches!(facts.resolve(to), Resolution::Resolved(_));
        if !holds {
            let cause = "its code is back, or its repair names nothing, since the repairs were \
                         read; run `repair_anchors` again";
            // clone: the map dropped outlives the one kept.
            dropped.insert(from.clone(), cause.to_owned());
        }
        holds
    });
    dropped
}

/// The facts of `paths` in the tree at `store`'s code root, read through
/// the reads of the tree, with no other file of it; the cache is read, not
/// written, since the caller holds the write lock.
fn read_alone(store: &Store, paths: BTreeSet<&str>) -> CodeFacts {
    let mut reads = Reads::of_tree(store);
    let root = store.code_root();
    let (entries, _) = read_tree(store, root, Scope::Anchored, paths.into_iter(), &mut reads);
    CodeFacts::new(entries, BTreeSet::new())
}

/// The decisions of `decisions` that hold an anchor, by name.
fn anchored<'s>(
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> Vec<(&'s str, &'s Decision)> {
    decisions
        .into_iter()
        .map(|(name, decision)| (name.as_str(), decision))
        .filter(|(_, decision)| !decision.anchors.is_empty())
        .collect()
}

/// The facts of the calling tree: every source the walk takes, and the
/// paths `decisions` anchor.
fn calling_tree(store: &Store, decisions: &Decisions<'_>, reads: &mut Reads) -> CodeFacts {
    let anchored = decisions
        .iter()
        .flat_map(|(_, decision)| &decision.anchors)
        .map(Anchor::file);
    let (entries, walked) = read_tree(store, store.code_root(), Scope::Tree, anchored, reads);
    CodeFacts::new(entries, walked)
}

/// Read the other worktrees into `asked` for its stale anchors, those of
/// decisions ever accepted missing in the calling tree, whose facts `here`
/// are, and give those git could not say enough of; none is asked when
/// nothing is missing. Each worktree, in git's order, is read for the
/// anchors no earlier one holds at the paths its work tree changed since
/// the fork, and no further worktree is read once every one is held. Those
/// files alone are read, never a walk of the tree, against the calling
/// tree's crates and module declarations, there and as the fork held them,
/// and its blank set.
fn elsewhere(
    store: &Store,
    here: &CodeFacts,
    asked: &mut Asked<'_>,
    reads: &mut Reads,
) -> Vec<UnreadWorktree> {
    if asked.stale().is_empty() {
        return Vec::new();
    }
    let others = worktrees::others(store.code_root());
    let mut unread = others.unread;
    for forked in others.trees {
        if asked.stale().is_empty() {
            break;
        }
        let paths: BTreeSet<&str> = asked.stale().iter().map(|&anchor| anchor.file()).collect();
        let paths: Vec<&str> = paths.into_iter().collect();
        let changed = match forked.changed(&paths) {
            Ok(changed) if changed.is_empty() => continue,
            Ok(changed) => changed,
            Err(error) => {
                unread.push(UnreadWorktree {
                    worktree: Some(forked.top),
                    cause: error.into(),
                });
                continue;
            }
        };
        let fork = forked.base().map(|base| {
            let blobs = Blobs {
                root: &forked.top,
                indexer: &reads.indexer,
            };
            at_fork(here, &blobs, base, &changed)
        });
        let facts = here.beside(&read_paths(store, &forked.top, changed, reads));
        asked.read(Elsewhere {
            worktree: forked.worktree,
            facts,
            fork,
        });
    }
    unread
}

/// The facts of `paths` at commit `base`, read from git beside `here`, the
/// calling tree's.
fn at_fork(here: &CodeFacts, blobs: &Blobs<'_>, base: &str, paths: &BTreeSet<String>) -> CodeFacts {
    // clone: each read names its revision and path.
    let asked = paths
        .iter()
        .map(|path| (base.to_owned(), path.clone()))
        .collect();
    let mut read: BTreeMap<String, Option<Entry>> =
        paths.iter().map(|path| (path.clone(), None)).collect();
    for (_, path, entry) in blobs.read(&asked) {
        read.insert(path, Some(entry));
    }
    here.beside(&read)
}

/// What is at each of `paths` in the tree at `root`, `None` where nothing
/// is.
fn read_paths(
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
enum Scope {
    /// Every source the walk of the calling tree took (the reads'), once a
    /// call, the anchored paths, and the manifests above an anchored Rust
    /// file the walk does not take.
    Tree,
    /// The anchored paths alone.
    Anchored,
}

/// What is in the tree at `root` by `scope`, by path, and which paths the
/// walk took. Its cache misses and the keys it read go to `reads`.
fn read_tree<'a>(
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

/// What reading a file gave.
enum Sourced {
    Fault(ReadFault),
    /// Its index, and the cache entries it used: the index's and, for a C
    /// or C++ file, its lexical pass's.
    Indexed {
        indexed: Box<Indexed>,
        index: Used,
        lexed: Option<Used>,
    },
}

/// A cache entry a read used: its key, and its bytes to write when the
/// cache held none.
struct Used {
    key: SymbolsKey,
    miss: Option<Vec<u8>>,
}

/// Read the file at `path` under `root`, of `kind`, and index it, from
/// the cache when it holds the index; `None` when the file is gone.
fn read_and_index(
    cached: &Cached<'_>,
    worker: &Worker<'_>,
    (root, path, kind): (&Path, &str, FileKind),
) -> Option<Sourced> {
    match read_source(&root.join(path)) {
        Ok(bytes) => Some(index_bytes(cached, worker, (path, kind), &bytes)),
        Err(Gone) => None,
        Err(Faulty(fault)) => Some(Sourced::Fault(fault)),
    }
}

/// The index of `bytes`, the file at `path` of `kind`: the cache's, else
/// built.
fn index_bytes(
    cached: &Cached<'_>,
    worker: &Worker<'_>,
    (path, kind): (&str, FileKind),
    bytes: &[u8],
) -> Sourced {
    let content = Content::of(bytes);
    let (macros, lexed) = match kind.blanks_macros() {
        true => {
            let found = cached.lexed_or_lex(content, bytes);
            let lexed = Used {
                key: found.key,
                miss: found.miss,
            };
            (worker.tree().of_file(path, &found.lexed), Some(lexed))
        }
        false => (FileMacros::default(), None),
    };
    let key = cache::key(kind, content, &macros);
    let (indexed, miss) = match cached.index(key, kind, bytes.len()) {
        Some(indexed) => (indexed, None),
        None => match worker.index(kind, bytes, &macros) {
            Ok(indexed) => {
                let miss = Some(cache::encode(key, &indexed));
                (indexed, miss)
            }
            Err(fault) => return Sourced::Fault(fault),
        },
    };
    Sourced::Indexed {
        indexed: Box::new(indexed),
        index: Used { key, miss },
        lexed,
    }
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

/// What a call's reads share: the calling tree's walk, until its read
/// takes it; the indexer its files are indexed through, which holds the
/// calling tree's blank sets; and what the reads give the cache, the
/// entries it lacked, by key, so two files of one content are one entry,
/// and every key read, which bounds the cache. One call fills it once,
/// with the keys of every tree it read, so a few paths of another worktree
/// never prune the entries of the calling tree.
struct Reads {
    walked: BTreeMap<String, FileKind>,
    indexer: Indexer,
    misses: BTreeMap<SymbolsKey, Vec<u8>>,
    live: BTreeSet<SymbolsKey>,
}

impl Reads {
    /// The reads of a call in the tree at `store`'s code root: its walk,
    /// and its headers read for the indexer's blank sets. The one maker of
    /// an indexer, so no file is ever indexed against other sets.
    fn of_tree(store: &Store) -> Self {
        let root = store.code_root();
        let mut reads = Self {
            walked: walk::tree_sources(root).into_iter().collect(),
            indexer: Indexer::new(),
            misses: BTreeMap::new(),
            live: BTreeSet::new(),
        };
        lexical::read_headers(store, &mut reads, root);
        reads
    }

    /// The entry of a file of `kind` that `read` gave, the cache entries
    /// it used noted; `None` for a file gone.
    fn take(&mut self, kind: FileKind, read: Option<Sourced>) -> Option<Entry> {
        let index = match read? {
            Sourced::Fault(fault) => Err(FileFault::Read(fault)),
            Sourced::Indexed {
                indexed,
                index,
                lexed,
            } => {
                for used in std::iter::once(index).chain(lexed) {
                    self.note(used.key, used.miss);
                }
                (*indexed).map_err(FileFault::Index)
            }
        };
        Some(Entry::File { kind, index })
    }

    fn note(&mut self, key: SymbolsKey, miss: Option<Vec<u8>>) {
        self.live.insert(key);
        if let Some(bytes) = miss {
            self.misses.insert(key, bytes);
        }
    }

    /// Write the misses when the exclusive lock is free. A failure costs
    /// the next call a re-index, so it is reported and the call goes on.
    fn write(&self, store: &Store) {
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

#[cfg(test)]
mod tests;
