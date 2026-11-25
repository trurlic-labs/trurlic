use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::store::testing::setup_store;

/// The per-construct fixture corpus, `tests/fixtures/code/`. Its manifests
/// are named `Cargo.toml.fixture`, so cargo takes no directory of the
/// repository for a package of its own; the copy names them `Cargo.toml`.
fn fixture_tree() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code");
    let mut pending = vec![source.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(&source).unwrap().to_str().unwrap();
            let target = tmp.path().join(relative.trim_end_matches(".fixture"));
            if path.is_dir() {
                fs::create_dir_all(&target).unwrap();
                pending.push(path);
            } else {
                fs::copy(&path, &target).unwrap();
            }
        }
    }
    let store = setup_store(tmp.path());
    (tmp, store)
}

fn anchor(file: &str, symbol: &str) -> Anchor {
    Anchor::new(
        file.to_owned(),
        (!symbol.is_empty()).then(|| symbol.to_owned()),
    )
    .unwrap()
}

/// What `resolution` is, without its span and hash.
fn outcome(resolution: &Resolution) -> String {
    match resolution.to_string() {
        text if text.starts_with("resolves to lines") => "symbol".to_owned(),
        text => text,
    }
}

/// What each of `cases` resolves to in the tree of `store`.
fn resolve_all(store: &Store, cases: &[(&str, &str, &str)]) -> Vec<String> {
    let anchors: Vec<Anchor> = cases.iter().map(|(f, s, _)| anchor(f, s)).collect();
    let facts = gather(store, anchors.iter().map(Anchor::file));
    anchors
        .iter()
        .map(|anchor| format!("{anchor} -> {}", outcome(&facts.resolve(anchor))))
        .collect()
}

fn expected(cases: &[(&str, &str, &str)]) -> Vec<String> {
    cases
        .iter()
        .map(|(f, s, want)| format!("{} -> {want}", anchor(f, s)))
        .collect()
}

const SYMBOL: &str = "symbol";
const DEEPER: &str = "its file nests a path deeper than 64 segments";
const MISSING: &str = "its file defines nothing its symbol names";
const LEDGER: &str = "crates/ledger/src/lib.rs";
const BOOKS: &str = "crates/ledger/src/books/mod.rs";
const RULES: &str = "crates/ledger/src/rules.rs";

/// One case per construct: the anchor's file and symbol, and what it
/// resolves to.
const RUST_CASES: &[(&str, &str, &str)] = &[
    (LEDGER, "PAGE_SIZE", SYMBOL),
    (LEDGER, "OPENED", SYMBOL),
    (LEDGER, "page_count", SYMBOL),
    (LEDGER, "tally", SYMBOL),
    (LEDGER, "helper_in_a_template", MISSING),
    (LEDGER, "Book", SYMBOL),
    (LEDGER, "archive::Book", SYMBOL),
    (LEDGER, "Book::title", SYMBOL),
    (LEDGER, "Book::pages", "its symbol names 2 definitions"),
    (LEDGER, "Book::pages()", SYMBOL),
    (LEDGER, "Book::title()", MISSING),
    (
        LEDGER,
        "Book::pages(n)",
        "its symbol holds something between the parentheses of its `()`",
    ),
    (
        LEDGER,
        "Book::<Display>()",
        "its symbol ends in `()` after a trait segment",
    ),
    (
        LEDGER,
        "Book::pages)",
        "its symbol has a parenthesis that does not pair",
    ),
    (LEDGER, "Book::new", SYMBOL),
    (LEDGER, "Book::close", SYMBOL),
    (LEDGER, "Weight", SYMBOL),
    (LEDGER, "PageCount", SYMBOL),
    (LEDGER, "Raw::bits", SYMBOL),
    (LEDGER, "Entry::Blank", SYMBOL),
    (LEDGER, "Amount", SYMBOL),
    (LEDGER, "Entry::Transfer::from", SYMBOL),
    (LEDGER, "from", "its symbol names 3 definitions"),
    (LEDGER, "Book::<Display>::fmt", SYMBOL),
    (LEDGER, "Book::<fmt::Display>::fmt", SYMBOL),
    (LEDGER, "Book::fmt", SYMBOL),
    (LEDGER, "fmt", "its symbol names 2 definitions"),
    (LEDGER, "Book::<Display>", SYMBOL),
    (LEDGER, "Weight::<Debug>", SYMBOL),
    (LEDGER, "Book::<From<Entry>>::from", SYMBOL),
    (LEDGER, "Book::<From<Entry>>", SYMBOL),
    (LEDGER, "<From<Weight>>", SYMBOL),
    (
        LEDGER,
        "Book::<From>::from",
        "its symbol names 2 definitions",
    ),
    (LEDGER, "Book::from", "its symbol names 2 definitions"),
    (LEDGER, "From<Entry>", MISSING),
    (LEDGER, "Book::<Audit>::audit", SYMBOL),
    (LEDGER, "Book::<Audit>::Report", SYMBOL),
    (LEDGER, "Audit::audit", SYMBOL),
    (LEDGER, "audit", "its symbol names 2 definitions"),
    (LEDGER, "Audit::Report", SYMBOL),
    (LEDGER, "Audit::summary", SYMBOL),
    (LEDGER, "DEPTH", "its symbol names 2 definitions"),
    (LEDGER, "Audit::DEPTH", SYMBOL),
    (LEDGER, "books", SYMBOL),
    (LEDGER, "archive", SYMBOL),
    (LEDGER, "seal", SYMBOL),
    (LEDGER, "reconcile", SYMBOL),
    (LEDGER, "tests::reconcile", SYMBOL),
    (LEDGER, "reconcile::weigh", SYMBOL),
    (LEDGER, "moved", MISSING),
    (LEDGER, "Catalog", SYMBOL),
    (LEDGER, "Rack", SYMBOL),
    (LEDGER, "Shelf", MISSING),
    (LEDGER, "Imported", MISSING),
    (LEDGER, "external_total", SYMBOL),
    (LEDGER, "LedgerView", SYMBOL),
    (LEDGER, "CURRENT", SYMBOL),
    (LEDGER, "vanished", MISSING),
    (LEDGER, "Phantom", MISSING),
    (LEDGER, "LedgerView::book", SYMBOL),
    (LEDGER, "SPELLINGS", SYMBOL),
    (
        LEDGER,
        "Book::<Display",
        "its symbol has an angle bracket that does not pair",
    ),
    (LEDGER, "Book::", "its symbol has an empty segment"),
    (BOOKS, "CatalogId", SYMBOL),
    (BOOKS, "CatalogId::new", SYMBOL),
    (BOOKS, "CatalogId::raw", SYMBOL),
    (BOOKS, "Isbn", SYMBOL),
    (BOOKS, "ShelfNumber", SYMBOL),
    (BOOKS, "ShelfNumber::get", SYMBOL),
    (BOOKS, "NotAType", MISSING),
    (BOOKS, "book_id", SYMBOL),
    ("crates/shelf/src/lib.rs", "ShelfId", SYMBOL),
    ("crates/shelf/src/lib.rs", "ShelfId::get", SYMBOL),
    ("crates/shelf/src/lib.rs", "Hidden", MISSING),
    ("crates/shelf/src/lib.rs", "ShelfFull", SYMBOL),
    ("crates/shelf/src/lib.rs", "PyException", MISSING),
    ("crates/ids/src/lib.rs", "Local", SYMBOL),
    ("crates/ids/src/lib.rs", "numbered_id", SYMBOL),
    (RULES, "sign", SYMBOL),
    (RULES, "Eight", SYMBOL),
    (RULES, "Nine", MISSING),
    (RULES, "Owned", SYMBOL),
    (RULES, "Owned::get", SYMBOL),
    (RULES, "Owned::<Display>::fmt", SYMBOL),
    (RULES, "Owned::fmt", SYMBOL),
    (RULES, "Owned::from", MISSING),
    (RULES, "Named", MISSING),
    (RULES, "commented::helper", SYMBOL),
    (RULES, "Listed", SYMBOL),
    ("loose/single.rs", "Alone", SYMBOL),
    ("loose/single.rs", "Visible", SYMBOL),
];

const OTHER_CASES: &[(&str, &str, &str)] = &[
    ("Cargo.toml", "workspace", SYMBOL),
    ("Cargo.toml", "workspace::lints", SYMBOL),
    ("Cargo.toml", "lints::clippy::unwrap_used", SYMBOL),
    ("Cargo.toml", "clippy::expect_used", MISSING),
    ("Cargo.toml", "profile::release::panic", SYMBOL),
    (
        "Cargo.toml",
        "bench::name",
        "its symbol names 2 definitions",
    ),
    ("Cargo.toml", "bench", SYMBOL),
    ("settings.json", "stages::order", SYMBOL),
    ("settings.json", "parallel", SYMBOL),
    (
        "settings.json",
        "rules::name",
        "its symbol names 2 definitions",
    ),
    ("settings.json", "order::stages", MISSING),
    (
        "broken.json",
        "stages",
        "its file does not parse, near byte 0",
    ),
    ("Makefile", "ci", SYMBOL),
    ("Makefile", "check-format", SYMBOL),
    ("Makefile", ".PHONY", SYMBOL),
    ("Makefile", "VERSION", MISSING),
    ("Makefile", "ci()", MISSING),
    ("Makefile", "lint", MISSING),
    ("Makefile", "fake", MISSING),
    ("Makefile", "TEMPLATE", MISSING),
    ("docs/guide.md", "Install::From source", SYMBOL),
    ("docs/guide.md", "Guide::Use", SYMBOL),
    ("docs/guide.md", "Not a heading", MISSING),
    ("docs/guide.md", "source", MISSING),
    ("docs/guide.md", "Hidden by tildes", MISSING),
    ("docs/guide.md", "Guide::Store::open", SYMBOL),
    ("docs/guide.md", "Calling run()", SYMBOL),
    ("docs/guide.md", "Install (Linux)", SYMBOL),
    ("settings.json", "f()", SYMBOL),
    ("settings.json", "g(x)", SYMBOL),
    ("settings.json", "caf\u{e9}", SYMBOL),
    ("bounds/deep.json", "a", SYMBOL),
    ("bounds/deeper.json", "a", DEEPER),
    ("bounds/deep.toml", "key", SYMBOL),
    ("bounds/deeper.toml", "key", DEEPER),
    ("bounds/deep.md", "b", SYMBOL),
    ("bounds/deeper.md", "b", DEEPER),
    ("bounds/deep.rs", "m", SYMBOL),
    ("bounds/deeper.rs", "m", DEEPER),
    (
        "ci.yml",
        "on",
        "its symbol is on a kind of file no resolver reads",
    ),
    ("ci.yml", "", "resolves to the file"),
    (LEDGER, "", "resolves to the file"),
    ("docs", "", "resolves to the directory"),
    ("docs", "Guide", "its symbol is on a directory"),
    ("gone.rs", "", "nothing is at its path"),
    ("gone.rs", "x", "nothing is at its path"),
    ("docs/guide.md/x", "", "nothing is at its path"),
];

#[test]
fn each_construct_of_the_fixture_corpus_resolves_as_stated() {
    let (_tmp, store) = fixture_tree();

    assert_eq!(resolve_all(&store, RUST_CASES), expected(RUST_CASES));
    assert_eq!(resolve_all(&store, OTHER_CASES), expected(OTHER_CASES));
}

#[test]
fn a_resolved_symbol_carries_the_span_and_hash_of_its_definition() {
    let (tmp, store) = fixture_tree();
    let weigh = anchor(LEDGER, "reconcile::weigh");
    let resolution = gather(&store, [LEDGER].into_iter()).resolve(&weigh);
    let text = fs::read_to_string(tmp.path().join(LEDGER)).unwrap();
    let line = text.lines().position(|l| l.contains("fn weigh")).unwrap() + 1;

    assert_eq!(outcome(&resolution), SYMBOL, "{resolution}");
    assert!(
        resolution
            .to_string()
            .starts_with(&format!("resolves to lines {line}-{}", line + 3)),
        "{resolution}"
    );

    // A comment and a reformat leave the hash as it was; a token moves it.
    let path = tmp.path().join(LEDGER);
    let reformatted = text.replace(
        "        let _ = entry;\n        1\n",
        "        // Every entry weighs one.\n        let _ =   entry;   1\n",
    );
    fs::write(&path, &reformatted).unwrap();
    let hash = |resolution: Resolution| {
        resolution
            .to_string()
            .rsplit(", ")
            .next()
            .unwrap()
            .to_owned()
    };
    let after = gather(&store, [LEDGER].into_iter()).resolve(&weigh);
    assert_eq!(hash(after), hash(resolution));
    fs::write(&path, reformatted.replace("1\n", "2\n")).unwrap();
    let changed = gather(&store, [LEDGER].into_iter()).resolve(&weigh);
    assert_ne!(hash(changed), hash(resolution));
}

#[test]
fn hostile_files_are_refused_with_their_cause() {
    let (tmp, store) = fixture_tree();
    let big = File::create(tmp.path().join("big.json")).unwrap();
    big.set_len(MAX_SOURCE_FILE_BYTES + 1).unwrap();
    fs::write(tmp.path().join("bytes.md"), b"# Title\n\xff\n").unwrap();
    let mut cases = vec![
        (
            "big.json",
            "a",
            "its file is 4194305 bytes, over the 4194304 a source file may hold",
        ),
        ("bytes.md", "Title", "its file is not UTF-8 from byte 8"),
        ("big.json", "", "resolves to the file"),
    ];
    #[cfg(unix)]
    {
        let fifo = tmp.path().join("crates/ledger/src/fifo.rs");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        std::os::unix::fs::symlink("loop.rs", tmp.path().join("loop.rs")).unwrap();
        cases.extend([
            (
                "crates/ledger/src/fifo.rs",
                "x",
                "its file is not a regular file",
            ),
            ("crates/ledger/src/fifo.rs", "", "resolves to the file"),
            (
                "loop.rs",
                "x",
                "its file cannot be read: filesystem loop or indirection limit (e.g. symlink loop)",
            ),
            ("loop.rs", "", "resolves to the file"),
        ]);
    }

    assert_eq!(resolve_all(&store, &cases), expected(&cases));
}

/// The names of the cache's entries.
fn entries(store: &Store) -> BTreeSet<String> {
    let dir = store.root().join(".state/symbols");
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_warm_index_reads_the_cache_and_answers_as_a_cold_one() {
    let (_tmp, store) = fixture_tree();
    let cold = resolve_all(&store, RUST_CASES);
    let written = entries(&store);
    assert!(!written.is_empty());

    let warm = resolve_all(&store, RUST_CASES);

    assert_eq!(warm, cold);
    assert_eq!(entries(&store), written);
}

#[test]
fn an_entry_that_is_not_what_its_key_says_is_a_miss_and_is_replaced() {
    let (_tmp, store) = fixture_tree();
    let cold = resolve_all(&store, RUST_CASES);
    let dir = store.root().join(".state/symbols");
    let names: Vec<_> = entries(&store).into_iter().collect();
    // Every entry swapped with the next: each now holds another key.
    let bytes: Vec<_> = names
        .iter()
        .map(|n| fs::read(dir.join(n)).unwrap())
        .collect();
    for (name, other) in names.iter().zip(bytes.iter().cycle().skip(1)) {
        fs::write(dir.join(name), other).unwrap();
    }

    assert_eq!(resolve_all(&store, RUST_CASES), cold);
    for (name, before) in names.iter().zip(&bytes) {
        assert_eq!(&fs::read(dir.join(name)).unwrap(), before, "{name}");
    }
}

#[test]
fn a_busy_lock_skips_the_cache_write() {
    let (_tmp, store) = fixture_tree();
    let held = store.lock().unwrap();

    let cold = resolve_all(&store, RUST_CASES);
    assert!(entries(&store).is_empty());
    drop(held);

    assert_eq!(resolve_all(&store, RUST_CASES), cold);
    assert!(!entries(&store).is_empty());
}

#[test]
fn the_anchors_of_the_graph_resolve_against_the_code_root() {
    let (tmp, store) = fixture_tree();
    let decision = r#"component = "project"
choice = "Books keep their pages"
reason = "A reason."
alternatives = ["None: rejected"]
status = "proposed"
created = "2026-01-01T00:00:00Z"
anchors = [
    { file = "crates/ledger/src/lib.rs", symbol = "Book::pages" },
    { file = "crates/ledger/src/lib.rs", symbol = "Book::new" },
    { file = "docs" },
]
"#;
    fs::write(tmp.path().join(".trurlic/decisions/pages.toml"), decision).unwrap();
    let state = store.load_state().unwrap();

    let resolved: Vec<String> = resolve_anchors(&store, &state)
        .iter()
        .map(|(anchor, resolution)| format!("{anchor} -> {}", outcome(resolution)))
        .collect();

    assert_eq!(
        resolved,
        [
            format!("{LEDGER}::Book::new -> {SYMBOL}"),
            format!("{LEDGER}::Book::pages -> its symbol names 2 definitions"),
            "docs -> resolves to the directory".to_owned(),
        ]
    );
}

#[test]
fn a_file_gone_before_its_read_is_absent_not_a_fault() {
    let tmp = TempDir::new().unwrap();

    assert!(matches!(
        read_source(&tmp.path().join("gone.rs")),
        Err(Gone)
    ));
    assert!(matches!(
        read_source(&tmp.path().join("gone.rs/x")),
        Err(Gone)
    ));
    assert!(matches!(
        read_source(tmp.path()),
        Err(Faulty(ReadFault::NotAFile))
    ));
}

/// Each definition of `facts`: its file and path, kind, role and
/// visibility, `-` for none.
fn statements(facts: &CodeFacts) -> Vec<String> {
    facts
        .definitions()
        .filter(|(file, ..)| !file.starts_with("bounds/"))
        .map(|(file, scopes, definition, role)| {
            let path = crate::code::testing::path(scopes, definition.scope);
            let visibility = definition
                .visibility
                .map_or("-".to_owned(), |v| format!("{v:?}"));
            format!(
                "{file} {} {:?} {role:?} {visibility}",
                path, definition.kind
            )
        })
        .collect()
}

/// Each definition of the fixture corpus as [`statements`] writes it: one
/// per kind of definition, the tests of each mark (`#[test]`,
/// `#[tokio::test]`, `#[bench]`, a `cfg` that holds only under `test`, an
/// inner `#![cfg(test)]`, a `proptest!` case, a file a `#[cfg(test)] mod`
/// declares, a file under `tests/`) and each visibility.
const STATEMENTS: &[&str] = &[
    "Cargo.toml workspace Key Code -",
    "Cargo.toml workspace::members Key Code -",
    "Cargo.toml workspace::lints::clippy Key Code -",
    "Cargo.toml workspace::lints Key Code -",
    "Cargo.toml workspace::lints::clippy::unwrap_used Key Code -",
    "Cargo.toml workspace::lints::rust Key Code -",
    "Cargo.toml workspace::lints::rust::unsafe_code Key Code -",
    "Cargo.toml profile Key Code -",
    "Cargo.toml profile::release Key Code -",
    "Cargo.toml profile::release::panic Key Code -",
    "Cargo.toml profile::release::lto Key Code -",
    "Cargo.toml bench Key Code -",
    "Cargo.toml bench::name Key Code -",
    "Cargo.toml bench::name Key Code -",
    "Makefile .PHONY Target Code -",
    "Makefile ci Target Code -",
    "Makefile test Target Code -",
    "Makefile check-format Target Code -",
    "crates/ids/Cargo.toml package Key Code -",
    "crates/ids/Cargo.toml package::name Key Code -",
    "crates/ids/src/lib.rs numbered_id Macro Code Public",
    "crates/ids/src/lib.rs crate_only_id Macro Code Private",
    "crates/ids/src/lib.rs Local Struct Code -",
    "crates/ledger/Cargo.toml package Key Code -",
    "crates/ledger/Cargo.toml package::name Key Code -",
    "crates/ledger/src/abs.rs not_reached Function Code Public",
    "crates/ledger/src/books/mod.rs book_id Macro Code Private",
    "crates/ledger/src/books/mod.rs Catalog Struct Code Public",
    "crates/ledger/src/books/mod.rs Shelf Struct Code Public",
    "crates/ledger/src/books/mod.rs Imported Struct Code Public",
    "crates/ledger/src/books/mod.rs call_only Function Code Public",
    "crates/ledger/src/books/mod.rs CatalogId Struct Code -",
    "crates/ledger/src/books/mod.rs CatalogId::new Method Code -",
    "crates/ledger/src/books/mod.rs CatalogId::raw Method Code -",
    "crates/ledger/src/books/mod.rs Isbn Struct Code -",
    "crates/ledger/src/books/mod.rs Isbn::new Method Code -",
    "crates/ledger/src/books/mod.rs Isbn::raw Method Code -",
    "crates/ledger/src/books/mod.rs ShelfNumber Struct Code -",
    "crates/ledger/src/books/mod.rs ShelfNumber::get Method Code -",
    "crates/ledger/src/checks.rs balanced Function Test Public",
    "crates/ledger/src/lib.rs PAGE_SIZE Constant Code Public",
    "crates/ledger/src/lib.rs OPENED Static Code Private",
    "crates/ledger/src/lib.rs page_count Function Code Crate",
    "crates/ledger/src/lib.rs SPELLINGS Constant Code Public",
    "crates/ledger/src/lib.rs tally Macro Code Private",
    "crates/ledger/src/lib.rs Book Struct Code Public",
    "crates/ledger/src/lib.rs Book::title Field Code Public",
    "crates/ledger/src/lib.rs Book::pages Field Code Private",
    "crates/ledger/src/lib.rs Weight Struct Code Public",
    "crates/ledger/src/lib.rs PageCount TypeAlias Code Public",
    "crates/ledger/src/lib.rs Raw Union Code Public",
    "crates/ledger/src/lib.rs Raw::bits Field Code Private",
    "crates/ledger/src/lib.rs Raw::float Field Code Private",
    "crates/ledger/src/lib.rs Entry Enum Code Public",
    "crates/ledger/src/lib.rs Entry::Blank Variant Code Public",
    "crates/ledger/src/lib.rs Entry::Amount Variant Code Public",
    "crates/ledger/src/lib.rs Entry::Transfer Variant Code Public",
    "crates/ledger/src/lib.rs Entry::Transfer::from Field Code Public",
    "crates/ledger/src/lib.rs Entry::Transfer::to Field Code Public",
    "crates/ledger/src/lib.rs Audit Trait Code Public",
    "crates/ledger/src/lib.rs Audit::Report AssociatedType Code Public",
    "crates/ledger/src/lib.rs Audit::DEPTH Constant Code Public",
    "crates/ledger/src/lib.rs Audit::audit Method Code Public",
    "crates/ledger/src/lib.rs Audit::summary Method Code Public",
    "crates/ledger/src/lib.rs Book::new Method Code Public",
    "crates/ledger/src/lib.rs Book::pages Method Code Public",
    "crates/ledger/src/lib.rs Book::<Display> Impl Code -",
    "crates/ledger/src/lib.rs Book::<Display>::fmt Method Code -",
    "crates/ledger/src/lib.rs Weight::<Debug> Impl Code -",
    "crates/ledger/src/lib.rs Weight::<Debug>::fmt Method Code -",
    "crates/ledger/src/lib.rs Book::<From<Entry>> Impl Code -",
    "crates/ledger/src/lib.rs Book::<From<Entry>>::from Method Code -",
    "crates/ledger/src/lib.rs Book::<From<Weight>> Impl Code -",
    "crates/ledger/src/lib.rs Book::<From<Weight>>::from Method Code -",
    "crates/ledger/src/lib.rs Book::<Audit> Impl Code -",
    "crates/ledger/src/lib.rs Book::<Audit>::Report AssociatedType Code -",
    "crates/ledger/src/lib.rs Book::<Audit>::DEPTH Constant Code -",
    "crates/ledger/src/lib.rs Book::<Audit>::audit Method Code -",
    "crates/ledger/src/lib.rs Book::close Method Code Public",
    "crates/ledger/src/lib.rs books Module Code Public",
    "crates/ledger/src/lib.rs marks Module Code Private",
    "crates/ledger/src/lib.rs rules Module Code Private",
    "crates/ledger/src/lib.rs checks Module Test Private",
    "crates/ledger/src/lib.rs archive Module Code Private",
    "crates/ledger/src/lib.rs archive::seal Function Code Public",
    "crates/ledger/src/lib.rs archive::Book Struct Code Public",
    "crates/ledger/src/lib.rs reconcile Function Code Private",
    "crates/ledger/src/lib.rs reconcile::weigh Function Code Private",
    "crates/ledger/src/lib.rs Catalog Reexport Code Public",
    "crates/ledger/src/lib.rs Rack Reexport Code Public",
    "crates/ledger/src/lib.rs external_total Function Code Private",
    "crates/ledger/src/lib.rs LedgerView Struct Code Crate",
    "crates/ledger/src/lib.rs LedgerView::book Field Code Private",
    "crates/ledger/src/lib.rs CURRENT Static Code Private",
    "crates/ledger/src/lib.rs tests Module Test Private",
    "crates/ledger/src/lib.rs tests::reconcile Function Test Private",
    "crates/ledger/src/marks.rs for_the_parent Function Code Super",
    "crates/ledger/src/marks.rs for_a_path Function Code InPath",
    "crates/ledger/src/marks.rs for_itself Function Code Private",
    "crates/ledger/src/marks.rs the_old_spelling Function Code Crate",
    "crates/ledger/src/marks.rs runs_on_a_runtime Function Test Private",
    "crates/ledger/src/marks.rs measures Function Test Private",
    "crates/ledger/src/marks.rs only_under_test_on_unix Function Test Private",
    "crates/ledger/src/marks.rs also_built_for_benches Function Code Public",
    "crates/ledger/src/marks.rs never_under_test Function Code Private",
    "crates/ledger/src/marks.rs holds_for_any Function Test Private",
    "crates/ledger/src/marks.rs nested Module Test Crate",
    "crates/ledger/src/marks.rs nested::inside_a_test_module Function Test Public",
    "crates/ledger/src/rules.rs sign Function Code Public",
    "crates/ledger/src/rules.rs listed Macro Code Private",
    "crates/ledger/src/rules.rs worded Macro Code Private",
    "crates/ledger/src/rules.rs owned Macro Code Private",
    "crates/ledger/src/rules.rs commented Module Test Private",
    "crates/ledger/src/rules.rs commented::helper Function Test Private",
    "crates/ledger/src/rules.rs Listed Reexport Code Public",
    "crates/ledger/src/rules.rs absolute Module Test Private",
    "crates/ledger/src/rules.rs scoped Function Code Private",
    "crates/ledger/src/rules.rs scoped::in_body Module Test Private",
    "crates/ledger/src/rules.rs Eight Struct Code -",
    "crates/ledger/src/rules.rs i Struct Code -",
    "crates/ledger/src/rules.rs Owned Struct Code -",
    "crates/ledger/src/rules.rs Owned::get Method Code -",
    "crates/ledger/src/rules.rs Owned::<Display> Impl Code -",
    "crates/ledger/src/rules.rs Owned::<Display>::fmt Method Code -",
    "crates/ledger/src/rules/in_body.rs reached_by_test Function Test Public",
    "crates/ledger/tests/flow.rs opened_book Function Test Private",
    "crates/ledger/tests/flow.rs a_book_opens Function Test Private",
    "crates/shelf/Cargo.toml package Key Code -",
    "crates/shelf/Cargo.toml package::name Key Code -",
    "crates/shelf/src/lib.rs ShelfId Struct Code -",
    "crates/shelf/src/lib.rs ShelfId::get Method Code -",
    "crates/shelf/src/lib.rs ShelfFull Struct Code -",
    "docs/guide.md Guide Heading Code -",
    "docs/guide.md Guide::Install Heading Code -",
    "docs/guide.md Guide::Install::From source Heading Code -",
    "docs/guide.md Guide::Install::From a package Heading Code -",
    "docs/guide.md Guide::Use Heading Code -",
    "docs/guide.md Guide::Store::open Heading Code -",
    "docs/guide.md Guide::Calling run() Heading Code -",
    "docs/guide.md Guide::Install (Linux) Heading Code -",
    "loose/single.rs solo Macro Code Private",
    "loose/single.rs Alone Enum Code -",
    "loose/single.rs Visible Struct Code -",
    "loose/single.rs Visible::get Method Code -",
    "settings.json stages Key Code -",
    "settings.json stages::order Key Code -",
    "settings.json stages::parallel Key Code -",
    "settings.json rules Key Code -",
    "settings.json rules::name Key Code -",
    "settings.json rules::name Key Code -",
    "settings.json esc\"aped Key Code -",
    "settings.json caf\u{e9} Key Code -",
    "settings.json f() Key Code -",
    "settings.json g(x) Key Code -",
];

#[test]
fn each_definition_of_the_fixture_corpus_states_its_kind_role_and_visibility() {
    let (_tmp, store) = fixture_tree();
    let facts = gather(
        &store,
        ["Makefile", "docs/guide.md", "settings.json"].into_iter(),
    );

    assert_eq!(statements(&facts), STATEMENTS);
}

#[test]
fn an_anchor_resolves_alike_whatever_else_is_anchored() {
    let tmp = TempDir::new().unwrap();
    let files = [
        ("Cargo.toml", "[package]\nname = 'root'\n"),
        (
            "src/lib.rs",
            "macro_rules! id { (x $n:ident) => { pub struct $n; }; }\n",
        ),
        ("vendor/sub/.git", ""),
        ("vendor/sub/Cargo.toml", "[package]\nname = 'sub'\n"),
        ("vendor/sub/src/lib.rs", "id! { x Thing }\n"),
        (
            "vendor/sub/src/own.rs",
            "macro_rules! id { (x $n:ident) => { pub struct $n; }; }\n",
        ),
    ];
    for (path, text) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let store = setup_store(tmp.path());
    let thing = anchor("vendor/sub/src/lib.rs", "Thing");
    let resolve = |anchored: &[&str]| {
        let facts = gather(&store, anchored.iter().copied());
        outcome(&facts.resolve(&thing))
    };

    let alone = resolve(&["vendor/sub/src/lib.rs"]);
    assert_eq!(alone, MISSING);
    assert_eq!(
        resolve(&["vendor/sub/Cargo.toml", "vendor/sub/src/lib.rs"]),
        alone
    );
    assert_eq!(
        resolve(&["vendor/sub/src/own.rs", "vendor/sub/src/lib.rs"]),
        alone
    );
}
