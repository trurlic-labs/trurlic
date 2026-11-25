//! The bytes of a file's index in the cache, and back: a compact binary
//! form, whose size is bounded by the file it indexes.
//!
//! Numbers are LEB128 varints, texts a length and UTF-8 bytes, a hash its
//! 32 bytes. A definition whose span is its predecessor's (the targets of
//! one Makefile rule) keeps one byte for it. An entry is at most
//! [`MAX_ENTRY_BYTES_PER_SOURCE_BYTE`] times its file and
//! [`ENTRY_OVERHEAD_BYTES`] besides by construction. Every resolver adds
//! what an entry holds through the scope tree, which charges each part
//! its bytes here ([`room_of`], [`Measure`]) and refuses the file past its
//! room. A read takes nothing on trust. Every count is bounded by the
//! bytes left and by the file the entry is read for. Every id is bounded
//! by what it names, every parent by its child (so a path's walk ends),
//! and a path by [`MAX_NESTING`](super::scope::MAX_NESTING). Bytes that
//! break any of it are no index.

mod read;
mod write;

use super::index::{FileIndex, IndexFault};
use super::kinds::{DefinitionKind, Role, Visibility};

pub(crate) use read::decode;
pub(in crate::code) use write::Measure;
pub(crate) use write::encode;

/// The bytes an entry may take per byte of its file: a definition of two
/// bytes (an enum variant `A,`) costs its scope, its span and its 32-byte
/// hash.
pub(super) const MAX_ENTRY_BYTES_PER_SOURCE_BYTE: usize = 32;

/// The bytes an entry may take past [`MAX_ENTRY_BYTES_PER_SOURCE_BYTE`]
/// times its file: the counts of an empty index, and room besides.
pub(super) const ENTRY_OVERHEAD_BYTES: usize = 64;

/// The bytes of an index's outcome tag and its five counts, each a count
/// of at most `u32::MAX` items.
const COUNTS_BYTES: usize = 1 + 5 * 5;

/// The largest entry of a file of `bytes` bytes.
pub(crate) const fn max_entry_bytes(bytes: usize) -> usize {
    bytes
        .saturating_mul(MAX_ENTRY_BYTES_PER_SOURCE_BYTE)
        .saturating_add(ENTRY_OVERHEAD_BYTES)
}

/// The bytes the scopes, definitions, macros, invocations and module
/// files of the index of a file of `bytes` bytes may take: an entry of
/// them, its counts added, is within [`max_entry_bytes`].
pub(super) const fn room_of(bytes: usize) -> usize {
    max_entry_bytes(bytes) - COUNTS_BYTES
}

/// What indexing a file's bytes gave.
pub(crate) type Indexed = Result<FileIndex, IndexFault>;

pub(super) const fn kind_code(kind: DefinitionKind) -> u8 {
    match kind {
        DefinitionKind::Module => 0,
        DefinitionKind::Struct => 1,
        DefinitionKind::Union => 2,
        DefinitionKind::Enum => 3,
        DefinitionKind::Trait => 4,
        DefinitionKind::Impl => 5,
        DefinitionKind::Function => 6,
        DefinitionKind::Method => 7,
        DefinitionKind::Field => 8,
        DefinitionKind::Variant => 9,
        DefinitionKind::Constant => 10,
        DefinitionKind::Static => 11,
        DefinitionKind::TypeAlias => 12,
        DefinitionKind::AssociatedType => 13,
        DefinitionKind::Macro => 14,
        DefinitionKind::Reexport => 15,
        DefinitionKind::Key => 16,
        DefinitionKind::Target => 17,
        DefinitionKind::Heading => 18,
        DefinitionKind::Class => 19,
        DefinitionKind::Variable => 20,
    }
}

/// A visibility's code; 0 is none.
pub(super) const fn visibility_code(visibility: Option<Visibility>) -> u8 {
    match visibility {
        None => 0,
        Some(Visibility::Private) => 1,
        Some(Visibility::Crate) => 2,
        Some(Visibility::Super) => 3,
        Some(Visibility::InPath) => 4,
        Some(Visibility::Public) => 5,
    }
}

pub(super) const fn role_code(role: Role) -> u8 {
    match role {
        Role::Code => 0,
        Role::Test => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::code::index::{FileKind, index};

    #[test]
    fn every_kind_is_coded_by_its_place() {
        for (at, kind) in DefinitionKind::ALL.iter().enumerate() {
            assert_eq!(usize::from(kind_code(*kind)), at, "{kind:?}");
        }
        for (at, visibility) in Visibility::ALL.iter().enumerate() {
            assert_eq!(usize::from(visibility_code(Some(*visibility))), at + 1);
        }
    }

    #[test]
    fn an_index_reads_back_from_its_bytes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (file, kind) in [
            (
                "tests/fixtures/code/crates/ledger/src/lib.rs",
                FileKind::Rust,
            ),
            (
                "tests/fixtures/code/crates/ledger/src/books/mod.rs",
                FileKind::Rust,
            ),
            ("tests/fixtures/code/python/shelf.py", FileKind::Python),
            ("tests/fixtures/code/settings.json", FileKind::Json),
            ("tests/fixtures/code/Makefile", FileKind::Make),
            ("tests/fixtures/code/docs/guide.md", FileKind::Markdown),
            ("Cargo.toml", FileKind::Toml),
        ] {
            let indexed = index(kind, &std::fs::read(root.join(file)).unwrap());
            let bytes = std::fs::read(root.join(file)).unwrap();
            assert_eq!(
                decode(&encode(&indexed), bytes.len()),
                Some(indexed),
                "{file}"
            );
        }
        for fault in [
            IndexFault::NotUtf8 { at: 7 },
            IndexFault::Syntax { at: 300 },
            IndexFault::TooComplex,
            IndexFault::TooDeep,
            IndexFault::Grammar,
            IndexFault::TooManyIndentations { max: 256 },
            IndexFault::TooManyContinuations { max: 64 },
            IndexFault::TooLarge {
                bytes: 5_000_000,
                max: 1 << 20,
            },
        ] {
            assert_eq!(decode(&encode(&Err(fault)), 0), Some(Err(fault)));
        }
    }

    #[test]
    fn bytes_that_break_the_form_are_no_index() {
        let indexed = index(FileKind::Rust, b"mod a { fn b() {} }").unwrap();
        let bytes = encode(&Ok(indexed));

        let source = 19;
        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut], source), None, "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(decode(&longer, source), None);
        // A scope whose parent is itself, one of no segment, and a count
        // past the bytes.
        assert_eq!(decode(&[0, 1, 1, 0, 0, 0, 0], source), None);
        assert_eq!(decode(&[0, 1, 0, 0, 0, 0, 0, 0], source), None);
        assert_eq!(decode(&[0, 0x80, 0x80, 0x80, 0x80, 0x01], source), None);
        assert_eq!(decode(&[9], source), None);
    }

    #[test]
    fn an_entry_holding_more_items_than_its_file_has_bytes_is_no_index() {
        // Scopes of one empty segment each: four bytes apiece, which a
        // read cap of 32 bytes per source byte would let in by the million.
        let scopes = 200;
        let mut forged = vec![0, 0x80 | (scopes & 0x7f) as u8, (scopes >> 7) as u8];
        for _ in 0..scopes {
            forged.extend([0, 1, 0, 0]);
        }
        forged.extend([0, 0, 0, 0]);

        // Each scope and its segment is an item: 400 of them.
        assert!(matches!(decode(&forged, 400), Some(Ok(_))));
        assert_eq!(decode(&forged, 300), None);
    }

    /// The bytes [`Measure`] charges for what `index` holds, its counts
    /// added: what the scope tree spent of the room.
    fn measured(index: &FileIndex) -> usize {
        COUNTS_BYTES
            + index
                .scopes
                .iter()
                .map(|scope| Measure::scope(scope.parent, &scope.name))
                .sum::<usize>()
            + index
                .definitions
                .iter()
                .map(Measure::definition)
                .sum::<usize>()
            + index
                .rust
                .macros
                .iter()
                .map(Measure::type_macro)
                .sum::<usize>()
            + index
                .rust
                .invocations
                .iter()
                .map(Measure::invocation)
                .sum::<usize>()
            + index
                .rust
                .module_files
                .iter()
                .map(Measure::module_file)
                .sum::<usize>()
    }

    #[test]
    fn an_entry_takes_no_more_than_its_parts_were_charged() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut pending = vec![root.join("tests/fixtures/code"), root.join("src/code")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path.to_str().unwrap().trim_end_matches(".fixture");
                let Some(kind) = FileKind::of(name) else {
                    continue;
                };
                let bytes = std::fs::read(&path).unwrap();
                let Ok(indexed) = index(kind, &bytes) else {
                    continue;
                };
                let entry = encode(&Ok(indexed.clone())).len();
                assert!(entry <= measured(&indexed), "{name}");
                assert!(entry <= max_entry_bytes(bytes.len()), "{name}");
            }
        }
    }

    #[test]
    fn an_entry_is_bounded_by_its_file_whatever_it_holds() {
        let variants = format!("enum E {{ {} }}", "A,".repeat(50_000));
        let fields = format!("struct S {{ {} }}", "a:u8,".repeat(20_000));
        let targets = format!("{}: x\n", "t ".repeat(50_000));
        let headings = "# a\n".repeat(25_000);
        let keys = format!("{{{}\"z\":0}}", "\"a\":0,".repeat(20_000));
        let tables: String = (0..10_000).map(|n| format!("[a{n}]\nb=0\n")).collect();
        let nested = format!("{}{}", "mod m{".repeat(60), "}".repeat(60));
        let names = format!("{}z = 0\n", "a,".repeat(50_000));
        let lines = "x=1\n".repeat(50_000);
        let reexported = format!(
            "pub use {}::{{{}}};",
            "a".repeat(100_000),
            "self,".repeat(20_000)
        );
        for (kind, text) in [
            (FileKind::Rust, variants.as_str()),
            (FileKind::Rust, fields.as_str()),
            (FileKind::Rust, nested.as_str()),
            (FileKind::Rust, reexported.as_str()),
            (FileKind::Make, targets.as_str()),
            (FileKind::Markdown, headings.as_str()),
            (FileKind::Json, keys.as_str()),
            (FileKind::Toml, tables.as_str()),
            (FileKind::Python, names.as_str()),
            (FileKind::Python, lines.as_str()),
            (FileKind::Rust, ""),
        ] {
            let bytes = encode(&index(kind, text.as_bytes()));
            let bound = max_entry_bytes(text.len());
            assert!(
                bytes.len() <= bound,
                "{kind:?} of {} bytes: {} bytes, over {bound}",
                text.len(),
                bytes.len()
            );
        }
    }
}
