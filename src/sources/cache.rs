//! The bytes of a cache entry: a file's index, keyed by its kind and
//! content together with the versions of the index's shape, of the span
//! hash and of the kind's grammar, so an entry is never read under rules
//! other than its own, and a key names the same index in every tree of a
//! shared graph.
//!
//! An entry is the BLAKE3 of the rest, then its key, then the index in
//! `code`'s compact encoding. One whose sum, key or encoding does not hold
//! is a miss and is written again. The index of a file is bounded by the
//! file by construction (`code::max_entry_bytes`), and a read takes no
//! entry larger than that bound of the file it is read for
//! ([`max_entry_bytes`]): no entry is written that a read would refuse,
//! and a forged one costs a decode in proportion to the file.

use crate::code::{
    FileKind, HASH_SCHEME, INDEX_FORMAT, Indexed, decode_index, encode_index,
    max_entry_bytes as max_index_bytes,
};
use crate::store::SymbolsKey;

/// The bytes before the index: its sum and its key.
const HEADER_BYTES: usize = 64;

/// The largest entry of a file of `source_bytes` bytes.
pub(super) const fn max_entry_bytes(source_bytes: usize) -> usize {
    max_index_bytes(source_bytes).saturating_add(HEADER_BYTES)
}

/// The key of the index of `bytes`, a file of `kind`.
pub(super) fn key(kind: FileKind, bytes: &[u8]) -> SymbolsKey {
    let mut hasher = blake3::Hasher::new();
    for field in [kind.name().as_bytes(), HASH_SCHEME.as_bytes()] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hasher.update(&INDEX_FORMAT.to_le_bytes());
    hasher.update(&kind.grammar());
    hasher.update(bytes);
    SymbolsKey::new(*hasher.finalize().as_bytes())
}

/// The bytes of the entry of `indexed` under `key`.
pub(super) fn encode(key: SymbolsKey, indexed: &Indexed) -> Vec<u8> {
    let index = encode_index(indexed);
    let mut rest = Vec::with_capacity(32 + index.len());
    rest.extend_from_slice(key.as_bytes());
    rest.extend(index);
    let mut bytes = blake3::hash(&rest).as_bytes().to_vec();
    bytes.extend(rest);
    bytes
}

/// The index `bytes` hold for `key`; `None` when they are no entry of it.
pub(super) fn decode(key: SymbolsKey, bytes: &[u8]) -> Option<Indexed> {
    let (sum, rest) = bytes.split_at_checked(32)?;
    if sum != blake3::hash(rest).as_bytes() {
        return None;
    }
    let (stored, index) = rest.split_at_checked(32)?;
    if stored != key.as_bytes() {
        return None;
    }
    decode_index(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index;

    #[test]
    fn an_entry_reads_back_under_its_key_and_no_other() {
        let bytes = b"pub struct Table { rows: u32 }";
        let key = key(FileKind::Rust, bytes);
        let indexed = index(FileKind::Rust, bytes);
        let entry = encode(key, &indexed);

        assert_eq!(decode(key, &entry), Some(indexed));
        assert!(entry.len() <= max_entry_bytes(bytes.len()));
        assert_eq!(decode(super::key(FileKind::Rust, b"other"), &entry), None);
    }

    #[test]
    fn a_key_holds_the_kind_and_the_bytes() {
        assert_ne!(key(FileKind::Toml, b"a = 1"), key(FileKind::Json, b"a = 1"));
        assert_ne!(key(FileKind::Toml, b"a = 1"), key(FileKind::Toml, b"a = 2"));
    }

    #[test]
    fn a_torn_or_altered_entry_is_a_miss() {
        let bytes = b"fn a() {}";
        let key = key(FileKind::Rust, bytes);
        let entry = encode(key, &index(FileKind::Rust, bytes));
        let mut altered = entry.clone();
        *altered.last_mut().unwrap() ^= 1;

        assert_eq!(decode(key, &entry[..entry.len() - 1]), None);
        assert_eq!(decode(key, &altered), None);
        assert_eq!(decode(key, b""), None);
        assert_eq!(decode(key, &[0; 64]), None);
    }
}
