//! Files of a revision of the repository, read from git in one bounded
//! `cat-file --batch` and indexed by the shell's one indexer: what the
//! anchor findings and their repairs read of a tree other than a work
//! tree.

use std::collections::BTreeSet;
use std::path::Path;

use crate::code::{Entry, FileFault, FileKind};
use crate::git;

use super::MAX_SOURCE_FILE_BYTES;
use super::indexer::Indexer;

/// Files of the repository's history, read from git and indexed by the
/// shell's one indexer.
pub(super) struct Blobs<'r> {
    /// The directory git runs in, which the paths are relative to.
    pub(super) root: &'r Path,
    pub(super) indexer: &'r Indexer,
}

impl Blobs<'_> {
    /// What git's history held of each of `files`, a revision and a path,
    /// each read in a bounded read and indexed; a file git holds no blob of
    /// there, or any failure of git, leaves it out, and a file the indexer
    /// cannot take is an index fault.
    pub(super) fn read(&self, files: &BTreeSet<(String, String)>) -> Vec<(String, String, Entry)> {
        if files.is_empty() {
            return Vec::new();
        }
        // clone: git is handed the files as a list.
        let asked: Vec<(String, String)> = files.iter().cloned().collect();
        let Ok(blobs) = git::renames::blobs(self.root, &asked, MAX_SOURCE_FILE_BYTES) else {
            return Vec::new();
        };
        let blobs: Vec<(FileKind, String, String, Vec<u8>)> = blobs
            .into_iter()
            .filter_map(|((revision, path), bytes)| {
                Some((FileKind::of(&path)?, revision, path, bytes))
            })
            .collect();
        let indexed = self.indexer.map(&blobs, |worker, (kind, _, _, bytes)| {
            match worker.index(*kind, bytes) {
                Ok(indexed) => indexed.map_err(FileFault::Index),
                Err(fault) => Err(FileFault::Read(fault)),
            }
        });
        let indexed = indexed
            .unwrap_or_else(|fault| blobs.iter().map(|_| Err(FileFault::Read(fault))).collect());
        blobs
            .into_iter()
            .zip(indexed)
            .map(|((kind, revision, path, _), index)| (revision, path, Entry::File { kind, index }))
            .collect()
    }
}
