//! The bytes of a cache entry: a file's index, keyed by its kind and
//! content together with the versions of the index's shape, of the span
//! hash and of the kind's grammar, so an entry is never read under rules
//! other than its own, and a key names the same index in every tree of a
//! shared graph. A C or C++ file's key holds the part of its tree's blank
//! set it writes too, so a `#define` blanks anew only the files that
//! write its name; and the file's lexical pass, which says what that part
//! is, is an entry of its own, keyed by its content.
//!
//! An entry is the BLAKE3 of the rest, then its key, then the index or the
//! lexical pass in `code`'s compact encoding. One whose sum, key or
//! encoding does not hold is a miss and is written again. The index of a
//! file is bounded by the file by construction (`code::max_entry_bytes`),
//! and a read takes no entry larger than that bound of the file it is
//! read for ([`max_entry_bytes`]): no entry is written that a read would
//! refuse, and a forged one costs a decode in proportion to the file.

use crate::code::{
    BOUNDS, FileKind, FileMacros, HASH_SCHEME, INDEX_FORMAT, Indexed, Lexed, decode_index,
    decode_lexed, encode_index, encode_lexed, grammar_identity_of, lexed_grammars,
    max_entry_bytes as max_index_bytes, max_lexed_bytes, name_of,
};
use crate::store::SymbolsKey;

/// The bytes before the index: its sum and its key.
const HEADER_BYTES: usize = 64;

/// What marks a lexical pass's key apart from every index's.
const LEXED: &str = "c-family-lexed";

/// The digest of a file's bytes, which each of its keys holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Content([u8; 32]);

impl Content {
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

/// The largest entry of a file of `kind` of `source_bytes` bytes.
pub(super) const fn max_entry_bytes(kind: FileKind, source_bytes: usize) -> usize {
    max_index_bytes(kind, source_bytes).saturating_add(HEADER_BYTES)
}

/// The largest lexical entry of a file of `source_bytes` bytes.
pub(super) const fn max_lexed_entry_bytes(source_bytes: usize) -> usize {
    max_lexed_bytes(source_bytes).saturating_add(HEADER_BYTES)
}

/// The key of the index of a file of `kind` whose bytes are `content`; a
/// C or C++ file's holds the identity of `macros`, the part of its tree's
/// blank set it writes.
pub(super) fn key(kind: FileKind, content: Content, macros: &FileMacros) -> SymbolsKey {
    let mut hasher = rules(name_of(kind));
    hasher.update(&grammar_identity_of(kind));
    if kind.blanks_macros() {
        hasher.update(macros.identity());
    }
    hasher.update(&content.0);
    SymbolsKey::new(*hasher.finalize().as_bytes())
}

/// The key of the lexical pass of a C or C++ file whose bytes are
/// `content`: the pass reads both grammars' words and no query.
pub(super) fn lexed_key(content: Content) -> SymbolsKey {
    let mut hasher = rules(LEXED);
    hasher.update(lexed_grammars());
    hasher.update(&content.0);
    SymbolsKey::new(*hasher.finalize().as_bytes())
}

/// A hasher over `what` and the versions and bounds of the index's rules.
fn rules(what: &str) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    for field in [what.as_bytes(), HASH_SCHEME.as_bytes()] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    hasher.update(&INDEX_FORMAT.to_le_bytes());
    for (name, bound) in BOUNDS {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bound as u64).to_le_bytes());
    }
    hasher
}

/// The bytes of the entry of `indexed` under `key`.
pub(super) fn encode(key: SymbolsKey, indexed: &Indexed) -> Vec<u8> {
    framed(key, encode_index(indexed))
}

/// The bytes of the entry of `lexed` under `key`.
pub(super) fn encode_lexical(key: SymbolsKey, lexed: &Lexed) -> Vec<u8> {
    framed(key, encode_lexed(lexed))
}

/// `body` under its sum and `key`.
fn framed(key: SymbolsKey, body: Vec<u8>) -> Vec<u8> {
    let mut rest = Vec::with_capacity(32 + body.len());
    rest.extend_from_slice(key.as_bytes());
    rest.extend(body);
    let mut bytes = blake3::hash(&rest).as_bytes().to_vec();
    bytes.extend(rest);
    bytes
}

/// The index `bytes` hold for `key`, the key of a file of `source_bytes`
/// bytes; `None` when they are no entry of it.
pub(super) fn decode(
    key: SymbolsKey,
    bytes: &[u8],
    kind: FileKind,
    source_bytes: usize,
) -> Option<Indexed> {
    decode_index(body(key, bytes)?, kind, source_bytes)
}

/// The lexical pass `bytes` hold for `key`, the key of a file of
/// `source_bytes` bytes; `None` when they are no entry of it.
pub(super) fn decode_lexical(key: SymbolsKey, bytes: &[u8], source_bytes: usize) -> Option<Lexed> {
    decode_lexed(body(key, bytes)?, source_bytes)
}

/// What the entry `bytes` holds under `key`; `None` when its sum or key
/// does not hold.
fn body(key: SymbolsKey, bytes: &[u8]) -> Option<&[u8]> {
    let (sum, rest) = bytes.split_at_checked(32)?;
    if sum != blake3::hash(rest).as_bytes() {
        return None;
    }
    let (stored, body) = rest.split_at_checked(32)?;
    (stored == key.as_bytes()).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::index;
    use crate::code::{BlankMacros, lexed};

    fn none() -> FileMacros {
        FileMacros::default()
    }

    fn rust_key(bytes: &[u8]) -> SymbolsKey {
        key(FileKind::Rust, Content::of(bytes), &none())
    }

    #[test]
    fn an_entry_reads_back_under_its_key_and_no_other() {
        let bytes = b"pub struct Table { rows: u32 }";
        let key = rust_key(bytes);
        let indexed = index(FileKind::Rust, bytes);
        let entry = encode(key, &indexed);

        assert_eq!(
            decode(key, &entry, FileKind::Rust, bytes.len()),
            Some(indexed)
        );
        assert!(entry.len() <= max_entry_bytes(FileKind::Rust, bytes.len()));
        assert_eq!(
            decode(rust_key(b"other"), &entry, FileKind::Rust, bytes.len()),
            None
        );
    }

    #[test]
    fn a_lexical_entry_reads_back_under_its_key_and_no_index_shares_it() {
        let bytes = b"#define API\nAPI int f(void);\n";
        let content = Content::of(bytes);
        let key = lexed_key(content);
        let lexed = lexed(bytes);
        let entry = encode_lexical(key, &lexed);

        assert_eq!(decode_lexical(key, &entry, bytes.len()), Some(lexed));
        assert!(entry.len() <= max_lexed_entry_bytes(bytes.len()));
        for kind in [FileKind::C, FileKind::Cpp] {
            assert_ne!(super::key(kind, content, &none()), key);
        }
    }

    #[test]
    fn a_key_holds_the_kind_and_the_bytes() {
        let content = Content::of(b"a = 1");

        assert_ne!(
            key(FileKind::Toml, content, &none()),
            key(FileKind::Json, content, &none())
        );
        assert_ne!(rust_key(b"a = 1"), rust_key(b"a = 2"));
    }

    #[test]
    fn the_part_of_the_trees_set_a_file_writes_keys_a_c_or_cpp_file_and_no_other() {
        let tree = BlankMacros::of([("api.h", &lexed(b"#define API\n").into_defines())]);
        let writes = Content::of(b"API int f();");
        let macros = tree.of_file("f.c", &lexed(b"API int f();"));

        for kind in [FileKind::C, FileKind::Cpp] {
            assert_ne!(key(kind, writes, &macros), key(kind, writes, &none()));
        }
        assert_eq!(
            key(FileKind::Rust, writes, &macros),
            key(FileKind::Rust, writes, &none())
        );
    }

    #[test]
    fn a_torn_or_altered_entry_is_a_miss() {
        let bytes = b"fn a() {}";
        let key = rust_key(bytes);
        let entry = encode(key, &index(FileKind::Rust, bytes));
        let mut altered = entry.clone();
        *altered.last_mut().unwrap() ^= 1;

        let len = bytes.len();
        assert_eq!(
            decode(key, &entry[..entry.len() - 1], FileKind::Rust, len),
            None
        );
        assert_eq!(decode(key, &altered, FileKind::Rust, len), None);
        assert_eq!(decode(key, b"", FileKind::Rust, len), None);
        assert_eq!(decode(key, &[0; 64], FileKind::Rust, len), None);
    }
}
