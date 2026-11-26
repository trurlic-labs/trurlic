//! Files of the repository's history, read from git in one bounded
//! `cat-file --batch` and indexed as the tree's files are, through the
//! index's cache by content: what the anchor findings and their repairs
//! read of a tree other than a work tree.

use std::collections::BTreeSet;
use std::path::Path;

use crate::code::{Entry, FileKind};
use crate::git;
use crate::store::Store;

use super::lexical::Cached;
use super::{MAX_SOURCE_FILE_BYTES, Reads, Sourced, index_bytes};

/// Files of the repository's history, read from git.
pub(super) struct Blobs<'r> {
    /// The directory git runs in, which the paths are relative to.
    pub(super) root: &'r Path,
    pub(super) store: &'r Store,
}

impl Blobs<'_> {
    /// What git's history held of each of `files`, a name git reads a blob
    /// by and the path it held, each read in a bounded read and indexed,
    /// the cache entries used noted in `reads`; a blob git names none for,
    /// or any failure of git, leaves its files out, and a file the indexer
    /// cannot take is an index fault.
    pub(super) fn read(
        &self,
        reads: &mut Reads,
        files: &BTreeSet<(String, String)>,
    ) -> Vec<(String, String, Entry)> {
        if files.is_empty() {
            return Vec::new();
        }
        let names: BTreeSet<&String> = files.iter().map(|(name, _)| name).collect();
        // clone: git is handed the names as a list.
        let names: Vec<String> = names.into_iter().cloned().collect();
        let Ok(blobs) = git::renames::blobs(self.root, &names, MAX_SOURCE_FILE_BYTES) else {
            return Vec::new();
        };
        let found: Vec<(FileKind, &String, &String, &[u8])> = files
            .iter()
            .filter_map(|(name, path)| Some((FileKind::of(path)?, name, path, blobs.get(name)?)))
            .collect();
        let cached = Cached {
            store: self.store,
            misses: &reads.misses,
        };
        let indexed = reads.indexer.map(&found, |worker, (kind, _, path, bytes)| {
            index_bytes(&cached, worker, (path, *kind), bytes)
        });
        let indexed =
            indexed.unwrap_or_else(|fault| found.iter().map(|_| Sourced::Fault(fault)).collect());
        found
            .into_iter()
            .zip(indexed)
            .filter_map(|((kind, name, path, _), sourced)| {
                let entry = reads.take(kind, Some(sourced))?;
                // clone: the entry is keyed by the name and path it was asked by.
                Some((name.clone(), path.clone(), entry))
            })
            .collect()
    }
}
