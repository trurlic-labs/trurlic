//! The check that a pure module (the code index, `lint`) reaches no file
//! system, process, environment, thread, clock, network or console: an
//! allow list per module ([`AllowList`]), closed by construction, of what
//! its non-test code may name.
//!
//! Every token of every Rust file under the checked directory is read,
//! macro arguments and bodies included, a raw identifier as its name.
//! Test code is left out by a rule of the check's own, never the index's.
//! It is an item an exact `#[test]` or `#[cfg(test)]` marks, or whose body
//! opens with an inner `#![cfg(test)]`, a file whose every `mod`
//! declaration is such an item or in a test file, and a file whose root
//! holds an inner `#![cfg(test)]`. A path's first segment says where it
//! goes, a `use` list read whole. `std`, `core` and `alloc` go only to
//! the modules of [`STD_MODULES`] and the items of the list. The crate
//! (`crate`, `$crate`, or as many `super`s as reach its root) goes only to
//! the list's modules and store items. A dependency is named only when the
//! list holds it or the item named. Refused outright: the names of
//! [`REFUSED_NAMES`] wherever they stand, a glob or an alias of such a
//! root, an alias of any path of `std`, `core` or `alloc`, `extern crate`,
//! a `macro_rules!` definition, an attribute that names a `path`
//! (`#[path = ..]`, `#[cfg_attr(.., path = ..)]`), a `mod name` in a
//! macro's tokens, which declares a module the check cannot see marked, a
//! `mod name;` inside an inline module, whose file the check places by
//! its declaring file's path alone, and the prelude macros of [`MACROS`],
//! which read files or the environment or write a stream.
//! The one include admitted is `include_str!` of a plain string literal,
//! a relative path that stays inside the checked directory: a language's
//! query file, source of the index as its Rust files are.
//!
//! What can still reach the outside, a list complete for what the check
//! reads (paths, names, macros and attributes): a macro the
//! crate root defines before the checked module in textual scope (it has
//! none); what an allowed crate's or item's own functions and methods do,
//! with a handle the module cannot name or a value it holds; what a caller
//! hands in (a closure, a value whose `Drop` or trait impl does I/O); and
//! the runtime's own messages to stderr, a panic's or a failed
//! allocation's.

use std::path::Path;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use tree_sitter::Node;

use crate::code::FileKind;
use crate::code::testing::{RUST_LEXICON, Scopes, Token, is_comment, parse, text_of};

/// The modules of `std`, `core` and `alloc` a pure module may name: none
/// reads or writes outside the process's memory, but for the hashed
/// collections of [`REFUSED_NAMES`].
const STD_MODULES: [&str; 17] = [
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
    "vec",
];

/// The names a pure module never spells, wherever they stand, so no
/// alias or import reaches them: the hashed collections, whose default
/// hasher's keys are read from the operating system's entropy.
const REFUSED_NAMES: [&str; 5] = ["HashMap", "HashSet", "RandomState", "hash_map", "hash_set"];

/// What a pure module's non-test code may name beyond the modules of
/// [`STD_MODULES`].
pub(crate) struct AllowList {
    /// The crate's modules: the checked one and the pure ones it reads.
    pub(crate) modules: &'static [&'static str],
    /// The store's items: its value types.
    pub(crate) store_items: &'static [&'static str],
    /// Items of `std`, `core` and `alloc` outside those modules, as
    /// (module, item).
    pub(crate) std_items: &'static [(&'static str, &'static str)],
    /// The dependencies it may name whole.
    pub(crate) crates: &'static [&'static str],
    /// The items of a dependency it may name, as (crate, item).
    pub(crate) crate_items: &'static [(&'static str, &'static str)],
}

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

/// Each fault of the Rust files under `root/dir` against `allowed`, `root`
/// a crate's directory, as `file:line: what`, files sorted.
pub(crate) fn faults(root: &Path, dir: &str, allowed: &AllowList) -> Vec<String> {
    let texts = sources(root);
    let dependencies = dependencies(&texts);
    let read: BTreeMap<&str, Read> = texts
        .iter()
        .filter(|(path, _)| path.ends_with(".rs"))
        .map(|(path, text)| (path.as_str(), read(text)))
        .collect();
    let tests = test_files(&read);
    let mut found = Vec::new();
    for (path, file) in &read {
        if !path.starts_with(&format!("{dir}/")) || tests.contains(path) {
            continue;
        }
        let place = Place { path, dir };
        found.extend(
            file_faults(
                &file.tokens,
                module_depth(path),
                &dependencies,
                place,
                allowed,
            )
            .into_iter()
            .map(|(line, what)| format!("{path}:{line}: {what}")),
        );
        found.extend(
            file.unplaced
                .iter()
                .map(|(line, what)| format!("{path}:{line}: {what}")),
        );
    }
    found
}

/// What the check reads of a file: its tokens outside test items, each
/// with its line, each `mod name;` it declares and whether a test mark
/// holds it, the line of each module outside test items whose file the
/// check cannot place, and why, and whether the whole file is test code.
struct Read {
    tokens: Vec<(usize, String)>,
    modules: Vec<(String, bool)>,
    unplaced: Vec<(usize, &'static str)>,
    test: bool,
}

/// What the check reads of `text`. A module file is placed by the file
/// that declares it, so a `mod name;` inside an inline module, whose file
/// is under that module's directory, is refused rather than placed.
fn read(text: &str) -> Read {
    let tree = parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
    let mut marked: Vec<Range<usize>> = Vec::new();
    let mut modules = Vec::new();
    let mut unplaced = Vec::new();
    let mut test = false;
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        match node.kind() {
            "inner_attribute_item" if spelled(node, text) == "#![cfg(test)]" => {
                match node.parent().filter(|body| body.kind() != "source_file") {
                    Some(body) => marked.push(body.parent().unwrap_or(body).byte_range()),
                    None => test = true,
                }
            }
            "attribute_item" if is_test_mark(node, text) => marked.extend(marked_item(node)),
            "token_tree" => unplaced.extend(
                declared_modules(node, text)
                    .into_iter()
                    .map(|(line, at)| (line, at, "a module declared through a macro")),
            ),
            "mod_item" if node.child_by_field_name("body").is_none() => {
                let name = node
                    .child_by_field_name("name")
                    .map_or("", |name| text_of(name, text));
                modules.push((name.to_owned(), node.start_byte()));
                let inline = node.parent().and_then(|body| body.parent());
                if inline.is_some_and(|module| module.kind() == "mod_item") {
                    let line = node.start_position().row + 1;
                    unplaced.push((
                        line,
                        node.start_byte(),
                        "a module file declared in an inline module",
                    ));
                }
            }
            _ => {}
        }
    }
    let held = |at: usize| marked.iter().any(|range| range.contains(&at));
    let mut tokens = Vec::new();
    crate::code::testing::tokens(tree.root_node(), text, &RUST_LEXICON, |token| {
        if let Token::Node(node) = token
            && !held(node.start_byte())
        {
            let spelled = text_of(node, text);
            let name = spelled.strip_prefix("r#").unwrap_or(spelled);
            tokens.push((node.start_position().row + 1, name.to_owned()));
        }
        std::ops::ControlFlow::Continue(())
    });
    Read {
        tokens,
        modules: modules
            .into_iter()
            .map(|(name, at)| (name, held(at)))
            .collect(),
        unplaced: unplaced
            .into_iter()
            .filter(|&(_, at, _)| !held(at))
            .map(|(line, _, what)| (line, what))
            .collect(),
        test,
    }
}

/// Each `mod name` among the tokens of `tree`, a token tree: its line and
/// its first byte.
fn declared_modules(tree: Node<'_>, text: &str) -> Vec<(usize, usize)> {
    let mut cursor = tree.walk();
    let tokens: Vec<Node<'_>> = tree.children(&mut cursor).collect();
    tokens
        .windows(2)
        .filter_map(|pair| match pair {
            [keyword, name] if text_of(*keyword, text) == "mod" && name.kind() == "identifier" => {
                Some((keyword.start_position().row + 1, keyword.start_byte()))
            }
            _ => None,
        })
        .collect()
}

/// `node`'s text without whitespace.
fn spelled(node: Node<'_>, text: &str) -> String {
    text_of(node, text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Whether the attribute `node` is exactly `#[test]` or `#[cfg(test)]`.
fn is_test_mark(node: Node<'_>, text: &str) -> bool {
    matches!(spelled(node, text).as_str(), "#[test]" | "#[cfg(test)]")
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

/// The files of `read` that are test code whole: under an inner
/// `#![cfg(test)]`, declared only by marked `mod` items, or in a test
/// file's modules.
fn test_files<'a>(read: &BTreeMap<&'a str, Read>) -> BTreeSet<&'a str> {
    // Each file's declarations: whether one ships it.
    let mut shipped: BTreeMap<String, bool> = BTreeMap::new();
    for (path, file) in read {
        for (name, marked) in &file.modules {
            let dir = modules_dir(path);
            for child in [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")] {
                *shipped.entry(child).or_default() |= !marked;
            }
        }
    }
    let parent = |path: &str| {
        read.keys()
            .copied()
            .find(|parent| *parent != path && modules_dir(parent) == parent_dir(path))
    };
    let mut tests: BTreeSet<&str> = read
        .iter()
        .filter(|(path, file)| file.test || shipped.get(**path) == Some(&false))
        .map(|(path, _)| *path)
        .collect();
    loop {
        let more: Vec<&str> = read
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
/// declare: `a/b` for `a/b.rs`, `a/b/mod.rs` and `a/b/lib.rs`.
fn modules_dir(path: &str) -> String {
    let stem = path.trim_end_matches(".rs");
    match stem.rsplit_once('/') {
        Some((dir, "mod" | "lib" | "main")) => dir.to_owned(),
        Some(_) | None => stem.to_owned(),
    }
}

/// The directory the module file at `path` is declared from: `a` for
/// `a/b.rs`, and for `a/b/mod.rs`.
fn parent_dir(path: &str) -> String {
    let dir = modules_dir(path);
    let module = match path.ends_with("/mod.rs") {
        true => dir.as_str(),
        false => path,
    };
    module
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
        .to_owned()
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
fn dependencies(texts: &[(String, String)]) -> Vec<String> {
    let Some((_, manifest)) = texts.iter().find(|(path, _)| path == "Cargo.toml") else {
        return Vec::new();
    };
    let manifest = crate::code::testing::index(FileKind::Toml, manifest.as_bytes()).unwrap();
    let scopes = Scopes(&manifest.scopes);
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
/// `src/code/mod.rs` one, `src/code/parse/rust/marks.rs` four.
fn module_depth(path: &str) -> usize {
    let inside = path.trim_start_matches("src/");
    let segments = inside.split('/').count();
    match inside.rsplit('/').next() {
        Some("mod.rs" | "lib.rs" | "main.rs") => segments - 1,
        _ => segments,
    }
}

/// Where a checked file is: its path from the crate's directory, and the
/// checked directory, which holds it.
#[derive(Clone, Copy)]
struct Place<'a> {
    path: &'a str,
    dir: &'a str,
}

/// The faults of one file's `tokens`, a file `depth` modules below the
/// crate root at `place`, whose dependencies are `dependencies`, against
/// `allowed`.
fn file_faults(
    tokens: &[(usize, String)],
    depth: usize,
    dependencies: &[String],
    place: Place<'_>,
    allowed: &AllowList,
) -> Vec<(usize, String)> {
    let spelled = |at: usize| tokens.get(at).map_or("", |(_, token)| token.as_str());
    let mut found = Vec::new();
    for at in 0..tokens.len() {
        let line = tokens.get(at).map_or(0, |(line, _)| *line);
        match (spelled(at), spelled(at + 1), spelled(at + 2)) {
            ("extern", "crate", _) => found.push((line, "extern crate".to_owned())),
            (name, _, _) if REFUSED_NAMES.contains(&name) => {
                found.push((line, format!("names `{name}`")));
            }
            ("macro_rules!", _, _) | ("macro_rules", "!", _) => {
                found.push((line, "macro_rules!".to_owned()));
            }
            ("#", "[" | "!", _) if names_a_path(tokens, at) => {
                found.push((line, "#[path]".to_owned()));
            }
            ("include_str", "!", _) if is_admitted_include(tokens, at, place) => {}
            (name, "!", _) if MACROS.contains(&name) => found.push((line, format!("{name}!"))),
            _ => {}
        }
        if !starts_path(tokens, at) {
            continue;
        }
        let mut next = at;
        for path in read_path(tokens, &mut next, Vec::new()) {
            if let Some(fault) = path_fault(&path, depth, dependencies, allowed) {
                found.push((line, fault));
            }
        }
    }
    found
}

/// Whether the attribute that opens at `at`, its `#`, names a `path`
/// anywhere in its brackets: `#[path = ..]` or `#[cfg_attr(.., path =
/// ..)]`.
fn names_a_path(tokens: &[(usize, String)], at: usize) -> bool {
    let spelled = |n: usize| tokens.get(n).map_or("", |(_, token)| token.as_str());
    let mut depth = 0_usize;
    for n in at + 1.. {
        match spelled(n) {
            "" => return false,
            "[" | "(" | "{" => depth += 1,
            "]" | ")" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return false;
                }
            }
            "path" if spelled(n + 1) == "=" => return true,
            _ => {}
        }
    }
    false
}

/// Whether the `include_str!` at `at` reads a file `place`'s directory
/// holds: its argument one plain string literal, a relative path with no
/// escape, which stays inside the directory once each `..` is taken.
fn is_admitted_include(tokens: &[(usize, String)], at: usize, place: Place<'_>) -> bool {
    let spelled = |n: usize| tokens.get(n).map_or("", |(_, token)| token.as_str());
    let literal = spelled(at + 3);
    let Some(relative) = literal
        .strip_prefix('"')
        .and_then(|inside| inside.strip_suffix('"'))
        .filter(|inside| !inside.is_empty() && !inside.contains(['\\', '"']))
    else {
        return false;
    };
    if spelled(at + 2) != "(" || spelled(at + 4) != ")" || relative.starts_with('/') {
        return false;
    }
    let mut segments: Vec<&str> = place.path.split('/').collect();
    segments.pop();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return false;
                }
            }
            segment => segments.push(segment),
        }
    }
    let dir_depth = place.dir.split('/').count();
    segments.len() > dir_depth && segments.join("/").starts_with(&format!("{}/", place.dir))
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
    let word = first == "$crate" || first == "::" || is_word(first);
    let continues = matches!(spelled(1), "::" | ".");
    let visibility = (spelled(1) == "(" && spelled(2) == "pub")
        || (spelled(1) == "in" && spelled(2) == "(" && spelled(3) == "pub");
    word && !continues && !visibility
}

/// A run of letters, digits and underscores: an identifier or a keyword.
fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
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
            word if word == "$crate" || is_word(word) => {
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
/// outside what `allowed` holds; `None` when it does not.
fn path_fault(
    path: &[String],
    depth: usize,
    dependencies: &[String],
    allowed: &AllowList,
) -> Option<String> {
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
            [.., "as"] => refuse(),
            [module, ..] if STD_MODULES.contains(module) => None,
            [module, item, ..] if allowed.std_items.contains(&(*module, *item)) => None,
            _ => refuse(),
        },
        "crate" | "$crate" => match rest {
            [module, ..] if allowed.modules.contains(module) => None,
            ["store", item, ..] if allowed.store_items.contains(item) => None,
            _ => refuse(),
        },
        name if allowed.crates.contains(&name) => None,
        name if rest
            .first()
            .is_some_and(|item| allowed.crate_items.contains(&(name, *item))) =>
        {
            None
        }
        name if dependencies.iter().any(|dependency| dependency == name) => refuse(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// The list the seeded module is checked against: a store value type,
    /// the items of `std` its pure file names, a parser crate and the
    /// thread pool's prelude.
    const SEEDED_LIST: AllowList = AllowList {
        modules: &["index"],
        store_items: &["Anchor"],
        std_items: &[("io", "ErrorKind"), ("sync", "Arc"), ("sync", "LazyLock")],
        crates: &["tree_sitter"],
        crate_items: &[("rayon", "prelude")],
    };

    /// Each form a path to the outside can take, one per file, and code
    /// that is pure or test code, which none of the faults is in.
    const SEEDED: &[(&str, &str)] = &[
        (
            "pure",
            "use std::collections::BTreeMap;\nuse std::io::ErrorKind;\nuse crate::store::Anchor;\nuse super::index::FileKind;\nuse tree_sitter::Node;\nfn f() -> core::fmt::Result { Ok(()) }\nconst Q: &str = include_str!(\"queries/q.scm\");\nconst R: &str = include_str!(\"./queries/../q.scm\");\nuse std::sync::{Arc, LazyLock};\nuse rayon::prelude::*;\nmod checks {\n    #![cfg(test)]\n    fn probe() { std::fs::read(\"x\"); }\n}\n",
        ),
        ("path_type", "fn f(path: &std::path::PathBuf) {}\n"),
        (
            "module_alias",
            "use std::collections as c;\nfn f() { let _ = c::BTreeMap::<u8, u8>::new(); }\n",
        ),
        ("item_alias", "use std::collections::BTreeMap as Map;\n"),
        ("sync_clock", "fn f(wait: &std::sync::Condvar) {}\n"),
        (
            "macro_definition",
            "macro_rules! call { ($v:expr, $m:ident) => { $v.$m() }; }\n",
        ),
        (
            "crate_item",
            "fn f() -> usize { rayon::current_num_threads() }\n",
        ),
        ("crate_item_glob", "use rayon::*;\n"),
        ("fs", "fn f() { let _ = std::fs::read(\"/x\"); }\n"),
        ("listed", "use std::{fs, collections};\n"),
        ("aliased", "use std::fs as disk;\n"),
        ("self_alias", "use std::{self as s};\n"),
        ("glob", "use std::*;\n"),
        ("root_alias", "use std as s;\n"),
        (
            "absolute",
            "fn f() { let _ = ::std::env::var(\"HOME\"); }\n",
        ),
        ("time", "fn f() { let _ = std::time::Instant::now(); }\n"),
        (
            "thread",
            "fn f() { let _ = std::thread::available_parallelism(); }\n",
        ),
        ("io", "fn f() { let _ = std::io::stdin(); }\n"),
        (
            "os",
            "fn f() { let _ = std::os::unix::process::parent_id(); }\n",
        ),
        ("path", "fn f() { let _ = std::path::absolute(\"x\"); }\n"),
        (
            "backtrace",
            "fn f() { let _ = std::backtrace::Backtrace::force_capture(); }\n",
        ),
        (
            "macro_argument",
            "fn f() -> String { format!(\"{:?}\", std::fs::read(\"/x\")) }\n",
        ),
        (
            "macro_body",
            "macro_rules! read { () => { std::net::TcpStream::connect(\"x\") }; }\n",
        ),
        ("dependency", "fn f() { rayon::spawn(|| ()); }\n"),
        ("crate_shell", "fn f() { crate::git::toplevel(); }\n"),
        ("crate_store", "fn f(store: &crate::store::Store) {}\n"),
        ("crate_glob", "use crate::*;\n"),
        ("crate_alias", "use crate as c;\n"),
        (
            "super_reach",
            "fn f() { super::super::console::write(); }\n",
        ),
        (
            "dollar_crate",
            "macro_rules! m { () => { $crate::sources::gather() }; }\n",
        ),
        ("extern", "extern crate std as s;\n"),
        (
            "path_attribute",
            "#[path = \"../../outside.rs\"]\nmod outside;\n",
        ),
        ("include", "const X: &str = include_str!(\"../lib.rs\");\n"),
        (
            "include_absolute",
            "const X: &str = include_str!(\"/etc/hosts\");\n",
        ),
        (
            "include_escaping",
            "const X: &str = include_str!(\"queries/../../lib.rs\");\n",
        ),
        (
            "include_computed",
            "const X: &str = include_str!(concat!(\"q\", \".scm\"));\n",
        ),
        ("include_raw", "const X: &str = include_str!(r\"q.scm\");\n"),
        (
            "include_bytes",
            "const X: &[u8] = include_bytes!(\"q.scm\");\n",
        ),
        (
            "environment",
            "const X: Option<&str> = option_env!(\"HOME\");\n",
        ),
        ("print", "fn f() { println!(\"x\"); }\n"),
        ("raw_root", "fn f() { let _ = r#std::fs::read(\"/x\"); }\n"),
        (
            "tool_attribute",
            "#[rustfmt::test]\npub(crate) fn tool() { let _ = std::fs::read(\"/x\"); }\n",
        ),
        (
            "conditional_path",
            "#[cfg_attr(all(), path = \"../outside.rs\")]\nmod outside;\n",
        ),
        (
            "test_file_shipped",
            "#[cfg(test)]\nmod hidden;\n#[cfg_attr(all(), path = \"test_file_shipped/hidden.rs\")]\nmod shipped;\n",
        ),
        (
            "inner_test_module",
            "mod hidden {\n    #![cfg(test)]\n}\npub(crate) fn leak() { let _ = std::fs::read(\"/x\"); }\n",
        ),
        (
            "macro_module",
            "macro_rules! ship { () => { mod shipped; }; }\n",
        ),
        (
            "nested_module",
            "#[cfg(test)]\nmod twin;\n#[cfg(not(test))]\nmod twin {\n    mod leak;\n}\n",
        ),
    ];

    #[test]
    fn each_path_to_the_outside_is_found_however_it_is_spelled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("Cargo.toml", "[package]\n[dependencies]\nrayon = \"1\"\n");
        write("src/lib.rs", "mod index;\n");
        // Each refused name, reached through an allowed module's own name.
        let refused: Vec<(String, String)> = REFUSED_NAMES
            .iter()
            .map(|name| {
                let text = format!(
                    "use std::collections;\nfn f() {{ let _ = collections::{name}::default(); }}\n"
                );
                (format!("refused_{}", name.to_lowercase()), text)
            })
            .collect();
        let files: Vec<(&str, &str)> = SEEDED
            .iter()
            .copied()
            .chain(
                refused
                    .iter()
                    .map(|(name, text)| (name.as_str(), text.as_str())),
            )
            .collect();
        let modules: String = files
            .iter()
            .map(|(name, _)| format!("mod {name};\n"))
            .collect();
        write(
            "src/index/mod.rs",
            &format!(
                "{modules}#[cfg(test)]\nmod tests;\n#[cfg(test)]\nfn probe() {{ std::fs::read(\"x\"); }}\n"
            ),
        );
        write("src/index/tests.rs", "use std::fs;\n");
        write("src/index/test_file_shipped/hidden.rs", "use std::fs;\n");
        write("src/index/nested_module/twin.rs", "");
        write("src/index/nested_module/twin/leak.rs", "use std::fs;\n");
        for (name, text) in &files {
            write(&format!("src/index/{name}.rs"), text);
        }
        let found = faults(root, "src/index", &SEEDED_LIST);
        let faulted: std::collections::BTreeSet<&str> = found
            .iter()
            .map(|fault| fault.split(':').next().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<String> = files
            .iter()
            .filter(|(name, _)| *name != "pure")
            .map(|(name, _)| format!("src/index/{name}.rs"))
            .collect();

        assert_eq!(
            faulted,
            expected.iter().map(String::as_str).collect(),
            "{found:#?}"
        );
    }
}
