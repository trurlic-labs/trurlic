//! What the checks read of a crate ([`Crate`]). That is its non-test code
//! as tokens and paths, each in the module it stands in, as the code
//! index's one reader of Rust paths reads them (`Reading`). It is too the
//! forms that code takes which no reading can follow, refused by every
//! check, and the dependencies its manifest states.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::Path;

use tree_sitter::Node;

use crate::code::FileKind;
use crate::code::testing::{
    End, Led, PathKind, Reading, Scopes, is_attribute, is_inline_module, joined, lead, parse,
    text_of,
};

use super::test_code::{FileMarks, Held, held_by, modules_dir, test_files};

/// The `macro_rules!` definitions non-test code may hold, each the path
/// of its file from the crate's directory, its name and why. Any other is
/// refused. What a macro's body names reaches each module that invokes
/// it unseen. A metavariable may make a path of any tokens a caller hands
/// in (`$root::x`, `$a $b x`, `use crate::{$m}`, an attribute's `$s`),
/// which no reading follows.
const DEFINED_MACROS: &[(&str, &str, &str)] = &[
    (
        "src/console.rs",
        "out",
        "`println!` for command output: the one way to stdout, as a lint makes it",
    ),
    (
        "src/console.rs",
        "diag",
        "`eprintln!` for diagnostics: the one way to stderr, as a lint makes it",
    ),
];

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

/// What the check reads of one Rust file, outside its test items.
pub(super) struct Read {
    /// Its tokens, in order.
    pub(super) tokens: Vec<Token>,
    /// The paths its tokens name, a `use` list's entries each one.
    pub(super) paths: Vec<Named>,
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

/// A path a file's non-test code names.
pub(super) struct Named {
    pub(super) line: usize,
    /// Its segments as spelled, a glob as `*` and an alias's `as` kept
    /// last (`std::fs::as` for `use std::fs as disk`).
    pub(super) segments: Vec<String>,
    /// The module its first segment stands in, an index into
    /// [`Read::modules`].
    pub(super) module: usize,
    /// Whether it is the tree of a `use` a visibility opens, among a
    /// macro's tokens or not: a re-export.
    pub(super) exported: bool,
}

impl Named {
    /// The segments from the crate root this path names, standing in
    /// `module` (segments from the crate root), when it goes into the
    /// crate: its leading `crate`, `$crate`, `self` and `super`s read from
    /// `module` as the code index reads them (`lead`), then its other
    /// segments but `self`. `None` for a path that starts with a name or
    /// leads above the root, and for one only `self`s lead, which stays in
    /// its module (`self`, `self::x`).
    pub(super) fn crate_path<'a>(&'a self, module: &'a [String]) -> Option<Vec<&'a str>> {
        // The module a leading segment leads to: the first `within`
        // segments of `module`.
        let mut within = None;
        let mut leaves = false;
        let mut rest = self.segments.iter().map(String::as_str).peekable();
        while let Some(segment) = rest.peek() {
            let from = within.unwrap_or(module.len());
            match lead(segment, from, 0, |from: usize| from.checked_sub(1)) {
                Led::Module(to) => within = Some(to),
                Led::AboveRoot => return None,
                Led::Name => break,
            }
            leaves |= *segment != "self";
            rest.next();
        }
        let above = module
            .get(..within.filter(|_| leaves)?)
            .unwrap_or_default()
            .iter();
        let rest = rest.filter(|segment| *segment != "self");
        Some(above.map(String::as_str).chain(rest).collect())
    }
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

/// What the check reads of `text`, the file at `path`, and what decides
/// whether it is test code whole.
fn read(path: &str, text: &str) -> (Read, FileMarks) {
    let tree = parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
    let marks = marks(tree.root_node(), text);
    let held = |at: usize| marks.held.iter().any(|range| range.contains(&at));
    let reading = Reading::of(tree.root_node(), text);
    let mut refused: Vec<(usize, &'static str)> = marks
        .refused
        .iter()
        .filter(|(_, at, _)| !held(*at))
        .map(|&(line, _, what)| (line, what))
        .collect();
    refused.extend(marks.paths.iter().map(|&line| (line, PATH_ATTRIBUTE)));
    refused.extend(macro_definitions(path, &marks, held));
    if tree.root_node().has_error() {
        refused.push((1, "a file the grammar does not parse whole"));
    }
    let mut file = viewed(&reading, module_path(path), text, held);
    refused.extend(refused_tokens(&file.tokens));
    refused.sort_unstable();
    file.refused = refused;
    let declared = marks.declared.into_iter();
    let file_marks = FileMarks {
        declared: declared.map(|(name, at)| (name, held(at))).collect(),
        test: marks.test,
    };
    (file, file_marks)
}

/// `reading`, of `text`, as the checks view it: the tokens and paths
/// outside the bytes `held` marks, each in its module, `own` the file's.
fn viewed(
    reading: &Reading<'_>,
    own: Vec<String>,
    text: &str,
    held: impl Fn(usize) -> bool,
) -> Read {
    let mut modules = vec![own.clone()];
    for inline in &reading.modules {
        let mut names = Vec::new();
        let mut item = Some(*inline);
        while let Some(node) = item {
            if is_inline_module(node) {
                let name = node.child_by_field_name("name");
                names.push(name.map_or("", |name| text_of(name, text)).to_owned());
            }
            item = node.parent();
        }
        names.reverse();
        modules.push(own.iter().cloned().chain(names).collect());
    }
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let line = |at: usize| starts.partition_point(|start| *start <= at);
    let module = |inline: Option<usize>| inline.map_or(0, |inline| inline + 1);
    let tokens = reading.tokens.iter().filter(|token| !held(token.start));
    let tokens = tokens.map(|token| Token {
        line: line(token.start),
        text: token.text.to_string(),
        module: module(token.module),
    });
    let tokens = tokens.collect();
    let paths = reading.paths();
    let mut named = Vec::new();
    for path in &paths.named {
        let Some(first) = reading.tokens.get(path.first).filter(|t| !held(t.start)) else {
            continue;
        };
        let mut segments = Vec::new();
        let mut at = Some(path.last);
        while let Some(token) = at {
            segments.push(reading.tokens[token].text.to_string());
            at = paths.before(token);
        }
        segments.reverse();
        match path.end {
            End::Path => {}
            End::Glob => segments.push("*".to_owned()),
            End::Alias(_) => segments.push("as".to_owned()),
        }
        let exported = PathKind::InMacro { exported: true };
        named.push(Named {
            line: line(first.start),
            segments,
            module: module(first.module),
            exported: path.kind == PathKind::Reexport || path.kind == exported,
        });
    }
    Read {
        tokens,
        paths: named,
        modules,
        refused: Vec::new(),
    }
}

/// What one pass over a file's tree finds, test items included: the
/// bytes of each item a test mark holds, each `mod name;` with its first
/// byte, each form the check cannot read through (line, first byte,
/// what), the line of each `#[path]`, each `macro_rules!` definition, the
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

/// A `macro_rules!` definition: its line, its name, its bytes, and the
/// bytes of the block or file it stands in, the textual scope it opens in.
struct Macro {
    line: usize,
    name: String,
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
        if is_attribute(node, text) {
            let unread = unread_in_attribute(node, text).into_iter();
            marks
                .refused
                .extend(unread.map(|what| (line, node.start_byte(), what)));
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
                name: node
                    .child_by_field_name("name")
                    .map_or("", |name| text_of(name, text))
                    .to_owned(),
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

/// What in `attribute`, one whose strings the reading reads for paths,
/// the reading cannot follow: `macro_use`, which brings a module's macros
/// into scope with no path, and a string with a `\x` or `\u` escape,
/// which may spell any path.
fn unread_in_attribute(attribute: Node<'_>, text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut pending = vec![attribute];
    while let Some(node) = pending.pop() {
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
        let spelled = text_of(node, text);
        if node.kind() == "identifier" && spelled == "macro_use" {
            found.push("#[macro_use]");
        }
        let whole = joined(spelled);
        if node.kind().ends_with("string_literal")
            && (whole.contains("\\x") || whole.contains("\\u"))
        {
            found.push("an escape in an attribute's string");
        }
    }
    found
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

/// The line and what of each `macro_rules!` definition outside test
/// items in the file at `path` that [`DEFINED_MACROS`] does not admit. An
/// admitted one is refused too where a module outside test items is
/// declared after it in its textual scope, whose code may invoke it with
/// no path.
fn macro_definitions(
    path: &str,
    marks: &Marks,
    held: impl Fn(usize) -> bool,
) -> Vec<(usize, &'static str)> {
    let listed = |name: &str| DEFINED_MACROS.iter().any(|m| (m.0, m.1) == (path, name));
    let scoped = |definition: &Macro| {
        let scope = &definition.scope;
        let after = |at: usize| !held(at) && at >= definition.bytes.end && scope.contains(&at);
        marks.modules.iter().any(|&at| after(at))
    };
    let refused = marks.macros.iter().filter(|m| !held(m.bytes.start));
    let refused = refused.filter_map(|definition| match listed(&definition.name) {
        false => Some((definition.line, "a macro_rules! no list admits")),
        true if scoped(definition) => Some((
            definition.line,
            "a module in the textual scope of a macro_rules!",
        )),
        true => None,
    });
    refused.collect()
}

/// The line and what of each form among `tokens` the check cannot read
/// through. That is `include!`, whose file it does not read, and `extern
/// crate`, which names a crate, or the crate itself, by a name no list
/// holds. So is a `macro_rules!` among a macro's tokens, which no tree
/// holds as a definition: the one the tree holds is the single token
/// `macro_rules!`, judged by `macro_definitions`.
fn refused_tokens(tokens: &[Token]) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let refused = match (token.text.as_str(), text_at(tokens, at + 1)) {
            ("include", "!") => "include!",
            ("extern", "crate") => "extern crate",
            ("macro_rules", "!") => "a macro_rules! no list admits",
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
fn module_path(path: &str) -> Vec<String> {
    modules_dir(path)
        .strip_prefix("src")
        .unwrap_or_default()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}
