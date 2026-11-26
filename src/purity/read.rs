//! What the checks read of a crate ([`Crate`]): its non-test code as
//! tokens, each in the module it stands in; the forms that code takes
//! which the reading cannot follow, refused by every check; the
//! dependencies its manifest states.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Range};
use std::path::Path;

use tree_sitter::Node;

use crate::code::FileKind;
use crate::code::testing::{RUST_LEXICON, Scopes, Token as Event, parse, text_of};

use super::test_code::{FileMarks, Held, held_by, modules_dir, test_files};

/// What a `#[path]` is refused as. It is refused in test code too: the
/// file it names is read as the module its own path spells.
const PATH_ATTRIBUTE: &str = "#[path]";

/// A crate as the checks read it.
pub(crate) struct Crate {
    /// What is read of each Rust file that is not test code whole, by its
    /// path from the crate's directory.
    pub(super) files: BTreeMap<String, Read>,
    /// Each form of the crate's code the reading cannot follow, as
    /// `file:line: what`, files sorted: every check reports them, since
    /// one hides code or a module from all of them.
    pub(super) refused: Vec<String>,
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
    /// Each form outside test items the check cannot read through, and
    /// each `#[path]`: its line and what it is.
    refused: Vec<(usize, &'static str)>,
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
        let mut files = BTreeMap::new();
        let mut marks = BTreeMap::new();
        for (path, text) in texts.iter().filter(|(path, _)| path.ends_with(".rs")) {
            let (file, file_marks) = read(path, text);
            files.insert(path.clone(), file);
            marks.insert(path.as_str(), file_marks);
        }
        let tests = test_files(&marks);
        let refused = files
            .iter()
            .flat_map(|(path, file)| {
                let test = tests.contains(path.as_str());
                file.refused
                    .iter()
                    .filter(move |(_, what)| !test || *what == PATH_ATTRIBUTE)
                    .map(move |(line, what)| format!("{path}:{line}: {what}"))
            })
            .collect();
        files.retain(|path, _| !tests.contains(path.as_str()));
        Self {
            files,
            refused,
            dependencies,
        }
    }

    /// The files of the module `module` and of the modules in it, a path
    /// from the crate root (`store::model`), by path.
    pub(super) fn files_of<'a>(
        &'a self,
        module: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a Read)> {
        self.files
            .iter()
            .filter(move |(_, file)| {
                file.modules
                    .first()
                    .is_some_and(|own| is_within(own, module))
            })
            .map(|(path, file)| (path.as_str(), file))
    }

    /// The modules of the crate root that hold non-test code, by name,
    /// those inline in its root included.
    pub(crate) fn root_modules(&self) -> BTreeSet<&str> {
        self.files
            .values()
            .flat_map(|file| {
                let modules = &file.modules;
                let first = |token: &Token| modules.get(token.module)?.first();
                file.tokens.iter().filter_map(first)
            })
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
pub(super) fn is_within(path: &[impl AsRef<str>], module: &str) -> bool {
    let mut segments = path.iter().map(AsRef::as_ref);
    module.split("::").all(|part| segments.next() == Some(part))
}

/// A run of letters, digits and underscores: an identifier or a keyword.
pub(super) fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// What the check reads of `text`, the file at `path`, and what decides
/// whether it is test code whole.
fn read(path: &str, text: &str) -> (Read, FileMarks) {
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
    refused.extend(marks.paths.iter().map(|&line| (line, PATH_ATTRIBUTE)));
    refused.extend(macro_scopes(&marks, held));
    refused.extend(walked.refused);
    refused.extend(refused_tokens(&walked.tokens));
    if tree.root_node().has_error() {
        refused.push((1, "a file the grammar does not parse whole"));
    }
    refused.sort_unstable();
    let file = Read {
        tokens: walked.tokens,
        modules: walked.modules,
        refused,
    };
    let declared = marks.declared.into_iter();
    let file_marks = FileMarks {
        declared: declared.map(|(name, at)| (name, held(at))).collect(),
        test: marks.test,
    };
    (file, file_marks)
}

/// What one pass over a file's tree finds, test items included: the
/// bytes of each item a test mark holds, each `mod name;` with its first
/// byte, each module whose file the check cannot place (line, first byte,
/// why), the line of each `#[path]`, each `macro_rules!` definition, the
/// first byte of each `mod` item, and whether the file is test code whole.
#[derive(Default)]
struct Marks {
    held: Vec<Range<usize>>,
    declared: Vec<(String, usize)>,
    refused: Vec<(usize, usize, &'static str)>,
    paths: Vec<usize>,
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
        match held_by(node, text) {
            Some(Held::Item(bytes)) => marks.held.push(bytes),
            Some(Held::File) => marks.test = true,
            None => {}
        }
        match node.kind() {
            "attribute_item" | "inner_attribute_item" if names_a_path(node, text) => {
                marks.paths.push(line);
            }
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
/// [`Read::modules`], and each form of an attribute it refuses
/// ([`push_token`]).
struct Walked {
    tokens: Vec<Token>,
    modules: Vec<Vec<String>>,
    refused: Vec<(usize, &'static str)>,
}

/// The tokens of `root`, a file's tree, outside the bytes `held` marks,
/// each with the module it stands in, `own` the file's.
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
            Event::Enter(node) if is_attribute(node, text) => attributes += 1,
            Event::Leave(node) if is_attribute(node, text) => {
                attributes = attributes.saturating_sub(1);
            }
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
/// attribute or not, onto `walked`. Inside an attribute, a string is
/// read for the paths it spells ([`spelled_paths`]) and refused when it
/// holds a `\x` or `\u` escape, which may spell any of them; `macro_use` is
/// refused, as it brings a module's macros into scope with no path.
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
    let name = match node.kind() {
        "identifier" => spelled.strip_prefix("r#").unwrap_or(spelled),
        _ => spelled,
    };
    if in_attribute && node.kind().ends_with("string_literal") {
        // A line's closing `\` drops the newline and the next line's
        // leading whitespace.
        let lines: Vec<&str> = spelled.split("\\\n").map(str::trim_start).collect();
        let joined = lines.concat();
        if joined.contains("\\x") || joined.contains("\\u") {
            let why = "an escape in an attribute's string";
            walked.refused.push((line, why));
        }
        walked
            .tokens
            .extend(spelled_paths(&joined).into_iter().map(token));
    } else if in_attribute && name == "macro_use" {
        walked.refused.push((line, "#[macro_use]"));
    }
    walked.tokens.push(token(name));
}

/// The tokens of each path of two segments or more that `literal`, a
/// string literal, spells anywhere in it: segments joined by `::`, with
/// or without whitespace around it (`"crate::x::f"`, `"Vec<super::T>"`,
/// `"T: crate :: Bound"`). A single word names an item in scope, which a
/// `use` or a definition the check reads accounts for; no macro puts
/// `$crate` into a string. A leading `::` is left out, as a path in code
/// is read without it.
fn spelled_paths(literal: &str) -> Vec<&str> {
    let lexemes = lexemes(literal);
    let is_segment = |at: usize| lexemes.get(at).is_some_and(|lexeme| is_word(lexeme));
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < lexemes.len() {
        let mut end = at + 1;
        while is_segment(at) && lexemes.get(end) == Some(&"::") && is_segment(end + 1) {
            end += 2;
        }
        tokens.extend(
            lexemes
                .get(at..end)
                .filter(|run| run.len() > 1)
                .unwrap_or_default(),
        );
        at = end;
    }
    tokens
}

/// `text` cut into words, `::` and single characters, whitespace dropped.
fn lexemes(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let word = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let length = match word {
            0 if rest.starts_with("::") => 2,
            0 => first.len_utf8(),
            word => word,
        };
        let (lexeme, after) = rest.split_at(length);
        if !lexeme.trim().is_empty() {
            found.push(lexeme);
        }
        rest = after;
    }
    found
}

/// Whether `node` is an attribute, outer or inner, other than `doc`,
/// whose strings are prose.
fn is_attribute(node: Node<'_>, text: &str) -> bool {
    let doc = node
        .named_child(0)
        .and_then(|attribute| attribute.named_child(0))
        .is_some_and(|path| text_of(path, text) == "doc");
    matches!(node.kind(), "attribute_item" | "inner_attribute_item") && !doc
}

/// Whether `node` is a `mod name { .. }` item.
fn is_inline_module(node: Node<'_>) -> bool {
    node.kind() == "mod_item" && node.child_by_field_name("body").is_some()
}

/// The line and what of each form among `tokens` the check cannot read
/// through: `include!`, whose file it does not read, and `extern crate`,
/// which names a crate, or the crate itself, by a name no list holds.
fn refused_tokens(tokens: &[Token]) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let refused = match (token.text.as_str(), text_at(tokens, at + 1)) {
            ("include", "!") => "include!",
            ("extern", "crate") => "extern crate",
            _ => continue,
        };
        found.push((token.line, refused));
    }
    found
}

/// Whether the attribute `node` names a `path` anywhere in its brackets:
/// `#[path = ..]` or `#[cfg_attr(.., path = ..)]`.
fn names_a_path(node: Node<'_>, text: &str) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        let assigned = node.next_sibling().is_some_and(|next| next.kind() == "=");
        if assigned && text_of(node, text) == "path" {
            return true;
        }
    }
    false
}

/// Each `mod name` among the tokens of `tree`, a token tree, the name a
/// macro's metavariable or not: its line and its first byte.
fn declared_modules(tree: Node<'_>, text: &str) -> Vec<(usize, usize)> {
    let mut cursor = tree.walk();
    let tokens: Vec<Node<'_>> = tree.children(&mut cursor).collect();
    tokens
        .windows(2)
        .filter_map(|pair| match pair {
            [keyword, name]
                if text_of(*keyword, text) == "mod"
                    && matches!(name.kind(), "identifier" | "metavariable") =>
            {
                Some((keyword.start_position().row + 1, keyword.start_byte()))
            }
            _ => None,
        })
        .collect()
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
    modules_dir(path)
        .strip_prefix("src")
        .unwrap_or_default()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}
