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
//! from the cache when an entry holds its key. Indexing runs in parallel;
//! a file that vanishes between the walk and the read is absent. The
//! misses of every tree a call reads are written to the cache once, after
//! the call's own work, under the exclusive lock when it is free: a busy
//! lock skips the write, since the cache is never authoritative.

mod cache;
mod renames;
mod walk;
mod worktrees;

pub(crate) use worktrees::UnreadWorktree;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

use rayon::prelude::*;

use crate::code::{self, CodeFacts, Entry, FileFault, FileKind, Indexed, ReadFault};
use crate::console::diag;
use crate::git;
use crate::lint::anchors::{
    self, Asked, Checked, Decisions, Elsewhere, Finding, Here, Prior, Renames, Unasked,
};
use crate::store::{Anchor, Decision, Store, SymbolsKey};

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
    let mut fill = CacheFill::default();
    let facts = calling_tree(store, &decisions, &mut fill);
    let here = Here::of(&decisions, &facts);
    let unresolved = here.unresolved(&decisions);
    let (elsewhere, unread) = elsewhere(store, &facts, &unresolved, &mut fill);
    let asked = Asked::of(&unresolved, &elsewhere);
    let asked = with_history(store, &facts, &here, asked, &mut fill);
    fill.write(store);
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
    let mut fill = CacheFill::default();
    let facts = calling_tree(store, &decisions, &mut fill);
    fill.write(store);
    anchors::unasked(&decisions, &Here::of(&decisions, &facts))
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
fn calling_tree(store: &Store, decisions: &Decisions<'_>, fill: &mut CacheFill) -> CodeFacts {
    let anchored = decisions
        .iter()
        .flat_map(|(_, decision)| &decision.anchors)
        .map(Anchor::file);
    let (entries, walked) = read_tree(store, store.code_root(), Scope::Tree, anchored, fill);
    CodeFacts::new(entries, &walked)
}

/// `asked` with what git says of the stale anchors' repairs: where their
/// gone files went and, for a rename that changed the file, its old path
/// before it; then, for the symbol search's candidates, each anchor's file
/// and candidate at the revision where that file last held its code.
fn with_history<'s, 'w>(
    store: &Store,
    facts: &CodeFacts,
    here: &Here<'s, '_>,
    asked: Asked<'s, 'w>,
    fill: &mut CacheFill,
) -> Asked<'s, 'w> {
    let root = store.code_root();
    let renames = renames(store, facts, &asked.gone(here), fill);
    let before: BTreeSet<(String, String)> = renames
        .to
        .iter()
        .filter(|(_, renamed)| !renamed.exact)
        // clone: the reads are keyed by revision and path apart from the map.
        .map(|(from, renamed)| (renamed.before.clone(), from.clone()))
        .collect();
    let asked = asked.with_renames(renames).with_prior(prior(root, &before));
    let searched = asked.searched(here);
    if searched.is_empty() {
        return asked;
    }
    let files: BTreeSet<&str> = searched.iter().map(|search| search.anchor.file()).collect();
    let revisions = renames::revisions(root, &files);
    let mut wanted = BTreeSet::new();
    for search in &searched {
        if let Some(revision) = revisions.get(search.anchor.file()) {
            // clone: each read names its revision and path.
            wanted.insert((revision.clone(), search.anchor.file().to_owned()));
            // clone: as above, for the candidate.
            wanted.insert((revision.clone(), search.target.clone()));
        }
    }
    let mut evidence = prior(root, &wanted);
    for (file, revision) in revisions {
        evidence.judge_at(file, revision);
    }
    asked.with_prior(evidence)
}

/// Where git says each of `gone`, the files of stale anchors with nothing
/// at their path, was renamed to, and the facts of those paths beside
/// `here`, the calling tree's, read as an anchor's path is.
fn renames(
    store: &Store,
    here: &CodeFacts,
    gone: &BTreeSet<&str>,
    fill: &mut CacheFill,
) -> Renames {
    let to = renames::renamed(store.code_root(), gone);
    if to.is_empty() {
        return Renames::none();
    }
    // clone: the paths are read apart from the map that keeps them.
    let paths = to.values().map(|renamed| renamed.to.clone()).collect();
    let facts = here.beside(&read_paths(store, store.code_root(), paths, fill));
    Renames { to, facts }
}

/// What git's history held of each of `files`, a revision and a path, each
/// read in a bounded read and indexed; a file git holds no blob of there,
/// or any failure of git, leaves it out.
fn prior(root: &Path, files: &BTreeSet<(String, String)>) -> Prior {
    let mut prior = Prior::default();
    if files.is_empty() {
        return prior;
    }
    // clone: git is handed the files as a list.
    let asked: Vec<(String, String)> = files.iter().cloned().collect();
    let Ok(blobs) = git::renames::blobs(root, &asked, MAX_SOURCE_FILE_BYTES) else {
        return prior;
    };
    for ((revision, path), bytes) in blobs {
        let Some(kind) = FileKind::of(&path) else {
            continue;
        };
        let index = code::index(kind, &bytes).map_err(FileFault::Index);
        prior.insert(revision, path, Entry::File { kind, index });
    }
    prior
}

/// The facts of the other worktrees for `missing`, the anchors of
/// decisions ever accepted missing in the calling tree, whose facts
/// `here` are, and those git could not say enough of; none when nothing is
/// missing. Each worktree, in git's order, is read for the missing anchors
/// no earlier one holds whose files it changed since the fork, and no
/// further worktree is read once every one is held. Those files alone are
/// read, never a walk of the tree, against the calling tree's macros.
fn elsewhere(
    store: &Store,
    here: &CodeFacts,
    missing: &BTreeSet<&Anchor>,
    fill: &mut CacheFill,
) -> (Vec<Elsewhere>, Vec<UnreadWorktree>) {
    if missing.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let others = worktrees::others(store.code_root());
    let mut unread = others.unread;
    // clone: a set of references, narrowed as worktrees hold its anchors.
    let mut remaining = missing.clone();
    let mut read = Vec::new();
    for forked in others.trees {
        if remaining.is_empty() {
            break;
        }
        let paths: BTreeSet<&str> = remaining.iter().map(|anchor| anchor.file()).collect();
        let paths: Vec<&str> = paths.into_iter().collect();
        let changed = match forked.changed(&paths) {
            Ok(changed) if changed.is_empty() => continue,
            Ok(changed) => changed,
            Err(error) => {
                // clone: the warning names the worktree beside the listing.
                let path = forked.worktree.path.clone();
                unread.push(UnreadWorktree::of(Some(path), &error));
                continue;
            }
        };
        let facts = here.beside(&read_paths(store, &forked.worktree.path, changed, fill));
        let tree = Elsewhere {
            worktree: forked.worktree,
            facts,
        };
        remaining.retain(|anchor| !tree.holds(anchor));
        read.push(tree);
    }
    (read, unread)
}

/// What is at each of `paths` in the tree at `root`, `None` where nothing
/// is.
fn read_paths(
    store: &Store,
    root: &Path,
    paths: BTreeSet<String>,
    fill: &mut CacheFill,
) -> BTreeMap<String, Option<Entry>> {
    let anchored = paths.iter().map(String::as_str);
    let (mut found, _) = read_tree(store, root, Scope::Anchored, anchored, fill);
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
    /// Every source the walk takes, the anchored paths, and the manifests
    /// above an anchored Rust file the walk does not take.
    Tree,
    /// The anchored paths alone.
    Anchored,
}

/// What is in the tree at `root` by `scope`, by path, and which paths the
/// walk took. Its cache misses and the keys it read go to `fill`.
fn read_tree<'a>(
    store: &Store,
    root: &Path,
    scope: Scope,
    anchored: impl Iterator<Item = &'a str>,
    fill: &mut CacheFill,
) -> (BTreeMap<String, Entry>, BTreeSet<String>) {
    let walked: BTreeMap<String, FileKind> = match scope {
        Scope::Tree => walk::tree_sources(root).into_iter().collect(),
        Scope::Anchored => BTreeMap::new(),
    };
    let (mut entries, anchored) = probe_anchored(root, anchored, &walked, scope);
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
    for (path, kind, read) in read {
        let index = match read {
            None => continue,
            Some(Sourced::Fault(fault)) => Err(FileFault::Read(fault)),
            Some(Sourced::Indexed { key, indexed, miss }) => {
                fill.note(key, miss);
                indexed.map_err(FileFault::Index)
            }
        };
        entries.insert(path, Entry::File { kind, index });
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

/// What a call's reads give the cache: the entries it lacked, by key, so
/// two files of one content are one entry, and every key read, which
/// bounds the cache. One call fills it once, with the keys of every tree
/// it read, so a few paths of another worktree never prune the entries of
/// the calling tree.
#[derive(Debug, Default)]
struct CacheFill {
    misses: BTreeMap<SymbolsKey, Vec<u8>>,
    live: BTreeSet<SymbolsKey>,
}

impl CacheFill {
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
