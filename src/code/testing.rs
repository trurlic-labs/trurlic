//! What the resolvers' tests read an index through: each definition's
//! path and the text of its span; and the check that the index names no
//! shell module of the crate.

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

/// The crate's modules that are the shell: the code index names none of
/// them. What std and the store reach (the file system, processes, the
/// environment, the clock, the console) is held by the compiler, through
/// clippy with `tests/pure/clippy.toml` (`tests/lints.rs`).
const SHELL_MODULES: [&str; 7] = [
    "sources",
    "git",
    "discovery",
    "commands",
    "mcp",
    "console",
    "cli",
];

/// Refuse any Rust file under `root/dir` that names a shell module of the
/// crate outside its test code; `root` is the crate's directory.
pub(crate) fn assert_names_no_shell(root: &std::path::Path, dir: &str) {
    let found = shell_names_of(root, dir);
    assert!(found.is_empty(), "{dir} names {found:?}");
}

/// Each identifier that names a shell module of the crate in the Rust
/// files under `root/dir` outside test code, as `file:line: name`, files
/// sorted. Every token is read, those of macro arguments and bodies
/// included, so neither a path's spelling, an alias nor a macro hides a
/// name: reaching a module takes its name. Test code is what the index
/// itself says is: a definition of the test role, and a file the crate's
/// tree makes test code whole (`roles.rs`), read from every Rust file
/// under `root/src` and `root/Cargo.toml`.
pub(crate) fn shell_names_of(root: &std::path::Path, dir: &str) -> Vec<String> {
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
    let indexes: Vec<(&str, FileKind, super::index::FileIndex)> = texts
        .iter()
        .map(|(path, text)| {
            let kind = FileKind::of(path).unwrap();
            (path.as_str(), kind, index(kind, text.as_bytes()).unwrap())
        })
        .collect();
    let files: super::expand::Files<'_> = indexes
        .iter()
        .map(|(path, kind, index)| (*path, (*kind, index)))
        .collect();
    let crates = super::expand::Crates::of(&files, &super::expand::Files::new());
    let tests = super::roles::test_files(&files, &super::expand::Files::new(), &crates);
    let mut found = Vec::new();
    for ((path, text), (_, _, indexed)) in texts.iter().zip(&indexes) {
        if !path.starts_with(&format!("{dir}/")) || tests.contains(path.as_str()) {
            continue;
        }
        found.extend(
            shell_names_in(text, indexed)
                .into_iter()
                .map(|(line, name)| format!("{path}:{line}: {name}")),
        );
    }
    found
}

/// Each identifier of `text`, a Rust file whose index is `indexed`, that
/// names a shell module outside the spans of its test definitions: its
/// line and name.
fn shell_names_in(text: &str, indexed: &super::index::FileIndex) -> Vec<(usize, String)> {
    let tests: Vec<(usize, usize)> = indexed
        .definitions
        .iter()
        .filter(|d| d.role == super::kinds::Role::Test)
        .map(|d| (d.span.start, d.span.end))
        .collect();
    let tree = super::syntax::parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
    let mut found = Vec::new();
    super::syntax::tokens(tree.root_node(), text, &super::rust::LITERALS, |token| {
        if let super::syntax::Token::Node(node) = token {
            let name = super::syntax::text_of(node, text);
            let at = node.start_byte();
            let in_test = tests.iter().any(|&(from, to)| from <= at && at < to);
            if node.kind() == "identifier" && SHELL_MODULES.contains(&name) && !in_test {
                found.push((node.start_position().row + 1, name.to_owned()));
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}
