//! The lexical passes of the C and C++ files a call reads, and the cache
//! reads that key their indexes. The calling tree's headers are read
//! first ([`read_headers`]): their `#define`s give the indexer the tree's
//! blank set, and their digests key each header's entries, so a header
//! whose index the cache holds is read once. Each pass is cached by its
//! file's content, so an unchanged file is never lexed again, and the
//! misses of the call are read before the store's, so a header lexed in
//! the first step is not lexed again in the second.

use std::collections::BTreeMap;
use std::path::Path;

use crate::code::{self, FileKind, Indexed, Lexed};
use crate::console::diag;
use crate::store::{Store, SymbolsKey};

use super::cache::{self, Content};
use super::{Reads, read_source};

/// A header of the calling tree as the first step read it: its bytes'
/// digest and length, which key its entries without a second read.
#[derive(Debug, Clone, Copy)]
pub(super) struct Header {
    pub(super) content: Content,
    pub(super) bytes: usize,
}

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

    /// The lexical pass of a file of `source_bytes` bytes whose digest is
    /// `content`; `None` when no entry holds it.
    pub(super) fn lexed(&self, content: Content, source_bytes: usize) -> Option<Lexed> {
        let key = cache::lexed_key(content);
        match self.misses.get(&key) {
            Some(entry) => cache::decode_lexical(key, entry, source_bytes),
            None => {
                let max = cache::max_lexed_entry_bytes(source_bytes);
                let entry = self.store.read_symbols(key, max)?;
                cache::decode_lexical(key, &entry, source_bytes)
            }
        }
    }

    /// The lexical pass of `bytes`, whose digest is `content`: the cache's,
    /// else lexed here.
    pub(super) fn lexed_or_lex(&self, content: Content, bytes: &[u8]) -> Found {
        let key = cache::lexed_key(content);
        if let Some(lexed) = self.lexed(content, bytes.len()) {
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

/// The headers of `walked`, the tree at `root`, each read in a bounded
/// read and within its kind's size, by path; their `#define`s are the
/// indexer's tree set. A header that cannot be read gives none, as one
/// that vanished; a system that starts no thread leaves the set empty,
/// and the files' own reads then fail alike.
pub(super) fn read_headers(
    store: &Store,
    reads: &mut Reads,
    root: &Path,
    walked: &BTreeMap<String, FileKind>,
) -> BTreeMap<String, Header> {
    let headers: Vec<(&String, FileKind)> = walked
        .iter()
        .filter(|(path, kind)| kind.blanks_macros() && code::is_header(path))
        .map(|(path, kind)| (path, *kind))
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
        let content = Content::of(&bytes);
        let header = Header {
            content,
            bytes: bytes.len(),
        };
        Some((header, cached.lexed_or_lex(content, &bytes)))
    });
    let Ok(read) = read else {
        diag!("warning: the C and C++ macros to blank were not read: no thread started");
        return BTreeMap::new();
    };
    let mut found = BTreeMap::new();
    let mut defines = Vec::with_capacity(read.len());
    for ((path, _), read) in headers.into_iter().zip(read) {
        let Some((header, lexed)) = read else {
            continue;
        };
        reads.note(lexed.key, lexed.miss);
        defines.push(lexed.lexed.into_defines());
        // clone: the map outlives the walk it was read from.
        found.insert(path.clone(), header);
    }
    reads.indexer.take_headers(&defines);
    found
}
