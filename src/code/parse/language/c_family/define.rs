//! A `#define` as the blank rule reads it, and the lexical pass of a C or
//! C++ file that gives its `#define`s and the names it writes
//! ([`lexed`]), cached by content. Each `#define`'s body is read up to
//! [`MAX_MACRO_BODY_TOKENS`] tokens, so the pass is linear in its bytes.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::OnceLock;

use super::lex::{Item, lex};

/// The tokens of a `#define`'s body read to classify it; a longer body is
/// no attribute.
pub(in crate::code) const MAX_MACRO_BODY_TOKENS: usize = 64;

/// The words a declaration may hold that state no name: a body of them
/// alone is an attribute.
const SPECIFIERS: [&str; 32] = [
    STATIC,
    "inline",
    "extern",
    "const",
    "constexpr",
    "consteval",
    "constinit",
    "volatile",
    "_Atomic",
    "register",
    "thread_local",
    "_Thread_local",
    "__thread",
    "noexcept",
    "explicit",
    "virtual",
    "override",
    "final",
    "mutable",
    "__inline",
    "__inline__",
    "__forceinline",
    "_Noreturn",
    "noreturn",
    "restrict",
    "__restrict",
    "__restrict__",
    "__cdecl",
    "__stdcall",
    "__fastcall",
    "__vectorcall",
    "__extension__",
];

/// The specifier that makes a definition private, which a blanked macro
/// that holds it marks.
const STATIC: &str = "static";

/// The attribute forms a compiler knows, each a word and its parenthesized
/// arguments.
const ATTRIBUTES: [&str; 9] = [
    "__attribute__",
    "__attribute",
    "__declspec",
    "alignas",
    "_Alignas",
    "__asm__",
    "__asm",
    "_Pragma",
    "__pragma",
];

/// The extensions of a C or C++ header, whose `#define`s reach the files
/// that include it; a source file's reach no other file.
const HEADER_EXTENSIONS: [&str; 3] = [".h", ".hh", ".hpp"];

/// What a `#define`'s body is, as the blank rule reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Body {
    /// Specifiers and attribute forms, and these names: written alone, or
    /// called with arguments; whether it is empty, and whether it holds
    /// `static`.
    Attribute {
        names: Vec<String>,
        calls: Vec<String>,
        empty: bool,
        holds_static: bool,
    },
    /// Only `namespace x {` and `extern "C" {` openings.
    Opens,
    /// Only closing braces.
    Closes,
    /// Anything else, which no rule blanks.
    Code,
}

/// One `#define` of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Define {
    pub(super) name: String,
    pub(super) function_like: bool,
    pub(super) body: Body,
}

/// The `#define`s of one C or C++ file, as the blank sets are made from
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Defines(pub(super) Vec<Define>);

/// What the lexical pass of one C or C++ file gives: its `#define`s, and
/// the names it writes outside every directive, comment and literal,
/// which are all its blank can touch: sorted, each once, a line break
/// after each, one text so that a cache read allocates it once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Lexed {
    pub(super) defines: Defines,
    pub(super) spelled: String,
}

impl Lexed {
    /// Its `#define`s, which a header gives the tree's set.
    pub(crate) fn into_defines(self) -> Defines {
        self.defines
    }

    /// The names it writes.
    pub(super) fn spelled(&self) -> impl Iterator<Item = &str> {
        self.spelled.lines()
    }
}

/// Whether the file at `path` is a C or C++ header, whose `#define`s
/// reach the files of its tree (`set.rs`).
pub(crate) fn is_header(path: &str) -> bool {
    HEADER_EXTENSIONS
        .iter()
        .any(|extension| path.ends_with(extension))
}

/// The lexical pass over `bytes`, a C or C++ file; nothing of bytes that
/// are not UTF-8, which no index reads either.
pub(crate) fn lexed(bytes: &[u8]) -> Lexed {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Lexed::default();
    };
    let mut defines = Vec::new();
    let mut spelled = Vec::new();
    lex(text, true, |item| match item {
        Item::Directive { start, end, .. } => {
            let define = text.get(start..end).and_then(define_of);
            defines.extend(define.filter(|define| !grammar_words().contains(&define.name)));
        }
        Item::Identifier { start, end } => spelled.extend(text.get(start..end)),
        Item::Comment { .. } | Item::Token { .. } => {}
    });
    spelled.sort_unstable();
    spelled.dedup();
    Lexed {
        defines: Defines(defines),
        spelled: spelled.into_iter().flat_map(|name| [name, "\n"]).collect(),
    }
}

/// The words either C grammar reads as a token of its own, each a keyword,
/// a specifier or an attribute form: the anonymous kinds of their nodes
/// that are identifiers.
fn grammar_words() -> &'static BTreeSet<String> {
    static WORDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        let mut words = BTreeSet::new();
        for language in [&super::super::C, &super::super::CPP] {
            let grammar = (language.grammar)();
            for id in 0..u16::try_from(grammar.node_kind_count()).unwrap_or(u16::MAX) {
                let word = grammar.node_kind_for_id(id).filter(|kind| {
                    !grammar.node_kind_is_named(id)
                        && is_identifier(kind)
                        && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                });
                words.extend(word.map(str::to_owned));
            }
        }
        words
    })
}

/// The `#define` the directive `text` is, when it is one.
pub(super) fn define_of(text: &str) -> Option<Define> {
    let rest = text.strip_prefix('#')?.trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix("define")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    // A name that starts with a digit is no identifier any file writes,
    // so it blanks nothing.
    let (name, rest) = rest.split_at(name_end);
    if name.is_empty() {
        return None;
    }
    let (function_like, parameters, body) = match rest.strip_prefix('(') {
        Some(after) => {
            let (parameters, body) = after.split_once(')')?;
            (true, parameters.split(',').map(str::trim).collect(), body)
        }
        None => (false, Vec::new(), rest),
    };
    Some(Define {
        name: name.to_owned(),
        function_like,
        body: classify(&body_tokens(body), &parameters),
    })
}

/// The tokens of a `#define`'s body, comments and continuations left out;
/// one more than [`MAX_MACRO_BODY_TOKENS`] at most.
fn body_tokens(body: &str) -> Vec<&str> {
    let mut found = Vec::new();
    tokens(body, &mut |range| {
        if found.len() <= MAX_MACRO_BODY_TOKENS {
            found.extend(body.get(range));
        }
    });
    found
}

/// Hand `each` the range of each token of `text`, a C or C++ text read
/// with no directive in it (a macro's body): each identifier, literal,
/// number and punctuation mark, comments and continuations left out.
pub(in crate::code) fn tokens(text: &str, each: &mut dyn FnMut(Range<usize>)) {
    lex(text, false, |item| match item {
        Item::Token { start, end } | Item::Identifier { start, end } => each(start..end),
        Item::Directive { .. } | Item::Comment { .. } => {}
    });
}

/// What a body of `tokens` is, in a macro of `parameters`.
fn classify(tokens: &[&str], parameters: &[&str]) -> Body {
    if tokens.len() > MAX_MACRO_BODY_TOKENS {
        return Body::Code;
    }
    if !tokens.is_empty() && tokens.iter().all(|token| *token == "}") {
        return Body::Closes;
    }
    if opens(tokens) {
        return Body::Opens;
    }
    let (mut names, mut calls) = (Vec::new(), Vec::new());
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        let called = tokens.get(at + 1) == Some(&"(");
        at += 1;
        match *token {
            "[" if tokens.get(at) == Some(&"[") => at = skip_attribute(tokens, at),
            word if SPECIFIERS.contains(&word) => {}
            word if ATTRIBUTES.contains(&word) && called => at = skip_group(tokens, at),
            word if is_identifier(word) && !parameters.contains(&word) => match called {
                true => {
                    calls.push(word.to_owned());
                    at = skip_group(tokens, at);
                }
                false => names.push(word.to_owned()),
            },
            _ => return Body::Code,
        }
    }
    Body::Attribute {
        names,
        calls,
        empty: tokens.is_empty(),
        holds_static: tokens.contains(&STATIC),
    }
}

/// Whether `tokens` are only `namespace a::b {`, `inline namespace x {`
/// and `extern "C" {` openings, one at least.
fn opens(tokens: &[&str]) -> bool {
    let mut at = 0;
    while at < tokens.len() {
        if tokens.get(at) == Some(&"inline") {
            at += 1;
        }
        match tokens.get(at) {
            Some(&"namespace") => {
                at += 1;
                while tokens.get(at).is_some_and(|token| is_identifier(token)) {
                    at += 1;
                    if tokens.get(at) == Some(&"::") {
                        at += 1;
                    }
                }
            }
            Some(&"extern") if tokens.get(at + 1).is_some_and(|t| t.starts_with('"')) => at += 2,
            _ => return false,
        }
        if tokens.get(at) != Some(&"{") {
            return false;
        }
        at += 1;
    }
    at > 0
}

/// The place past the parenthesized group at `at`; `at` when none opens
/// there.
fn skip_group(tokens: &[&str], at: usize) -> usize {
    let mut depth = 0_usize;
    let mut next = at;
    while let Some(token) = tokens.get(next) {
        next += 1;
        match *token {
            "(" => depth += 1,
            ")" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return next;
                }
            }
            _ if depth == 0 => return at,
            _ => {}
        }
    }
    next
}

/// The place past the `[[...]]` attribute whose second `[` is at `at`.
fn skip_attribute(tokens: &[&str], at: usize) -> usize {
    let mut next = at + 1;
    while let Some(token) = tokens.get(next) {
        next += 1;
        if *token == "]" && tokens.get(next) == Some(&"]") {
            return next + 1;
        }
    }
    next
}

/// Whether `word` is a C identifier: a letter or `_` first.
fn is_identifier(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
}
