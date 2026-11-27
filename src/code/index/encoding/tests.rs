use super::room::COUNTS_BYTES;
use super::{Measure, decode, encode, max_entry_bytes};
use crate::code::index::{FileIndex, FileKind, IndexFault, MAX_DOC_BYTES};

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
/// about `bytes` bytes: the form [`densest`](super::room::densest) names
/// for it, repeated.
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

/// A manifest that states each fact a manifest's entry holds.
const MANIFEST: &str = "\
[package]
name = 'a'
workspace = '..'
[dependencies]
b = { path = '../b' }
c = { workspace = true }
[workspace]
exclude = ['old']
[workspace.dependencies]
d = { path = 'd' }
e = '1'
";

#[test]
fn every_entry_a_write_makes_reads_back() {
    let variants = format!("enum E {{ {} }}", "A, ".repeat(5_000));
    let names = format!("{}z = 0\n", "a,".repeat(50_000));
    let long = "a ".repeat(1_000);
    let docs =
        format!("//! {long}\n/** {long} */\nfn f() {{}}\n/// b\nstruct S {{ /// c\n d: u8 }}");
    let cases = [
        ("variants".to_owned(), FileKind::Rust, variants.into_bytes()),
        ("names".to_owned(), FileKind::Python, names.into_bytes()),
        (
            "manifest".to_owned(),
            FileKind::Toml,
            MANIFEST.as_bytes().to_vec(),
        ),
        (
            "virtual".to_owned(),
            FileKind::Toml,
            b"[workspace]\n".to_vec(),
        ),
        ("docs".to_owned(), FileKind::Rust, docs.into_bytes()),
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
    let text = b"//! F.\nmod a { /// B.\nfn b() {} }";
    let indexed = crate::code::testing::index(FileKind::Rust, text).unwrap();
    let bytes = encode(&Ok(indexed));

    let source = text.len();
    let decoded = |bytes: &[u8]| decode(bytes, FileKind::Rust, source);
    for cut in 0..bytes.len() {
        assert_eq!(decoded(&bytes[..cut]), None, "cut at {cut}");
    }
    let manifest = encode(&crate::code::testing::index(
        FileKind::Toml,
        MANIFEST.as_bytes(),
    ));
    for cut in 0..manifest.len() {
        let decoded = decode(&manifest[..cut], FileKind::Toml, MANIFEST.len());
        assert_eq!(decoded, None, "manifest cut at {cut}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(decoded(&longer), None);
    // A scope whose parent is itself, one of no segment, and a count past
    // the bytes.
    assert_eq!(decoded(&[0, 1, 1, 0, 0, 0, 0]), None);
    assert_eq!(decoded(&[0, 1, 0, 0, 0, 0, 0, 0]), None);
    assert_eq!(decoded(&[0, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
    assert_eq!(decoded(&[9]), None);
}

/// An entry of no scope, definition, module file or manifest, and a file
/// doc of `code` and `text`.
fn file_doc_entry(code: u8, text: &[u8]) -> Vec<u8> {
    let length = u16::try_from(text.len()).unwrap();
    let mut entry = vec![0, 0, 0, 0, code];
    entry.extend([0x80 | (length & 0x7f) as u8, (length >> 7) as u8]);
    entry.extend(text);
    entry.push(0);
    entry
}

#[test]
fn a_doc_reads_back_within_its_bound_and_none_past_it_empty_or_of_no_code() {
    let source = 4096;
    let decoded = |bytes: &[u8]| decode(bytes, FileKind::Rust, source);
    let at_bound = vec![b'a'; MAX_DOC_BYTES];
    let past = vec![b'a'; MAX_DOC_BYTES + 1];

    let Some(Ok(index)) = decoded(&file_doc_entry(2, &at_bound)) else {
        panic!("a cut doc at the bound is an index");
    };
    let doc = index.doc.unwrap();
    assert_eq!((doc.text().len(), doc.is_cut()), (MAX_DOC_BYTES, true));
    assert_eq!(decoded(&file_doc_entry(1, &past)), None);
    assert_eq!(decoded(&file_doc_entry(1, b"")), None);
    assert_eq!(decoded(&file_doc_entry(3, b"a")), None);
    assert_eq!(decoded(&[0, 0, 0, 0, 3, 0]), None);
}

/// An entry of `scopes` scopes of one segment of a one-byte name each,
/// and nothing else: five bytes apiece, no doc and no manifest.
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
    forged.extend([0, 0, 0, 0]);
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
    let entry = 1 + count + 5 * scopes + 4;
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
    assert_eq!(decode(&forged(0), FileKind::Rust, size + 1), None);
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
        + index.manifest.as_deref().map_or(0, Measure::manifest)
        + index.doc.as_ref().map_or(0, Measure::file_doc)
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
    let dependencies: String = (0..20_000)
        .map(|n| format!("a{n}.path=''\nb{n}.workspace=true\n"))
        .collect();
    let manifest = format!("[package]\n[dependencies]\n{dependencies}");
    let nested = format!("{}{}", "mod m{".repeat(60), "}".repeat(60));
    let names = format!("{}z = 0\n", "a,".repeat(50_000));
    let lines = "x=1\n".repeat(50_000);
    let reexported = format!(
        "pub use {}::{{{}}};",
        "a".repeat(100_000),
        "self,".repeat(20_000)
    );
    // Near the cap: an item with the shortest doc in seven bytes, a
    // paragraph of a megabyte, a doc past the bound every 600 bytes, and a
    // file doc of a megabyte.
    let documented = format!("enum E{{{}}}", "///a\nA,".repeat(149_000));
    let paragraph = format!(
        "{}fn f() {{}}",
        format!("/// {}\n", "a".repeat(95)).repeat(10_000)
    );
    let past_bound = format!("/// {}\nfn f() {{}}\n", "a".repeat(580)).repeat(1_700);
    let file_doc = "//! a\n".repeat(170_000);
    for (kind, text) in [
        (FileKind::Rust, documented.as_str()),
        (FileKind::Rust, paragraph.as_str()),
        (FileKind::Rust, past_bound.as_str()),
        (FileKind::Rust, file_doc.as_str()),
        (FileKind::Rust, variants.as_str()),
        (FileKind::Rust, fields.as_str()),
        (FileKind::Rust, nested.as_str()),
        (FileKind::Rust, reexported.as_str()),
        (FileKind::Make, targets.as_str()),
        (FileKind::Markdown, headings.as_str()),
        (FileKind::Json, keys.as_str()),
        (FileKind::Toml, tables.as_str()),
        (FileKind::Toml, manifest.as_str()),
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
