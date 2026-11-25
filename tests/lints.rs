//! The lints in Cargo.toml and clippy.toml refuse what they claim to, and
//! the code index (`src/code/`) reaches nothing `tests/pure/clippy.toml`
//! names.
//!
//! Each case runs clippy on a one-file crate that carries this package's
//! `[lints]` table, `clippy.toml` and `rust-toolchain.toml`, read when the
//! test runs, and holds one seeded violation. The case passes when clippy
//! rejects it with the expected code. Clean code and code the test-only
//! exemptions cover must pass, so a case cannot fail for a broken fixture.
//!
//! The purity check runs clippy over this library's own code, test code
//! left out as the compiler leaves it out (`--lib`), with the purity
//! configuration in place of the crate's, and refuses every finding whose
//! span, or a macro call that expands to it, is in `src/code/`. The
//! compiler resolves each path, so an alias, a glob, a re-export, a
//! macro's argument or body and a method call all name what they reach.
//! Its seeded cases are a crate named as this one, one evasion per file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// `(case, expected error code, lib.rs)`. An empty code means clippy must
/// accept the source.
const CASES: &[(&str, &str, &str)] = &[
    (
        "clean code passes",
        "",
        "pub fn f(x: Option<u8>) -> u8 { x.unwrap_or(0) }",
    ),
    (
        "tests may unwrap, expect and panic",
        "",
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        \
         Some(1).unwrap();\n        Some(1).expect(\"present\");\n        \
         if false { panic!(\"seeded\") }\n    }\n}",
    ),
    ("unsafe code", "unsafe_code", "pub fn f() { unsafe {} }"),
    (
        "unsafe code cannot be excepted",
        "E0453",
        "#[expect(unsafe_code, reason = \"seeded\")]\npub fn f() {}",
    ),
    (
        "unwrap",
        "clippy::unwrap_used",
        "pub fn f(x: Option<u8>) -> u8 { x.unwrap() }",
    ),
    (
        "expect",
        "clippy::expect_used",
        "pub fn f(x: Option<u8>) -> u8 { x.expect(\"x\") }",
    ),
    (
        "panic",
        "clippy::panic",
        "pub fn f() { panic!(\"seeded\") }",
    ),
    ("todo", "clippy::todo", "pub fn f() { todo!() }"),
    (
        "unimplemented",
        "clippy::unimplemented",
        "pub fn f() { unimplemented!() }",
    ),
    (
        "dbg",
        "clippy::dbg_macro",
        "pub fn f(x: u8) -> u8 { dbg!(x) }",
    ),
    (
        "println",
        "clippy::print_stdout",
        "pub fn f() { println!(\"seeded\") }",
    ),
    (
        "eprintln",
        "clippy::print_stderr",
        "pub fn f() { eprintln!(\"seeded\") }",
    ),
    (
        "a stdout handle",
        "clippy::disallowed_methods",
        "pub fn f() -> std::io::Stdout { std::io::stdout() }",
    ),
    (
        "a stderr handle",
        "clippy::disallowed_methods",
        "pub fn f() -> std::io::Stderr { std::io::stderr() }",
    ),
    (
        "a clock read",
        "clippy::disallowed_methods",
        "pub fn f() -> std::time::SystemTime { std::time::SystemTime::now() }",
    ),
    (
        "allow",
        "clippy::allow_attributes",
        "#[allow(dead_code, reason = \"seeded\")]\nfn f() {}",
    ),
    (
        "expect without a reason",
        "clippy::allow_attributes_without_reason",
        "#[expect(dead_code)]\nfn f() {}",
    ),
    (
        "HashMap",
        "clippy::disallowed_types",
        "pub fn f() -> std::collections::HashMap<u8, u8> { Default::default() }",
    ),
    (
        "HashSet",
        "clippy::disallowed_types",
        "pub fn f() -> std::collections::HashSet<u8> { Default::default() }",
    ),
];

#[test]
fn each_lint_refuses_its_seeded_violation() {
    let fixture = Fixture::new();
    for (case, expected, source) in CASES {
        let report = fixture.clippy(source);
        let held = if expected.is_empty() {
            report.passed
        } else {
            !report.passed && report.codes.iter().any(|code| code == expected)
        };
        assert!(held, "{case}: {:?}\n{}", report.codes, report.stderr);
    }
}

/// The evasions of the purity configuration, one per file of the seeded
/// crate's `src/code/`, each of which clippy must find.
const PURE_CASES: &[(&str, &str)] = &[
    (
        "path",
        "pub fn f() -> Vec<u8> { std::fs::read(\"/etc/hostname\").unwrap_or_default() }",
    ),
    (
        "macro_argument",
        "pub fn f() -> String { format!(\"{:?}\", std::fs::read(\"/etc/hostname\")) }",
    ),
    (
        "macro_body",
        "macro_rules! read { () => { std::fs::read(\"/etc/hostname\") }; }\npub fn f() -> bool { read!().is_ok() }",
    ),
    (
        "glob",
        "use std::*;\npub fn f() -> bool { fs::read(\"/x\").is_ok() }",
    ),
    (
        "self_alias",
        "use std::{self as s};\npub fn f() -> bool { s::fs::read(\"/x\").is_ok() }",
    ),
    (
        "extern_crate",
        "extern crate std as s;\npub fn f() -> bool { s::fs::read(\"/x\").is_ok() }",
    ),
    (
        "path_method",
        "pub fn f() -> bool { std::path::Path::new(\"/etc\").exists() }",
    ),
    (
        "stdin",
        "use std::io::BufRead;\npub fn f(line: &mut String) -> bool { std::io::stdin().lock().read_line(line).is_ok() }",
    ),
    (
        "symlink",
        "pub fn f() -> bool { std::os::unix::fs::symlink(\"/a\", \"/b\").is_ok() }",
    ),
    (
        "sleep",
        "pub fn f() { std::thread::sleep(std::time::Duration::ZERO) }",
    ),
    (
        "include",
        "pub fn f() -> &'static str { include_str!(\"../lib.rs\") }",
    ),
    (
        "option_env",
        "pub fn f() -> Option<&'static str> { option_env!(\"HOME\") }",
    ),
    (
        "env",
        "pub fn f() -> Option<String> { std::env::var(\"HOME\").ok() }",
    ),
    ("process", "pub fn f() -> u32 { std::process::id() }"),
    (
        "clock",
        "pub fn f() -> std::time::Instant { std::time::Instant::now() }",
    ),
    (
        "store",
        "pub fn f(store: &crate::store::Store) { store.read_symbols() }",
    ),
    (
        "crate_clock",
        "pub fn f() { let _ = crate::store::Timestamp::now(); }",
    ),
    ("console", "pub fn f() { crate::console::diag!(\"x\") }"),
    (
        "shell_macro",
        "pub fn f() -> bool { crate::read_hostname!() }",
    ),
];

/// A file of the seeded crate's `src/code/` that names nothing the purity
/// configuration refuses, so a case cannot pass for a configuration that
/// refuses everything.
const PURE_CLEAN: &str = "use std::collections::BTreeMap;\nuse std::path::Path;\n\
     pub fn f() -> (BTreeMap<u8, u8>, std::io::ErrorKind, bool) {\n    \
     (BTreeMap::new(), std::io::ErrorKind::NotFound, Path::new(\"a\").join(\"b\").starts_with(\"a\"))\n}\n";

/// The rest of the seeded crate: the shell it may not reach.
const PURE_ROOT: &str = "pub mod code;\n\
     pub mod store {\n    pub struct Store;\n    impl Store { pub fn read_symbols(&self) {} }\n    \
     pub struct StoreLock;\n    pub struct Writer;\n    \
     pub mod time {\n        pub struct Timestamp;\n        impl Timestamp { pub fn now() -> Self { Timestamp } }\n    }\n    \
     pub use time::Timestamp;\n}\n\
     pub mod console {\n    pub fn write_out(_: std::fmt::Arguments<'_>) {}\n    \
     pub fn write_diag(_: std::fmt::Arguments<'_>) {}\n    \
     macro_rules! out { ($($arg:tt)*) => { $crate::console::write_out(format_args!($($arg)*)) }; }\n    \
     macro_rules! diag { ($($arg:tt)*) => { $crate::console::write_diag(format_args!($($arg)*)) }; }\n    \
     pub(crate) use {diag, out};\n}\n\
     #[macro_export]\nmacro_rules! read_hostname { () => { std::fs::read(\"/etc/hostname\").is_ok() }; }\n";

#[test]
fn the_purity_configuration_finds_each_evasion_of_its_seeded_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pure-gates");
    let code = dir.join("src/code");
    if code.exists() {
        fs::remove_dir_all(&code).unwrap();
    }
    fs::create_dir_all(&code).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"trurlic\"\nedition = \"2024\"\npublish = false\n\n\
         # Its own workspace, not a member of the one above it.\n[workspace]\n",
    )
    .unwrap();
    fs::copy(
        root.join("rust-toolchain.toml"),
        dir.join("rust-toolchain.toml"),
    )
    .unwrap();
    fs::write(dir.join("src/lib.rs"), PURE_ROOT).unwrap();
    let mut modules = String::from("pub mod clean;\n");
    fs::write(code.join("clean.rs"), PURE_CLEAN).unwrap();
    for (case, source) in PURE_CASES {
        modules.push_str(&format!("pub mod {case};\n"));
        fs::write(code.join(format!("{case}.rs")), source).unwrap();
    }
    fs::write(code.join("mod.rs"), modules).unwrap();

    let found = pure_findings(&dir);

    let expected: Vec<String> = PURE_CASES
        .iter()
        .map(|(case, _)| format!("src/code/{case}.rs"))
        .collect();
    let mut expected = expected;
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn the_code_index_reaches_nothing_the_purity_configuration_names() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    assert_eq!(pure_findings(root), Vec::<String>::new());
}

/// The files under `src/code/` of the crate at `dir` that clippy, with
/// the purity configuration, finds naming something it refuses: at the
/// finding's span, or at a macro call that expands to it.
fn pure_findings(dir: &Path) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO"))
        .args(["clippy", "--offline", "--lib", "--message-format=json"])
        .current_dir(dir)
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_TARGET_TMPDIR")).join("pure-target"),
        )
        .env("CLIPPY_CONF_DIR", root.join("tests/pure"))
        .output()
        .unwrap();
    let messages: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let unresolved: Vec<&str> = messages
        .iter()
        .filter_map(|message| message["message"]["rendered"].as_str())
        .filter(|rendered| rendered.contains("does not refer to"))
        .collect();
    assert!(
        unresolved.is_empty(),
        "a path of the purity configuration names nothing: {unresolved:?}"
    );
    let mut found: Vec<String> = messages
        .iter()
        .filter(|message| {
            message["message"]["code"]["code"]
                .as_str()
                .is_some_and(|code| code.starts_with("clippy::disallowed_"))
        })
        .flat_map(|message| {
            let mut files = Vec::new();
            let mut spans: Vec<Value> = message["message"]["spans"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            while let Some(span) = spans.pop() {
                if let Some(file) = span["file_name"].as_str() {
                    files.push(file.to_owned());
                }
                if !span["expansion"]["span"].is_null() {
                    spans.push(span["expansion"]["span"].clone());
                }
            }
            files
        })
        .filter(|file| file.starts_with("src/code/"))
        .collect();
    found.sort();
    found.dedup();
    found
}

struct Report {
    passed: bool,
    /// Codes of the errors, lint names included.
    codes: Vec<String>,
    stderr: String,
}

/// A crate under `target/tmp` whose lint configuration is this package's.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lint-gates");
        fs::create_dir_all(dir.join("src")).unwrap();
        let manifest: toml::Table =
            toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
        let lints = toml::Table::from_iter([("lints".to_owned(), manifest["lints"].clone())]);
        let fixture_manifest = format!(
            "[package]\nname = \"lint-gates\"\nedition = \"2024\"\npublish = false\n\n\
             # Its own workspace, not a member of the one above it.\n[workspace]\n\n{}",
            toml::to_string(&lints).unwrap()
        );
        fs::write(dir.join("Cargo.toml"), fixture_manifest).unwrap();
        for file in ["clippy.toml", "rust-toolchain.toml"] {
            fs::copy(root.join(file), dir.join(file)).unwrap();
        }
        Self { dir }
    }

    fn clippy(&self, source: &str) -> Report {
        fs::write(self.dir.join("src/lib.rs"), source).unwrap();
        let output = Command::new(env!("CARGO"))
            .args([
                "clippy",
                "--offline",
                "--all-targets",
                "--message-format=json",
            ])
            .current_dir(&self.dir)
            .env("CARGO_TARGET_DIR", self.dir.join("target"))
            .env_remove("CLIPPY_CONF_DIR")
            .output()
            .unwrap();
        let codes = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|message| message["message"]["level"] == "error")
            .filter_map(|message| {
                message["message"]["code"]["code"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        Report {
            passed: output.status.success(),
            codes,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}
