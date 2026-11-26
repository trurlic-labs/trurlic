//! What the checks read of a crate: every Rust file under `src`, each
//! with its tokens outside test code and the module each token stands
//! in, the files that are test code whole, the dependencies its manifest
//! states, and each form of its non-test code the reading cannot follow,
//! which every check refuses.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Range};
use std::path::Path;

use tree_sitter::Node;

use crate::code::FileKind;
use crate::code::testing::{RUST_LEXICON, Scopes, Token as Event, is_comment, parse, text_of};

/// A crate as the checks read it.
pub(crate) struct Crate {
    /// What is read of each Rust file that is not test code whole, by its
    /// path from the crate's directory.
    files: BTreeMap<String, Read>,
    /// The crate names of the dependencies its manifest states, as a path
    /// spells them.
    pub(super) dependencies: Vec<String>,
}

/// What the check reads of one Rust file.
pub(super) struct Read {
    /// Its tokens outside test items, in order.
    pub(super) tokens: Vec<Token>,
    /// The path from the crate root of the file's module, then of each
    /// inline module in it, in the order the tokens enter them.
    pub(super) modules: Vec<Vec<String>>,
    /// Each `mod name;` it declares, and whether a test mark holds it.
    declared: Vec<(String, bool)>,
    /// Each form outside test items the check cannot read through: its
    /// line and what it is.
    refused: Vec<(usize, &'static str)>,
    /// Whether an inner `#![cfg(test)]` at its root makes it test code
    /// whole.
    test: bool,
}

/// One token of a file's non-test code.
pub(super) struct Token {
    pub(super) line: usize,
    /// Its text, a raw identifier as its name.
    pub(super) text: String,
    /// The module it stands in, an index into [`Read::modules`].
    pub(super) module: usize,
}

impl Crate {
    /// Every Rust file under `root/src`, and `root/Cargo.toml`, read;
    /// `root` is a crate's directory.
    pub(crate) fn read(root: &Path) -> Self {
        let texts = sources(root);
        let dependencies = dependencies(&texts);
        let read: BTreeMap<String, Read> = texts
            .iter()
            .filter(|(path, _)| path.ends_with(".rs"))
            .map(|(path, text)| (path.clone(), read(path, text)))
            .collect();
        let tests: BTreeSet<String> = test_files(&read).into_iter().map(str::to_owned).collect();
        let files = read
            .into_iter()
            .filter(|(path, _)| !tests.contains(path))
            .collect();
        Self {
            files,
            dependencies,
        }
    }

    /// Each form of the crate's non-test code the reading cannot follow,
    /// in any file, as `file:line: what`, files sorted: every check
    /// reports them, since one hides code or a module from all of them.
    pub(super) fn refusals(&self) -> Vec<String> {
        self.files
            .iter()
            .flat_map(|(path, file)| {
                file.refused
                    .iter()
                    .map(move |(line, what)| format!("{path}:{line}: {what}"))
            })
            .collect()
    }

    /// The files of the module `module` and of the modules in it, a path
    /// from the crate root (`store::model`), by path.
    pub(super) fn files_of<'a>(
        &'a self,
        module: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a Read)> {
        self.files.iter().filter_map(move |(path, file)| {
            let own: Vec<&str> = file
                .modules
                .first()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(String::as_str)
                .collect();
            is_within(&own, module).then_some((path.as_str(), file))
        })
    }

    /// The modules of the crate root that hold non-test code, by name.
    pub(crate) fn root_modules(&self) -> BTreeSet<&str> {
        self.files
            .values()
            .filter_map(|file| file.modules.first()?.first())
            .map(String::as_str)
            .collect()
    }
}

/// The text of the token at `at` of `tokens`; empty past their end.
pub(super) fn text_at(tokens: &[Token], at: usize) -> &str {
    tokens.get(at).map_or("", |token| token.text.as_str())
}

/// Whether `path`, segments from the crate root, is in `module`, a path
/// from the crate root such as `code::index`.
pub(super) fn is_within(path: &[&str], module: &str) -> bool {
    let module: Vec<&str> = module.split("::").collect();
    path.starts_with(&module)
}

/// A run of letters, digits and underscores: an identifier or a keyword.
pub(super) fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// What the check reads of `text`, the file at `path`.
fn read(path: &str, text: &str) -> Read {
    let tree = parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
    let marks = marks(tree.root_node(), text);
    let held = |at: usize| marks.held.iter().any(|range| range.contains(&at));
    let own = module_path(path).into_iter().map(str::to_owned).collect();
    let walked = walk(tree.root_node(), text, own, held);
    let mut refused: Vec<(usize, &'static str)> = marks
        .refused
        .iter()
        .filter(|(_, at, _)| !held(*at))
        .map(|&(line, _, what)| (line, what))
        .collect();
    refused.extend(macro_scopes(&marks, held));
    refused.extend(walked.refused);
    refused.extend(refused_tokens(&walked.tokens));
    if tree.root_node().has_error() {
        refused.push((1, "a file the grammar does not parse whole"));
    }
    refused.sort_unstable();
    Read {
        tokens: walked.tokens,
        modules: walked.modules,
        declared: marks
            .declared
            .into_iter()
            .map(|(name, at)| (name, held(at)))
            .collect(),
        refused,
        test: marks.test,
    }
}

/// What one pass over a file's tree finds, test items included: the
/// bytes of each item a test mark holds, each `mod name;` with its first
/// byte, each module whose file the check cannot place (line, first byte,
/// why), each `macro_rules!` definition, the first byte of each `mod`
/// item, and whether the file is test code whole.
#[derive(Default)]
struct Marks {
    held: Vec<Range<usize>>,
    declared: Vec<(String, usize)>,
    refused: Vec<(usize, usize, &'static str)>,
    macros: Vec<Macro>,
    modules: Vec<usize>,
    test: bool,
}

/// A `macro_rules!` definition: its line, its bytes, and the bytes of the
/// block or file it stands in, the textual scope it opens in.
struct Macro {
    line: usize,
    bytes: Range<usize>,
    scope: Range<usize>,
}

/// The marks of `root`, a file's tree. A module file is placed by the
/// file that declares it, so a `mod name;` inside an inline module, whose
/// file is under that module's directory, is refused rather than placed.
fn marks(root: Node<'_>, text: &str) -> Marks {
    let mut marks = Marks::default();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        let line = node.start_position().row + 1;
        match node.kind() {
            "inner_attribute_item" if compact(node, text) == "#![cfg(test)]" => {
                match node.parent().filter(|body| body.kind() != "source_file") {
                    Some(body) => marks.held.push(body.parent().unwrap_or(body).byte_range()),
                    None => marks.test = true,
                }
            }
            "attribute_item" if is_test_mark(node, text) => marks.held.extend(marked_item(node)),
            "token_tree" => marks.refused.extend(
                declared_modules(node, text)
                    .into_iter()
                    .map(|(line, at)| (line, at, "a module declared through a macro")),
            ),
            "macro_definition" => marks.macros.push(Macro {
                line,
                bytes: node.byte_range(),
                scope: node.parent().map_or(0..0, |scope| scope.byte_range()),
            }),
            "mod_item" => {
                marks.modules.push(node.start_byte());
                if node.child_by_field_name("body").is_none() {
                    declared(node, text, &mut marks);
                }
            }
            _ => {}
        }
    }
    marks
}

/// Records `node`, a `mod name;` item, in `marks`.
fn declared(node: Node<'_>, text: &str, marks: &mut Marks) {
    let name = node
        .child_by_field_name("name")
        .map_or("", |name| text_of(name, text));
    marks.declared.push((name.to_owned(), node.start_byte()));
    let inline = node.parent().and_then(|body| body.parent());
    if inline.is_some_and(|module| module.kind() == "mod_item") {
        let line = node.start_position().row + 1;
        let why = "a module file declared in an inline module";
        marks.refused.push((line, node.start_byte(), why));
    }
}

/// The line of each `macro_rules!` definition outside test items after
/// which a module outside test items is declared in the definition's
/// textual scope: the module's code may invoke it with no path, so a
/// path its body spells counts against the module unseen.
fn macro_scopes(marks: &Marks, held: impl Fn(usize) -> bool) -> Vec<(usize, &'static str)> {
    marks
        .macros
        .iter()
        .filter(|definition| !held(definition.bytes.start))
        .filter(|definition| {
            marks.modules.iter().any(|&at| {
                !held(at) && at >= definition.bytes.end && definition.scope.contains(&at)
            })
        })
        .map(|definition| {
            (
                definition.line,
                "a module in the textual scope of a macro_rules!",
            )
        })
        .collect()
}

/// What a walk of a file's tokens yields: [`Read::tokens`],
/// [`Read::modules`], and each string of an attribute that holds an
/// escape.
struct Walked {
    tokens: Vec<Token>,
    modules: Vec<Vec<String>>,
    refused: Vec<(usize, &'static str)>,
}

/// The tokens of `root`, a file's tree, outside the bytes `held` marks,
/// each with the module it stands in, `own` the file's. A string inside
/// an attribute that spells a path of two segments or more is read as
/// that path's tokens ([`attribute_path`]).
fn walk(root: Node<'_>, text: &str, own: Vec<String>, held: impl Fn(usize) -> bool) -> Walked {
    let mut walked = Walked {
        tokens: Vec::new(),
        modules: vec![own],
        refused: Vec::new(),
    };
    let mut open: Vec<usize> = vec![0];
    let mut attributes = 0_usize;
    crate::code::testing::tokens(root, text, &RUST_LEXICON, |event| {
        match event {
            Event::Enter(node) if is_attribute(node) => attributes += 1,
            Event::Leave(node) if is_attribute(node) => attributes = attributes.saturating_sub(1),
            Event::Enter(node) if is_inline_module(node) => {
                let name = node
                    .child_by_field_name("name")
                    .map_or("", |name| text_of(name, text));
                let above = open.last().and_then(|at| walked.modules.get(*at));
                let mut path = above.cloned().unwrap_or_default();
                path.push(name.to_owned());
                open.push(walked.modules.len());
                walked.modules.push(path);
            }
            Event::Leave(node) if is_inline_module(node) => {
                open.pop();
            }
            Event::Node(node) if !held(node.start_byte()) => {
                let module = *open.last().unwrap_or(&0);
                push_token(node, text, (module, attributes > 0), &mut walked);
            }
            // A held token, a gap, a run, a hole, and any other node
            // entered or left.
            _ => {}
        }
        ControlFlow::Continue(())
    });
    walked
}

/// Pushes the token `node`, standing in the module `module` and inside an
/// attribute or not, onto `walked`.
fn push_token(
    node: Node<'_>,
    text: &str,
    (module, in_attribute): (usize, bool),
    walked: &mut Walked,
) {
    let line = node.start_position().row + 1;
    let spelled = text_of(node, text);
    let token = |text: &str| Token {
        line,
        text: text.to_owned(),
        module,
    };
    if in_attribute && node.kind().ends_with("string_literal") {
        match attribute_path(spelled) {
            Literal::Path(segments) => {
                walked.tokens.extend(segments.into_iter().map(token));
                return;
            }
            Literal::Escaped => walked
                .refused
                .push((line, "an escape in an attribute's string")),
            Literal::Text => {}
        }
    }
    let name = match node.kind() {
        "identifier" => spelled.strip_prefix("r#").unwrap_or(spelled),
        _ => spelled,
    };
    walked.tokens.push(token(name));
}

/// What a string literal inside an attribute is read as.
enum Literal<'t> {
    /// The tokens of the path it spells (`crate`, `::`, `store`, ...).
    Path(Vec<&'t str>),
    /// It holds a `\x` or `\u` escape, which may spell any path.
    Escaped,
    /// Anything else: a message, a name, a single word. A single word
    /// names an item in scope, which a `use` or a definition the check
    /// reads accounts for.
    Text,
}

/// What `literal`, a string literal's token inside an attribute
/// (`"crate::x::f"`, `r#"super::f"#`), is read as. A path of two segments
/// or more is read as a path in code is (`#[serde(with = "...")]`,
/// `default = "..."`, `try_from = "..."`).
fn attribute_path(literal: &str) -> Literal<'_> {
    let raw = literal.strip_prefix('r').map(|rest| rest.trim_matches('#'));
    let Some(inside) = raw
        .unwrap_or(literal)
        .strip_prefix('"')
        .and_then(|inside| inside.strip_suffix('"'))
    else {
        return Literal::Text;
    };
    if raw.is_none() && (inside.contains("\\x") || inside.contains("\\u")) {
        return Literal::Escaped;
    }
    let (absolute, relative) = match inside.strip_prefix("::") {
        Some(rest) => (true, rest),
        None => (false, inside),
    };
    let segments: Vec<&str> = relative.split("::").collect();
    let is_path = segments.len() > 1
        && segments
            .iter()
            .enumerate()
            .all(|(at, segment)| is_word(segment) || (at == 0 && *segment == "$crate"));
    if !is_path {
        return Literal::Text;
    }
    let mut tokens = Vec::new();
    for (at, segment) in segments.into_iter().enumerate() {
        if at > 0 || absolute {
            tokens.push("::");
        }
        tokens.push(segment);
    }
    Literal::Path(tokens)
}

/// Whether `node` is an attribute, outer or inner.
fn is_attribute(node: Node<'_>) -> bool {
    matches!(node.kind(), "attribute_item" | "inner_attribute_item")
}

/// Whether `node` is a `mod name { .. }` item.
fn is_inline_module(node: Node<'_>) -> bool {
    node.kind() == "mod_item" && node.child_by_field_name("body").is_some()
}

/// The line and what of each form among `tokens` the check cannot read
/// through: an attribute that names a module's file, `include!`, whose
/// file it does not read, `macro_use`, which brings a module's macros
/// into scope with no path, and `extern crate`, which names a crate, or
/// the crate itself, by a name no list holds.
fn refused_tokens(tokens: &[Token]) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let refused = match (token.text.as_str(), text_at(tokens, at + 1)) {
            ("#", "[" | "!") if names_a_path(tokens, at) => "#[path]",
            ("include", "!") => "include!",
            ("macro_use", _) => "#[macro_use]",
            ("extern", "crate") => "extern crate",
            _ => continue,
        };
        found.push((token.line, refused));
    }
    found
}

/// Whether the attribute that opens at `at`, its `#`, names a `path`
/// anywhere in its brackets: `#[path = ..]` or `#[cfg_attr(.., path =
/// ..)]`.
fn names_a_path(tokens: &[Token], at: usize) -> bool {
    let mut depth = 0_usize;
    for n in at + 1.. {
        match text_at(tokens, n) {
            "" => return false,
            "[" | "(" | "{" => depth += 1,
            "]" | ")" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return false;
                }
            }
            "path" if text_at(tokens, n + 1) == "=" => return true,
            _ => {}
        }
    }
    false
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

/// The files of `read` that are test code whole: under an inner
/// `#![cfg(test)]`, declared only by marked `mod` items, or in a test
/// file's modules.
fn test_files(read: &BTreeMap<String, Read>) -> BTreeSet<&str> {
    // Each file's declarations: whether one ships it.
    let mut shipped: BTreeMap<String, bool> = BTreeMap::new();
    for (path, file) in read {
        for (name, marked) in &file.declared {
            let dir = modules_dir(path);
            for child in [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")] {
                *shipped.entry(child).or_default() |= !marked;
            }
        }
    }
    let parent = |path: &str| {
        read.keys()
            .map(String::as_str)
            .find(|parent| *parent != path && modules_dir(parent) == parent_dir(path))
    };
    let mut tests: BTreeSet<&str> = read
        .iter()
        .filter(|(path, file)| file.test || shipped.get(path.as_str()) == Some(&false))
        .map(|(path, _)| path.as_str())
        .collect();
    loop {
        let more: Vec<&str> = read
            .keys()
            .map(String::as_str)
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

/// The path from the crate root of the module the file at `path` is:
/// `code` for `src/code/mod.rs`, `code::parse::rust::marks` for
/// `src/code/parse/rust/marks.rs`, none for `src/lib.rs`.
fn module_path(path: &str) -> Vec<&str> {
    let inside = path.trim_start_matches("src/").trim_end_matches(".rs");
    let mut segments: Vec<&str> = inside.split('/').collect();
    if matches!(segments.last(), Some(&("mod" | "lib" | "main"))) {
        segments.pop();
    }
    segments
}
