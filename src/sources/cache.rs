//! The bytes of a cache entry: a file's index, keyed by its kind and
//! content together with the versions of the index's shape and of the
//! span hash, so an entry is never read under rules other than its own,
//! and a key names the same index in every tree of a shared graph.
//!
//! An entry is a line with the BLAKE3 of the rest, then the JSON of its
//! key and the index. One whose sum, key or JSON does not hold is a miss
//! and is written again, as is one the store refuses to read.

use serde::{Deserialize, Serialize};

use crate::code::{FileIndex, FileKind, HASH_SCHEME, INDEX_FORMAT, IndexFault};
use crate::store::SymbolsKey;

/// What indexing a file's bytes gave.
pub(super) type Indexed = Result<FileIndex, IndexFault>;

/// The key of the index of `bytes`, a file of `kind`.
pub(super) fn key(kind: FileKind, bytes: &[u8]) -> SymbolsKey {
    let mut hasher = blake3::Hasher::new();
    for field in [kind.name().as_bytes(), HASH_SCHEME.as_bytes()] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hasher.update(&INDEX_FORMAT.to_le_bytes());
    hasher.update(bytes);
    SymbolsKey::new(*hasher.finalize().as_bytes())
}

#[derive(Serialize)]
struct EntryOut<'a> {
    key: String,
    index: &'a Indexed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryIn {
    key: String,
    index: Indexed,
}

/// The bytes of the entry of `indexed` under `key`.
pub(super) fn encode(key: SymbolsKey, indexed: &Indexed) -> Option<Vec<u8>> {
    let entry = EntryOut {
        key: key.to_string(),
        index: indexed,
    };
    let payload = serde_json::to_vec(&entry).ok()?;
    let mut bytes = blake3::hash(&payload).to_hex().as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend(payload);
    Some(bytes)
}

/// The index `bytes` hold for `key`; `None` when they are no entry of it.
pub(super) fn decode(key: SymbolsKey, bytes: &[u8]) -> Option<Indexed> {
    let split = bytes.iter().position(|&b| b == b'\n')?;
    let (sum, payload) = (bytes.get(..split)?, bytes.get(split + 1..)?);
    if sum != blake3::hash(payload).to_hex().as_bytes() {
        return None;
    }
    let entry: EntryIn = serde_json::from_slice(payload).ok()?;
    (entry.key == key.to_string()).then_some(entry.index)
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
        let entry = encode(key, &indexed).unwrap();

        assert_eq!(decode(key, &entry), Some(indexed));
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
        let entry = encode(key, &index(FileKind::Rust, bytes)).unwrap();
        let mut altered = entry.clone();
        *altered.last_mut().unwrap() ^= 1;

        assert_eq!(decode(key, &entry[..entry.len() - 1]), None);
        assert_eq!(decode(key, &altered), None);
        assert_eq!(decode(key, b""), None);
        assert_eq!(decode(key, b"\n{}"), None);
    }
}
