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
const MISSING: &str = "its file defines nothing its symbol names";
const LEDGER: &str = "crates/ledger/src/lib.rs";
const BOOKS: &str = "crates/ledger/src/books/mod.rs";

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
    (LEDGER, "Book::new", SYMBOL),
    (LEDGER, "Book::close", SYMBOL),
    (LEDGER, "Weight", SYMBOL),
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
    ("docs/guide.md", "Install::From source", SYMBOL),
    ("docs/guide.md", "Guide::Use", SYMBOL),
    ("docs/guide.md", "Not a heading", MISSING),
    ("docs/guide.md", "source", MISSING),
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
