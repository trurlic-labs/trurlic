//! The lints in Cargo.toml and clippy.toml refuse what they claim to.
//!
//! Each case runs clippy on a one-file crate that carries this package's
//! `[lints]` table, `clippy.toml` and `rust-toolchain.toml`, read when the
//! test runs, and holds one seeded violation. The case passes when clippy
//! rejects it with the expected code. Clean code and code the test-only
//! exemptions cover must pass, so a case cannot fail for a broken fixture.

#![expect(
    clippy::unwrap_used,
    reason = "a fixture step that fails is a failed test; clippy exempts only #[test] bodies"
)]

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
