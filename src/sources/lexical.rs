//! The cache reads of a call: an index by its key, and the lexical pass of
//! a C or C++ file. Each pass is cached by its file's content, so an
//! unchanged file is never lexed again, and the misses of the call are
//! read before the store's, so a header lexed in the first step is not
//! lexed again in the second.

use std::collections::BTreeMap;

use crate::code::{self, FileKind, Indexed, Lexed};
use crate::store::{Store, SymbolsKey};

use super::cache::{self, Content};

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
