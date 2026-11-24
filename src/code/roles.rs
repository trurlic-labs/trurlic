//! Test code a Rust file's bytes cannot state, read from the tree: a
//! file under its crate's `tests/` directory, and a module file that a
//! test module declares (`#[cfg(test)] mod tests;`), or that a test file
//! declares, at any depth. It is a step over the tree, like the macro
//! expansion, so a file's cached index stays a function of its bytes.
//!
//! A module file is `dir/name.rs` or `dir/name/mod.rs`, declared as
//! `mod name;` at the top of `dir/mod.rs`, `dir/lib.rs`, `dir/main.rs` or
//! `dir.rs`. A `#[path]` attribute, and a `mod name;` inside an inline
//! module, are not followed: their file is test code only by its own
//! attributes or its place under `tests/`.

use std::collections::{BTreeMap, BTreeSet};

use super::expand::{Crates, file_name, parent};
use super::index::{FileIndex, FileKind};
use super::kinds::{DefinitionKind, Role};
use super::symbol::Segment;

/// The files a module is declared in, from the directory its file is in.
const DECLARING_FILES: [&str; 3] = ["mod.rs", "lib.rs", "main.rs"];

/// The Rust files of `files` that are test code whole; `crates` is of
/// the same files.
pub(super) fn test_files<'i>(
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    let mut known: BTreeMap<&str, bool> = BTreeMap::new();
    for (&path, (kind, _)) in files {
        if *kind != FileKind::Rust || known.contains_key(path) {
            continue;
        }
        // The files from `path` up the chain of declarations, until one
        // whose answer is known or settled; each step goes to a file
        // nearer the crate's root, so the chain ends within the tree.
        let mut chain = vec![path];
        let mut current = path;
        let test = loop {
            if let Some(&test) = known.get(current) {
                break test;
            }
            if under_tests(current, crates) {
                break true;
            }
            let Some((declaring, name)) = declared_in(current, files) else {
                break false;
            };
            if declares_test_module(files, declaring, name) {
                break true;
            }
            if chain.len() > files.len() {
                break false;
            }
            chain.push(declaring);
            current = declaring;
        };
        known.extend(chain.into_iter().map(|file| (file, test)));
    }
    known
        .into_iter()
        .filter_map(|(path, test)| test.then_some(path))
        .collect()
}

/// Whether `path` is under the `tests/` directory of its crate.
fn under_tests(path: &str, crates: &Crates<'_>) -> bool {
    let root = crates.crate_of(path);
    if root == path {
        return false;
    }
    let relative = match root {
        "" => Some(path),
        root => path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/')),
    };
    relative.is_some_and(|relative| relative.starts_with("tests/"))
}

/// The file of `files` that declares the module whose file is `path`, and
/// the module's name; `None` for a crate's root or a file no file of
/// `files` declares.
fn declared_in<'i, 'p>(
    path: &'p str,
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
) -> Option<(&'i str, &'p str)> {
    let (dir, name) = match file_name(path) {
        "lib.rs" | "main.rs" => return None,
        "mod.rs" => {
            let dir = parent(path);
            (parent(dir), file_name(dir))
        }
        file => (parent(path), file.strip_suffix(".rs")?),
    };
    let joined = |file: &str| match dir {
        "" => file.to_owned(),
        dir => format!("{dir}/{file}"),
    };
    let beside = (!dir.is_empty()).then(|| format!("{dir}.rs"));
    let candidates = DECLARING_FILES
        .iter()
        .map(|file| joined(file))
        .chain(beside);
    for candidate in candidates {
        if let Some((&declaring, (FileKind::Rust, _))) = files.get_key_value(candidate.as_str()) {
            return Some((declaring, name));
        }
    }
    None
}

/// Whether `declaring` declares the module `name` at its top as test
/// code.
fn declares_test_module(
    files: &BTreeMap<&str, (FileKind, &FileIndex)>,
    declaring: &str,
    name: &str,
) -> bool {
    let module = [Segment::name(name)];
    files.get(declaring).is_some_and(|(_, index)| {
        index.definitions.iter().any(|definition| {
            definition.kind == DefinitionKind::Module
                && definition.path == module
                && definition.role == Role::Test
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::index;

    fn test_files_of(files: &[(&str, &str)]) -> Vec<String> {
        let indexes: Vec<(&str, FileKind, FileIndex)> = files
            .iter()
            .map(|(path, text)| {
                let kind = FileKind::of(path).unwrap();
                (*path, kind, index(kind, text.as_bytes()).unwrap())
            })
            .collect();
        let tree = indexes
            .iter()
            .map(|(path, kind, index)| (*path, (*kind, index)))
            .collect();
        let crates = Crates::of(&tree);
        test_files(&tree, &crates)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_file_is_test_code_under_tests_or_declared_by_test_code() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod a;\n#[cfg(test)]\nmod tests;\npub mod b;\n",
            ),
            ("src/a.rs", "fn f() {}\n"),
            ("src/tests.rs", "mod deeper;\n"),
            ("src/tests/deeper.rs", "mod deepest;\n"),
            ("src/tests/deeper/deepest/mod.rs", "fn g() {}\n"),
            ("src/b.rs", "#[cfg(test)]\nmod checks;\nmod plain;\n"),
            ("src/b/checks.rs", ""),
            ("src/b/plain.rs", ""),
            ("tests/flow.rs", "fn h() {}\n"),
            ("tests/common/mod.rs", ""),
            ("crates/x/Cargo.toml", "[package]\n"),
            ("crates/x/tests/it.rs", ""),
            ("crates/x/src/tests.rs", ""),
            ("loose/tests/alone.rs", ""),
        ];

        assert_eq!(
            test_files_of(&files),
            [
                "crates/x/tests/it.rs",
                "src/b/checks.rs",
                "src/tests.rs",
                "src/tests/deeper.rs",
                "src/tests/deeper/deepest/mod.rs",
                "tests/common/mod.rs",
                "tests/flow.rs",
            ]
        );
    }
}
