use super::*;

use crate::code::index::FileKind;

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
                "macro_rules! m { ($a:ident) => { struct $a; }; } ",
                "m!(X, Y); ",
                "impl T { fn f() {} } ",
                "pub use a::{self, self, ",
                "a, ",
                "(",
                ")",
                "\"",
                "mod b;",
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

/// A file of each kind as dense in definitions as the kind allows, of
/// about `bytes` bytes: the form [`densest`] names for it, repeated.
fn densest_files(bytes: usize) -> Vec<(FileKind, String)> {
    let list = |open: &str, item: &str, close: &str| {
        format!("{open}{}{close}", item.repeat(bytes / item.len()))
    };
    // Dotted keys of 60 one-byte tables each, under distinct tops.
    let dotted: String = (0..bytes / 124)
        .map(|top| format!("k{top}{}=0\n", ".a".repeat(59)))
        .collect();
    vec![
        (FileKind::Rust, list("enum E{", "A,", "}")),
        (FileKind::Python, list("", "a=", "0\n")),
        (FileKind::TypeScript, list("enum E{", "A,", "}")),
        (FileKind::Tsx, list("enum E{", "A,", "}")),
        (FileKind::C, list("enum{", "a,", "};")),
        (FileKind::Cpp, list("enum{", "a,", "};")),
        (FileKind::Toml, dotted),
        (FileKind::Json, list("{", "\"\":0,", "\"\":0}")),
        (FileKind::Make, list("", "a ", ":\n")),
        (FileKind::Markdown, list("", "#\n", "")),
    ]
}

#[test]
fn the_densest_file_of_each_kind_is_indexed_within_its_room() {
    for (kind, text) in densest_files(64 * 1024) {
        let indexed = crate::code::testing::index(kind, text.as_bytes());

        assert!(indexed.is_ok(), "{kind:?}: {indexed:?}");
        assert!(encode(&indexed).len() <= max_entry_bytes(kind, text.len()));
    }
}

#[test]
fn every_entry_a_write_makes_reads_back() {
    let variants = format!("enum E {{ {} }}", "A, ".repeat(5_000));
    let names = format!("{}z = 0\n", "a,".repeat(50_000));
    let cases = [
        ("variants".to_owned(), FileKind::Rust, variants.into_bytes()),
        ("names".to_owned(), FileKind::Python, names.into_bytes()),
    ];
    for (name, kind, bytes) in &cases {
        let indexed = crate::code::testing::index(*kind, bytes);
        assert!(indexed.is_ok(), "{name}: {indexed:?}");
    }
    let densest = densest_files(16 * 1024)
        .into_iter()
        .map(|(kind, text)| (format!("densest {kind:?}"), kind, text.into_bytes()));
    for (name, kind, bytes) in cases
        .into_iter()
        .chain(densest)
        .chain(corpus())
        .chain(generated())
    {
        let indexed = crate::code::testing::index(kind, &bytes);
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
    let indexed =
        crate::code::testing::index(FileKind::Rust, b"mod a { fn b() {} }\nm!(y x);").unwrap();
    let bytes = encode(&Ok(indexed));

    let source = 28;
    let decoded = |bytes: &[u8]| decode(bytes, FileKind::Rust, source);
    for cut in 0..bytes.len() {
        assert_eq!(decoded(&bytes[..cut]), None, "cut at {cut}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(decoded(&longer), None);
    // The invoked names `x` and `y` out of order, a scope whose parent is
    // itself, one of no segment, and a count past the bytes.
    let mut unsorted = bytes.clone();
    let end = unsorted.len();
    unsorted.swap(end - 3, end - 1);
    assert_eq!(decoded(&unsorted), None);
    assert_eq!(decoded(&[0, 1, 1, 0, 0, 0, 0]), None);
    assert_eq!(decoded(&[0, 1, 0, 0, 0, 0, 0, 0]), None);
    assert_eq!(decoded(&[0, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
    assert_eq!(decoded(&[9]), None);
}

/// An entry of `scopes` scopes of one segment of a one-byte name each,
/// and nothing else: five bytes apiece.
fn forged(scopes: usize) -> Vec<u8> {
    let mut forged = vec![0];
    let mut count = scopes;
    while count >= 0x80 {
        forged.push(0x80 | (count & 0x7f) as u8);
        count >>= 7;
    }
    forged.push(count as u8);
    for _ in 0..scopes {
        forged.extend([0, 1, 0, 1, b'a']);
    }
    forged.extend([0, 0, 0]);
    forged
}

/// The memory glibc's malloc takes for `n` bytes on a 64-bit target.
fn malloc(n: usize) -> usize {
    match n {
        0 => 0,
        n => (n + 8).next_multiple_of(16).max(32),
    }
}

/// What the decode of [`forged`]`(scopes)` holds with its entry: the
/// scopes' 32-byte slots and their depths, and per scope a block of one
/// 48-byte segment and one of its one-byte name.
fn resident(scopes: usize) -> usize {
    let count = (usize::BITS - scopes.leading_zeros()).div_ceil(7).max(1) as usize;
    let entry = 1 + count + 5 * scopes + 3;
    entry + malloc(scopes * 32) + malloc(scopes * 8) + scopes * (64 + 32)
}

#[test]
fn a_forged_entry_decodes_to_no_more_than_the_room_of_its_file_as_the_allocator_takes_it() {
    let source = 64 * 1024;
    let room = max_entry_bytes(FileKind::Rust, source);
    let most = (1..).find(|&scopes| resident(scopes + 1) > room).unwrap();

    assert!(matches!(
        decode(&forged(most), FileKind::Rust, source),
        Some(Ok(_))
    ));
    assert_eq!(decode(&forged(most + 1), FileKind::Rust, source), None);
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

/// What [`Measure`] charges for what `index` holds, its counts added:
/// what the scope tree spent of the room.
fn measured(index: &FileIndex) -> usize {
    let mut previous = None;
    let definitions: usize = index
        .definitions
        .iter()
        .map(|definition| {
            let cost = Measure::definition(definition, previous);
            previous = Some(definition.span);
            cost
        })
        .sum();
    COUNTS_BYTES
        + definitions
        + index
            .scopes
            .iter()
            .map(|scope| Measure::scope(scope.parent, &scope.name))
            .sum::<usize>()
        + index
            .rust
            .module_files
            .iter()
            .map(Measure::module_file)
            .sum::<usize>()
        + index
            .rust
            .invoked
            .iter()
            .map(|name| Measure::invoked(name))
            .sum::<usize>()
}

#[test]
fn an_entry_takes_no_more_than_its_parts_were_charged() {
    for (name, kind, bytes) in corpus() {
        let Ok(indexed) = crate::code::testing::index(kind, &bytes) else {
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
    let invoked: String = (0..20_000).map(|n| format!("m!(n{n});")).collect();
    let reexported = format!(
        "pub use {}::{{{}}};",
        "a".repeat(100_000),
        "self,".repeat(20_000)
    );
    for (kind, text) in [
        (FileKind::Rust, variants.as_str()),
        (FileKind::Rust, fields.as_str()),
        (FileKind::Rust, nested.as_str()),
        (FileKind::Rust, invoked.as_str()),
        (FileKind::Rust, reexported.as_str()),
        (FileKind::Make, targets.as_str()),
        (FileKind::Markdown, headings.as_str()),
        (FileKind::Json, keys.as_str()),
        (FileKind::Toml, tables.as_str()),
        (FileKind::Python, names.as_str()),
        (FileKind::Python, lines.as_str()),
        (FileKind::Rust, ""),
    ] {
        let indexed = crate::code::testing::index(kind, text.as_bytes());
        assert!(indexed.is_ok(), "{kind:?} of {} bytes", text.len());
        let bytes = encode(&indexed);
        let bound = max_entry_bytes(kind, text.len());
        assert!(
            bytes.len() <= bound,
            "{kind:?} of {} bytes: {} bytes, over {bound}",
            text.len(),
            bytes.len()
        );
    }
}
