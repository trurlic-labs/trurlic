//! What of a crate is test code, by a rule of the checks' own, never the
//! index's: an item an exact `#[test]` or `#[cfg(test)]` marks, or whose
//! body opens with an inner `#![cfg(test)]`; a file whose root holds an
//! inner `#![cfg(test)]`, whose every `mod` declaration is such an item,
//! or that is a module of such a file.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use tree_sitter::Node;

use crate::code::testing::{is_comment, text_of};

/// What a test mark holds.
pub(super) enum Held {
    /// The bytes of the item it marks, the mark included.
    Item(Range<usize>),
    /// The whole file: an inner `#![cfg(test)]` at its root.
    File,
}

/// What `node` holds when it is a test mark: an exact `#[test]` or
/// `#[cfg(test)]`, or an inner `#![cfg(test)]`.
pub(super) fn held_by(node: Node<'_>, text: &str) -> Option<Held> {
    match node.kind() {
        "inner_attribute_item" if compact(node, text) == "#![cfg(test)]" => Some(
            match node.parent().filter(|body| body.kind() != "source_file") {
                Some(body) => Held::Item(body.parent().unwrap_or(body).byte_range()),
                None => Held::File,
            },
        ),
        "attribute_item" if is_test_mark(node, text) => marked_item(node).map(Held::Item),
        _ => None,
    }
}

/// What decides whether one file is test code whole.
pub(super) struct FileMarks {
    /// Each `mod name;` it declares, and whether a test mark holds it.
    pub(super) declared: Vec<(String, bool)>,
    /// Whether an inner `#![cfg(test)]` at its root makes it test code
    /// whole.
    pub(super) test: bool,
}

/// `node`'s text without whitespace.
fn compact(node: Node<'_>, text: &str) -> String {
    text_of(node, text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Whether the attribute `node` is exactly `#[test]` or `#[cfg(test)]`.
fn is_test_mark(node: Node<'_>, text: &str) -> bool {
    matches!(compact(node, text).as_str(), "#[test]" | "#[cfg(test)]")
}

/// The bytes from the attribute `node` to the end of the item it marks.
fn marked_item(node: Node<'_>) -> Option<Range<usize>> {
    let mut next = node.next_sibling();
    while let Some(sibling) =
        next.filter(|sibling| sibling.kind() == "attribute_item" || is_comment(*sibling))
    {
        next = sibling.next_sibling();
    }
    next.map(|item| node.start_byte()..item.end_byte())
}

/// The files of `files`, each by its path, that are test code whole:
/// under an inner `#![cfg(test)]`, declared only by marked `mod` items, or
/// in a test file's modules.
pub(super) fn test_files<'a>(files: &BTreeMap<&'a str, FileMarks>) -> BTreeSet<&'a str> {
    // Each file's declarations: whether one ships it.
    let mut shipped: BTreeMap<String, bool> = BTreeMap::new();
    for (path, file) in files {
        for (name, marked) in &file.declared {
            let dir = modules_dir(path);
            for child in [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")] {
                *shipped.entry(child).or_default() |= !marked;
            }
        }
    }
    let parent = |path: &str| {
        files
            .keys()
            .copied()
            .find(|parent| *parent != path && modules_dir(parent) == parent_dir(path))
    };
    let mut tests: BTreeSet<&str> = files
        .iter()
        .filter(|(path, file)| file.test || shipped.get(**path) == Some(&false))
        .map(|(path, _)| *path)
        .collect();
    loop {
        let more: Vec<&str> = files
            .keys()
            .copied()
            .filter(|path| !tests.contains(path))
            .filter(|path| parent(path).is_some_and(|parent| tests.contains(parent)))
            .collect();
        if more.is_empty() {
            break;
        }
        tests.extend(more);
    }
    tests
}

/// The directory of the files the `mod name;` items of the file at `path`
/// declare: `src` for the crate roots `src/lib.rs` and `src/main.rs`,
/// `a/b` for `a/b.rs` and `a/b/mod.rs`.
pub(super) fn modules_dir(path: &str) -> &str {
    if matches!(path, "src/lib.rs" | "src/main.rs") {
        return "src";
    }
    let stem = path.strip_suffix(".rs").unwrap_or(path);
    stem.strip_suffix("/mod").unwrap_or(stem)
}

/// The directory the module file at `path` is declared from: `a` for
/// `a/b.rs` and `a/b/mod.rs`, none for a crate root.
fn parent_dir(path: &str) -> &str {
    modules_dir(path)
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
}
