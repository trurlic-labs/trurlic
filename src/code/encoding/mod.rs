//! The bytes of a file's index in the cache, and back: a compact binary
//! form, whose size is bounded by the file it indexes.
//!
//! Numbers are LEB128 varints, texts a length and UTF-8 bytes, a hash its
//! 32 bytes. A definition whose span is its predecessor's (the targets of
//! one Makefile rule) keeps one byte for it.
//!
//! Every resolver adds what an entry holds through the scope tree, which
//! charges each part its cost here ([`room_of`], [`Measure`]) and refuses
//! the file past its room. A part's cost is its bytes, and the [`weight`]
//! of each value it holds: at least the memory the value takes once read.
//! An entry's cost is thus at most [`MAX_ENTRY_BYTES_PER_SOURCE_BYTE`]
//! times its file, and never past [`MAX_ENTRY_BYTES`], with
//! [`ENTRY_OVERHEAD_BYTES`] besides.
//!
//! A read takes nothing on trust. It charges an entry as the tree did and
//! refuses one past the room of the file it is read for, before it
//! allocates. So every entry a write makes reads back, and an entry and
//! the index it decodes to together take no more memory than that room.
//! Every count is bounded by the bytes left, every id by what it names,
//! every parent by its child (so a path's walk ends), and a path by
//! [`MAX_NESTING`](super::scope::MAX_NESTING). Bytes that break any of it
//! are no index.

mod read;
mod write;

use super::index::{FileIndex, FileKind, IndexFault};
use super::kinds::{DefinitionKind, Role, Visibility};

pub(crate) use read::decode;
pub(in crate::code) use write::Measure;
pub(crate) use write::encode;

/// The cost an entry may take per byte of its file: a definition of two
/// bytes (a Makefile target `t `) costs its scope, its name, its span, its
/// 32-byte hash and the weight of each.
pub(super) const MAX_ENTRY_BYTES_PER_SOURCE_BYTE: usize = 64;

/// The cost no entry passes, whatever its file: the room of the largest
/// tree-sitter file, so a lexical file of more than a megabyte has less
/// per byte. It bounds the memory any read of the cache takes for one
/// entry.
pub(super) const MAX_ENTRY_BYTES: usize = 64 << 20;

/// The bytes an entry may take past its room per byte: the counts of an
/// empty index, and room besides.
pub(super) const ENTRY_OVERHEAD_BYTES: usize = 64;

/// What an entry is charged for each value it holds, past its bytes: the
/// memory the value takes once read, its slot in its collection and what
/// it owns but its texts, whose bytes are in the entry's
/// (`tests::each_weight_holds_its_value`).
pub(super) mod weight {
    pub(in crate::code) const SCOPE: usize = 32;
    pub(in crate::code) const SEGMENT: usize = 48;
    pub(in crate::code) const DEFINITION: usize = 72;
    pub(in crate::code) const INVOCATION: usize = 128;
    pub(in crate::code) const MODULE_FILE: usize = 32;
    /// An element of an opening or of arguments, or a group's token.
    pub(in crate::code) const ELEMENT: usize = 32;
    /// A macro, a rule, a binding's place and what it receives, an
    /// `impl`, a trait's token, a hand-off, or a name a macro gives with
    /// the segment it shares.
    pub(in crate::code) const MACRO_PART: usize = 104;
}

/// The bytes of an index's outcome tag and its five counts, each a count
/// of at most `u32::MAX` items.
const COUNTS_BYTES: usize = 1 + 5 * 5;

/// The largest cost of an entry of a file of `bytes` bytes within its
/// kind's size.
const fn room_cost(bytes: usize) -> usize {
    let room = bytes.saturating_mul(MAX_ENTRY_BYTES_PER_SOURCE_BYTE);
    let capped = match room < MAX_ENTRY_BYTES {
        true => room,
        false => MAX_ENTRY_BYTES,
    };
    capped + ENTRY_OVERHEAD_BYTES
}

/// The largest cost of an entry of a file of `kind` of `bytes` bytes, its
/// bytes and weights together: past the kind's size, a fault's alone.
pub(crate) const fn max_entry_bytes(kind: FileKind, bytes: usize) -> usize {
    match bytes > kind.max_bytes() {
        true => ENTRY_OVERHEAD_BYTES,
        false => room_cost(bytes),
    }
}

/// What the scopes, definitions, macros, invocations and module files of
/// the index of a file of `bytes` bytes, within its kind's size, may
/// cost: an entry of them, its counts added, is within its room.
pub(super) const fn room_of(bytes: usize) -> usize {
    room_cost(bytes) - COUNTS_BYTES
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
        DefinitionKind::Interface => 21,
        DefinitionKind::Test => 22,
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
        Some(Visibility::Protected) => 6,
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

    /// Every file of the fixture corpus and of `src/code`, with its kind
    /// and bytes.
    fn corpus() -> Vec<(String, FileKind, Vec<u8>)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        let mut pending = vec![root.join("tests/fixtures/code"), root.join("src/code")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path
                    .to_str()
                    .unwrap()
                    .trim_end_matches(".fixture")
                    .to_owned();
                if let Some(kind) = FileKind::of(&name) {
                    found.push((name, kind, std::fs::read(&path).unwrap()));
                }
            }
        }
        found.sort();
        found
    }

    /// Files of each kind drawn from a seeded source: runs of the pieces
    /// of `PIECES`, many alike, as dense in items as a file gets.
    fn generated() -> Vec<(String, FileKind, Vec<u8>)> {
        const PIECES: [(FileKind, &[&str]); 6] = [
            (
                FileKind::Rust,
                &[
                    "mod a {",
                    "}",
                    "enum E { A, A, ",
                    "struct S { a: u8, ",
                    "fn b() {} ",
                    "macro_rules! m { ($a:ident, $b:ident) => { struct $a; h!($a, $b); g!($b, $a); impl From<$b> for $a {} }; } ",
                    "m!(X, Y); ",
                    "impl T { fn f() {} } ",
                    "pub use a::{self, self, ",
                    "a, ",
                    "(",
                    ")",
                    "\"",
                    "m! { struct Q; enum R { S } } ",
                ],
            ),
            (
                FileKind::Python,
                &[
                    "a,",
                    "z = 0\n",
                    "class C:\n    ",
                    "def f(): pass\n",
                    "    ",
                    "x=1\n",
                ],
            ),
            (
                FileKind::Toml,
                &["[a]\n", "b=0\n", "[[c]]\n", "d.e=1\n", "f={g=1}\n"],
            ),
            (FileKind::Json, &["{", "}", "\"a\":0,", "[", "]", "\"z\":{"]),
            (FileKind::Make, &["t ", ": x\n", "a b c: d\n", "\techo\n"]),
            (
                FileKind::Markdown,
                &["# a\n", "## b\n", "text\n", "### c\n"],
            ),
        ];
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut draw = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % bound as u64).unwrap()
        };
        let mut found = Vec::new();
        for (kind, pieces) in PIECES {
            for file in 0..40 {
                let mut text = String::new();
                for _ in 0..draw(4_000) {
                    text.push_str(pieces[draw(pieces.len())]);
                }
                found.push((
                    format!("generated {kind:?} {file}"),
                    kind,
                    text.into_bytes(),
                ));
            }
        }
        found
    }

    #[test]
    fn every_entry_a_write_makes_reads_back() {
        let hand_offs = "macro_rules! m { ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => { first!($a, $b, $c, $d, $e, $f, $g, $h); second!($a, $b, $c, $d, $e, $f, $g, $h); }; }";
        let variants = format!("enum E {{ {} }}", "A, ".repeat(5_000));
        let names = format!("{}z = 0\n", "a,".repeat(50_000));
        let cases = [
            (
                "hand-offs".to_owned(),
                FileKind::Rust,
                hand_offs.as_bytes().to_vec(),
            ),
            ("variants".to_owned(), FileKind::Rust, variants.into_bytes()),
            ("names".to_owned(), FileKind::Python, names.into_bytes()),
        ];
        for (name, kind, bytes) in cases.into_iter().chain(corpus()).chain(generated()) {
            let indexed = index(kind, &bytes);
            assert_eq!(
                decode(&encode(&indexed), kind, bytes.len()),
                Some(indexed),
                "{name}"
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
            assert_eq!(
                decode(&encode(&Err(fault)), FileKind::Rust, 0),
                Some(Err(fault))
            );
        }
    }

    #[test]
    fn bytes_that_break_the_form_are_no_index() {
        let indexed = index(FileKind::Rust, b"mod a { fn b() {} }").unwrap();
        let bytes = encode(&Ok(indexed));

        let source = 19;
        let decoded = |bytes: &[u8]| decode(bytes, FileKind::Rust, source);
        for cut in 0..bytes.len() {
            assert_eq!(decoded(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(decoded(&longer), None);
        // A scope whose parent is itself, one of no segment, and a count
        // past the bytes.
        assert_eq!(decoded(&[0, 1, 1, 0, 0, 0, 0]), None);
        assert_eq!(decoded(&[0, 1, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(decoded(&[0, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
        assert_eq!(decoded(&[9]), None);
    }

    /// An entry of `scopes` scopes of one empty name each, and nothing
    /// else: four bytes apiece, and the weights of a scope and a segment.
    fn forged(scopes: usize) -> Vec<u8> {
        let mut forged = vec![0];
        let mut count = scopes;
        while count >= 0x80 {
            forged.push(0x80 | (count & 0x7f) as u8);
            count >>= 7;
        }
        forged.push(count as u8);
        for _ in 0..scopes {
            forged.extend([0, 1, 0, 0]);
        }
        forged.extend([0, 0, 0, 0]);
        forged
    }

    #[test]
    fn an_entry_past_the_room_of_the_file_it_is_read_for_is_no_index() {
        // 807 bytes and 200 times 80 of weight: 16,807, within the 16,832
        // of a file of 262 bytes and past the 16,768 of one of 261.
        let entry = forged(200);
        assert_eq!(entry.len(), 807);

        assert!(matches!(decode(&entry, FileKind::Rust, 262), Some(Ok(_))));
        assert_eq!(decode(&entry, FileKind::Rust, 261), None);
    }

    #[test]
    fn no_entry_is_read_past_64_mib_whatever_its_file() {
        // 798,915 scopes cost 67,108,868 of the 67,108,928 a lexical file
        // of 4 MiB has, which 64 times its bytes would far pass.
        let source = 4 << 20;

        assert!(matches!(
            decode(&forged(798_915), FileKind::Toml, source),
            Some(Ok(_))
        ));
        assert_eq!(decode(&forged(798_916), FileKind::Toml, source), None);
    }

    #[test]
    fn a_file_past_its_kinds_size_reads_back_its_fault_alone() {
        let size = FileKind::Rust.max_bytes();
        let fault = Err(IndexFault::TooLarge {
            bytes: size + 1,
            max: size,
        });

        assert_eq!(
            decode(&encode(&fault), FileKind::Rust, size + 1),
            Some(fault)
        );
        assert_eq!(decode(&forged(1), FileKind::Rust, size + 1), None);
    }

    #[test]
    fn each_weight_holds_its_value() {
        use std::sync::Arc;

        use crate::code::index::{
            Definition, Element, HandOff, Invocation, ModuleFile, Receives, Rule, TraitToken,
            TypeImpl, TypeMacro,
        };
        use crate::code::scope::Scope;
        use crate::code::symbol::Segment;

        // A name a macro gives sits in its list beside its kind, and its
        // segment in an `Arc` with two counts.
        let named = size_of::<(Arc<Segment>, DefinitionKind)>()
            + size_of::<Segment>()
            + 2 * size_of::<usize>();
        let held = [
            ("scope", size_of::<Scope>(), weight::SCOPE),
            ("segment", size_of::<Segment>(), weight::SEGMENT),
            ("definition", size_of::<Definition>(), weight::DEFINITION),
            ("invocation", size_of::<Invocation>(), weight::INVOCATION),
            ("module file", size_of::<ModuleFile>(), weight::MODULE_FILE),
            ("element", size_of::<Element>(), weight::ELEMENT),
            ("group token", size_of::<String>(), weight::ELEMENT),
            ("macro", size_of::<TypeMacro>(), weight::MACRO_PART),
            ("rule", size_of::<Rule>(), weight::MACRO_PART),
            (
                "receives",
                size_of::<(usize, Receives)>(),
                weight::MACRO_PART,
            ),
            ("impl", size_of::<TypeImpl>(), weight::MACRO_PART),
            ("trait token", size_of::<TraitToken>(), weight::MACRO_PART),
            ("hand-off", size_of::<HandOff>(), weight::MACRO_PART),
            ("named", named, weight::MACRO_PART),
        ];
        for (value, size, weight) in held {
            assert!(size <= weight, "{value}: {size} bytes, weight {weight}");
        }
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
        for (name, kind, bytes) in corpus() {
            let Ok(indexed) = index(kind, &bytes) else {
                continue;
            };
            let entry = encode(&Ok(indexed.clone())).len();
            assert!(entry <= measured(&indexed), "{name}");
            assert!(entry <= max_entry_bytes(kind, bytes.len()), "{name}");
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
            let bound = max_entry_bytes(kind, text.len());
            assert!(
                bytes.len() <= bound,
                "{kind:?} of {} bytes: {} bytes, over {bound}",
                text.len(),
                bytes.len()
            );
        }
    }
}
