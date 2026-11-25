//! The check that the code index reaches no file system, process,
//! environment, thread, clock, network or console: an allow list, closed
//! by construction, of what its non-test code may name.
//!
//! Every token of every Rust file under the checked directory is read,
//! macro arguments and bodies included. Test code is left out as the
//! index's own roles mark it: a definition of the test role, a file the
//! crate's tree makes test code whole. A path's first segment says where
//! it goes, a `use` list read whole. `std`, `core` and `alloc` go only to
//! the modules of [`STD_MODULES`], and `io` only to its `ErrorKind`. The
//! crate (`crate`, `$crate`, or as many `super`s as reach its root) goes
//! only to `code` and the store's [`STORE_ITEMS`]. A dependency is named
//! only when it is one of [`CRATES`]. Refused outright: a glob or an
//! alias of such a root, `extern crate`, `#[path]`, and the prelude macros
//! of [`MACROS`], which read files or the environment or write a stream.
//!
//! What can still reach the outside: a macro the crate root defines
//! before `code` in textual scope (it has none), what an allowed crate's
//! own functions do with a handle `code` cannot name, and what a caller
//! hands in.

use std::path::Path;

use super::crates::Crates;
use super::expand::Files;
use super::index::{FileIndex, FileKind, index};
use super::kinds::Role;
use super::roles::test_files;

/// The modules of `std`, `core` and `alloc` the index may name: none
/// reads or writes outside the process's memory.
const STD_MODULES: [&str; 18] = [
    "borrow",
    "char",
    "cmp",
    "collections",
    "convert",
    "fmt",
    "iter",
    "marker",
    "mem",
    "num",
    "ops",
    "option",
    "result",
    "slice",
    "str",
    "string",
    "sync",
    "vec",
];

/// The store's items the index may name: its value types.
const STORE_ITEMS: [&str; 1] = ["Anchor"];

/// The crates the index may name: its parser, its grammar, its hash and
/// the TOML reader.
const CRATES: [&str; 4] = ["tree_sitter", "tree_sitter_rust", "blake3", "toml"];

/// The prelude's macros that read a file or the environment, or write a
/// standard stream.
const MACROS: [&str; 10] = [
    "include",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "print",
    "println",
    "eprint",
    "eprintln",
    "dbg",
];

/// Each fault of the Rust files under `root/dir`, `root` a crate's
/// directory, as `file:line: what`, files sorted.
pub(super) fn faults(root: &Path, dir: &str) -> Vec<String> {
    let texts = sources(root);
    let indexes: Vec<(&str, FileKind, FileIndex)> = texts
        .iter()
        .map(|(path, text)| {
            let kind = FileKind::of(path).unwrap();
            (path.as_str(), kind, index(kind, text.as_bytes()).unwrap())
        })
        .collect();
    let files: Files<'_> = indexes
        .iter()
        .map(|(path, kind, index)| (*path, (*kind, index)))
        .collect();
    let crates = Crates::of(&files, &Files::new());
    let tests = test_files(&files, &Files::new(), &crates);
    let dependencies = dependencies(&files);
    let mut found = Vec::new();
    for ((path, text), (_, _, indexed)) in texts.iter().zip(&indexes) {
        if !path.starts_with(&format!("{dir}/")) || tests.contains(path.as_str()) {
            continue;
        }
        let depth = module_depth(path);
        let tokens = tokens_outside_tests(text, indexed);
        found.extend(
            file_faults(&tokens, depth, &dependencies)
                .into_iter()
                .map(|(line, what)| format!("{path}:{line}: {what}")),
        );
    }
    found
}

/// Every Rust file under `root/src`, and `root/Cargo.toml`, by path.
fn sources(root: &Path) -> Vec<(String, String)> {
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                texts.push((relative, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    texts.push((
        "Cargo.toml".to_owned(),
        std::fs::read_to_string(root.join("Cargo.toml")).unwrap(),
    ));
    texts.sort();
    texts
}

/// The crate names of the dependencies `Cargo.toml` states, as a path
/// spells them.
fn dependencies(files: &Files<'_>) -> Vec<String> {
    let Some((_, manifest)) = files.get("Cargo.toml") else {
        return Vec::new();
    };
    let scopes = super::scope::Scopes {
        file: &manifest.scopes,
        added: &[],
    };
    manifest
        .definitions
        .iter()
        .filter_map(|d| {
            let mut path: Vec<String> = scopes
                .path_from_end(d.scope)
                .map(ToString::to_string)
                .collect();
            path.reverse();
            match path.as_slice() {
                [table, name] if table.ends_with("dependencies") => Some(name.replace('-', "_")),
                _ => None,
            }
        })
        .collect()
}

/// How many modules down from the crate root the file at `path` is:
/// `src/code/mod.rs` one, `src/code/rust/marks.rs` three.
fn module_depth(path: &str) -> usize {
    let inside = path.trim_start_matches("src/");
    let segments = inside.split('/').count();
    match inside.rsplit('/').next() {
        Some("mod.rs" | "lib.rs" | "main.rs") => segments - 1,
        _ => segments,
    }
}

/// The tokens of `text` outside its test definitions, each with its
/// line.
fn tokens_outside_tests(text: &str, indexed: &FileIndex) -> Vec<(usize, String)> {
    let tests: Vec<(usize, usize)> = indexed
        .definitions
        .iter()
        .filter(|d| d.role == Role::Test)
        .map(|d| (d.span.start, d.span.end))
        .collect();
    let tree = super::syntax::parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
    let mut found = Vec::new();
    super::syntax::tokens(tree.root_node(), text, &super::rust::LEXICON, |token| {
        if let super::syntax::Token::Node(node) = token {
            let at = node.start_byte();
            if !tests.iter().any(|&(from, to)| from <= at && at < to) {
                let spelled = super::syntax::text_of(node, text).to_owned();
                found.push((node.start_position().row + 1, spelled));
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

/// The faults of one file's `tokens`, a file `depth` modules below the
/// crate root, whose dependencies are `dependencies`.
fn file_faults(
    tokens: &[(usize, String)],
    depth: usize,
    dependencies: &[String],
) -> Vec<(usize, String)> {
    let spelled = |at: usize| tokens.get(at).map_or("", |(_, token)| token.as_str());
    let mut found = Vec::new();
    for at in 0..tokens.len() {
        let line = tokens.get(at).map_or(0, |(line, _)| *line);
        match (spelled(at), spelled(at + 1), spelled(at + 2)) {
            ("extern", "crate", _) => found.push((line, "extern crate".to_owned())),
            ("#", "[", "path") => found.push((line, "#[path]".to_owned())),
            (name, "!", _) if MACROS.contains(&name) => found.push((line, format!("{name}!"))),
            _ => {}
        }
        if !starts_path(tokens, at) {
            continue;
        }
        let mut next = at;
        for path in read_path(tokens, &mut next, Vec::new()) {
            if let Some(fault) = path_fault(&path, depth, dependencies) {
                found.push((line, fault));
            }
        }
    }
    found
}

/// Whether a path starts at `at`: a word or `$crate` that no `::` or `.`
/// comes before, and no visibility's `pub(..)` holds.
fn starts_path(tokens: &[(usize, String)], at: usize) -> bool {
    let spelled = |back: usize| {
        at.checked_sub(back)
            .and_then(|n| tokens.get(n))
            .map_or("", |(_, token)| token.as_str())
    };
    let first = spelled(0);
    let word = first == "$crate" || first == "::" || super::rust::is_word(first);
    let continues = matches!(spelled(1), "::" | ".");
    let visibility = (spelled(1) == "(" && spelled(2) == "pub")
        || (spelled(1) == "in" && spelled(2) == "(" && spelled(3) == "pub");
    word && !continues && !visibility
}

/// The paths a path or `use` tree from `at` names, each prefixed by
/// `prefix`, a list's entries each one; `at` is left past it.
fn read_path(tokens: &[(usize, String)], at: &mut usize, prefix: Vec<String>) -> Vec<Vec<String>> {
    let spelled = |n: usize| tokens.get(n).map_or("", |(_, token)| token.as_str());
    let mut segments = prefix;
    if spelled(*at) == "::" {
        *at += 1;
    }
    loop {
        match spelled(*at) {
            "{" => {
                *at += 1;
                let mut found = Vec::new();
                while !matches!(spelled(*at), "}" | "") {
                    found.extend(read_path(tokens, at, segments.clone()));
                    if spelled(*at) == "," {
                        *at += 1;
                    }
                }
                *at += 1;
                return found;
            }
            "*" => {
                segments.push("*".to_owned());
                *at += 1;
                return vec![segments];
            }
            word if word == "$crate" || super::rust::is_word(word) => {
                segments.push(word.to_owned());
                *at += 1;
                if spelled(*at) == "as" {
                    segments.push("as".to_owned());
                    *at += 2;
                    return vec![segments];
                }
                if spelled(*at) != "::" {
                    return vec![segments];
                }
                *at += 1;
            }
            _ => return vec![segments],
        }
    }
}

/// Why `path`, in a file `depth` modules below the crate root, reaches
/// outside what the index may name; `None` when it does not.
fn path_fault(path: &[String], depth: usize, dependencies: &[String]) -> Option<String> {
    let segments: Vec<&str> = path
        .iter()
        .map(String::as_str)
        .filter(|segment| *segment != "self")
        .collect();
    let supers = segments
        .iter()
        .take_while(|segment| **segment == "super")
        .count();
    let (root, rest): (&str, &[&str]) = match segments.as_slice() {
        [] => return None,
        [_, ..] if supers > 0 && supers >= depth => ("crate", segments.get(supers..)?),
        [_, ..] if supers > 0 => return None,
        [first, rest @ ..] => (first, rest),
    };
    let spelled = path.join("::");
    let refuse = || Some(format!("names `{spelled}`"));
    match root {
        "std" | "core" | "alloc" => match rest {
            ["io", "ErrorKind", ..] => None,
            [module, ..] if STD_MODULES.contains(module) => None,
            _ => refuse(),
        },
        "crate" | "$crate" => match rest {
            ["code", ..] => None,
            ["store", item, ..] if STORE_ITEMS.contains(item) => None,
            _ => refuse(),
        },
        name if CRATES.contains(&name) => None,
        name if dependencies.iter().any(|dependency| dependency == name) => refuse(),
        _ => None,
    }
}
