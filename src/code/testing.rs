//! What the resolvers' tests read an index through: each definition's
//! path and the text of its span.

use super::index::{FileKind, index};
use super::scope::{ScopeId, Scopes};
use super::symbol::Symbol;

/// Each definition of `text`, a file of `kind`, as its path joined by
/// `::` and the text of its span, in the index's order.
pub(crate) fn defined(kind: FileKind, text: &str) -> Vec<(String, &str)> {
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    index
        .definitions
        .iter()
        .map(|d| (path(scopes, d.scope), &text[d.span.start..d.span.end]))
        .collect()
}

/// The path of `scope`, its segments joined by `::`.
pub(crate) fn path(scopes: Scopes<'_>, scope: ScopeId) -> String {
    let mut segments: Vec<String> = scopes
        .path_from_end(scope)
        .map(ToString::to_string)
        .collect();
    segments.reverse();
    segments.join("::")
}

/// The text of each definition of `text`, a file of `kind`, that
/// `symbol` names.
pub(crate) fn named<'t>(kind: FileKind, text: &'t str, symbol: &str) -> Vec<&'t str> {
    let symbol = Symbol::parse(symbol, kind.syntax()).unwrap();
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    index
        .definitions
        .iter()
        .filter(|d| symbol.names(scopes.path_from_end(d.scope)))
        .map(|d| &text[d.span.start..d.span.end])
        .collect()
}

/// Paths only a shell may name: under `std`, the file system, processes,
/// the environment, the clock, the network and `io`'s readers and
/// writers; the crate's shell modules; the crate's clock.
const SHELL_PREFIXES: [&[&str]; 13] = [
    &["std", "fs"],
    &["std", "process"],
    &["std", "env"],
    &["std", "time"],
    &["std", "net"],
    &["io", "Read"],
    &["io", "Write"],
    &["Timestamp", "now"],
    &["crate", "sources"],
    &["crate", "git"],
    &["crate", "console"],
    &["crate", "discovery"],
    &["crate", "commands"],
];

/// Refuse any Rust file under `dir` that names a shell path outside its
/// test code.
pub(crate) fn assert_names_no_shell(dir: &std::path::Path) {
    let found = shell_paths_of(dir);
    assert!(found.is_empty(), "{} names {found:?}", dir.display());
}

/// Each shell path the Rust files under `dir` name outside test code, as
/// `file: path`, files sorted. Every path of the parse tree is read, a
/// `use` tree's leaves joined to their prefix, so neither an alias nor a
/// list hides one; `std` imported or aliased whole counts too. Test code
/// is an item under `#[cfg(test)]`, and a file a `#[cfg(test)] mod`
/// declares.
pub(crate) fn shell_paths_of(dir: &std::path::Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            match path.is_dir() {
                true => pending.push(path),
                false if path.extension().is_some_and(|e| e == "rs") => files.push(path),
                false => {}
            }
        }
    }
    files.sort();
    let mut test_files = std::collections::BTreeSet::new();
    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let tree = super::syntax::parse(&text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
        let relative = file.strip_prefix(dir).unwrap().to_str().unwrap().to_owned();
        for (path, declared_test) in shell_paths_in(tree.root_node(), &text) {
            match declared_test {
                Some(module) => {
                    test_files.insert(declared_file(file, &module));
                }
                None => found.push((file.clone(), format!("{relative}: {path}"))),
            }
        }
    }
    found
        .into_iter()
        .filter(|(file, _)| {
            !test_files
                .iter()
                .any(|test| file.starts_with(test.with_extension("")) || file == test)
        })
        .map(|(_, path)| path)
        .collect()
}

/// The file a `mod module;` in `file` names, without its `.rs` or
/// `/mod.rs`.
fn declared_file(file: &std::path::Path, module: &str) -> std::path::PathBuf {
    let dir = file.parent().unwrap();
    let base = match file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs" | "lib.rs" | "main.rs") => dir.to_path_buf(),
        _ => dir.join(file.file_stem().unwrap()),
    };
    base.join(format!("{module}.rs"))
}

/// The shell paths the tree under `root` names outside `#[cfg(test)]`,
/// each `None`, and the module of each `#[cfg(test)] mod name;`, each
/// `Some(name)`.
fn shell_paths_in(root: tree_sitter::Node<'_>, text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        let mut test = false;
        for child in node.named_children(&mut cursor) {
            let spelled: String = super::syntax::text_of(child, text)
                .split_whitespace()
                .collect();
            if child.kind() == "attribute_item" {
                test |= spelled == "#[cfg(test)]";
                continue;
            }
            if std::mem::take(&mut test) {
                if child.kind() == "mod_item" && child.child_by_field_name("body").is_none() {
                    let name = child.child_by_field_name("name").unwrap();
                    found.push((
                        String::new(),
                        Some(super::syntax::text_of(name, text).to_owned()),
                    ));
                }
                continue;
            }
            match child.kind() {
                "use_declaration" => {
                    let argument = child.child_by_field_name("argument").unwrap();
                    found.extend(
                        use_paths(argument, text)
                            .into_iter()
                            .filter(|p| is_shell(p))
                            .map(|p| (p, None)),
                    );
                }
                "scoped_identifier" | "scoped_type_identifier" if is_shell(&spelled) => {
                    found.push((spelled, None));
                }
                _ => pending.push(child),
            }
        }
    }
    found
}

/// Each full path a `use` tree names, a leaf joined to its prefix.
fn use_paths(argument: tree_sitter::Node<'_>, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![(argument, String::new())];
    while let Some((node, prefix)) = pending.pop() {
        let join = |rest: &str| match prefix.is_empty() {
            true => rest.to_owned(),
            false => format!("{prefix}::{rest}"),
        };
        let spelled = |node| -> String {
            super::syntax::text_of(node, text)
                .split_whitespace()
                .collect()
        };
        match node.kind() {
            "scoped_use_list" => {
                let path = node
                    .child_by_field_name("path")
                    .map(spelled)
                    .unwrap_or_default();
                let list = node.child_by_field_name("list").unwrap();
                pending.push((list, join(&path)));
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    pending.push((child, prefix.clone()));
                }
            }
            "use_as_clause" => {
                let path = node
                    .child_by_field_name("path")
                    .map(spelled)
                    .unwrap_or_default();
                found.push(join(&path));
            }
            "self" => found.push(prefix.clone()),
            _ => found.push(join(&spelled(node))),
        }
    }
    found
}

/// Whether `path` reaches a shell path, or is `std` itself.
fn is_shell(path: &str) -> bool {
    let segments: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    segments == ["std"]
        || SHELL_PREFIXES.iter().any(|prefix| {
            segments
                .windows(prefix.len())
                .any(|window| window == *prefix)
        })
}
