//! The lexical passes of the C and C++ files a call reads, and the cache
//! reads that key their indexes. The calling tree's headers are read
//! first ([`read_headers`]): their `#define`s give the indexer the tree's
//! blank sets. Each pass is cached by its file's content, so an unchanged
//! file is never lexed again, and the misses of the call are read before
//! the store's, so a header lexed in the first step is not lexed again in
//! the second.

use std::collections::BTreeMap;
use std::path::Path;

use crate::code::{self, FileKind, Indexed, Lexed};
use crate::console::diag;
use crate::store::{Store, SymbolsKey};

use super::cache::{self, Content};
use super::{Reads, read_source};

/// What a call reads its cache entries from: the store, and the misses
/// it has found so far.
pub(super) struct Cached<'r> {
    pub(super) store: &'r Store,
    pub(super) misses: &'r BTreeMap<SymbolsKey, Vec<u8>>,
}

/// A file's lexical pass, its key, and its entry when the cache lacked
/// it.
pub(super) struct Found {
    pub(super) lexed: Lexed,
    pub(super) key: SymbolsKey,
    pub(super) miss: Option<Vec<u8>>,
}

impl Cached<'_> {
    /// The index of a file of `kind` of `source_bytes` bytes under `key`;
    /// `None` when no entry holds it.
    pub(super) fn index(
        &self,
        key: SymbolsKey,
        kind: FileKind,
        source_bytes: usize,
    ) -> Option<Indexed> {
        let entry = self
            .store
            .read_symbols(key, cache::max_entry_bytes(kind, source_bytes))?;
        cache::decode(key, &entry, kind, source_bytes)
    }

    /// The lexical pass of `bytes`, whose digest is `content`: the cache's,
    /// else lexed here.
    pub(super) fn lexed_or_lex(&self, content: Content, bytes: &[u8]) -> Found {
        let key = cache::lexed_key(content);
        let decode = |entry: &[u8]| cache::decode_lexical(key, entry, bytes.len());
        let cached = match self.misses.get(&key) {
            Some(entry) => decode(entry),
            None => self
                .store
                .read_symbols(key, cache::max_lexed_entry_bytes(bytes.len()))
                .and_then(|entry| decode(&entry)),
        };
        if let Some(lexed) = cached {
            return Found {
                lexed,
                key,
                miss: None,
            };
        }
        let lexed = code::lexed(bytes);
        let miss = Some(cache::encode_lexical(key, &lexed));
        Found { lexed, key, miss }
    }
}

/// Read the headers of the tree at `root` that `reads` walked, each in a
/// bounded read and within its kind's size, and give their `#define`s to
/// its indexer as the tree's blank sets. A header that cannot be read
/// gives none, as one that vanished; a system that starts no thread leaves
/// the sets empty, and the files' own reads then fail alike.
pub(super) fn read_headers(store: &Store, reads: &mut Reads, root: &Path) {
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
