//! What C and C++ need done to a file's bytes before the parse, since the
//! index runs no preprocessor and no query runs before the parse: the
//! macros the tree itself defines as attributes or specifiers
//! (`LEVELDB_EXPORT`, `FMT_CONSTEXPR`, `CURL_PRINTF(2, 3)`) or as a
//! namespace's opening and closing (`FMT_BEGIN_NAMESPACE`) are blanked,
//! so `class LEVELDB_EXPORT DB {` parses as the class it is; and the
//! comments of a `#define` continued over lines are blanked, line breaks
//! and all, since tree-sitter's C grammars read a break or a blank run
//! there as the directive's end.
//!
//! A blank is spaces in place of the bytes, so every offset holds, and
//! the parse and the hash read the blanked text: a blanked name is no
//! token. A blankable object-like macro whose every body but an empty one
//! holds `static` (`#define STATIC static`) is written `static` instead,
//! when its name is long enough to hold it, so the definition it stands
//! before stays private. A name is blanked only where it stands outside
//! every directive, comment and literal, so the `#define` that made it
//! blankable and an `#ifdef` that tests it keep it.
//!
//! Which names are blankable (`set.rs`) is a fact of the tree's headers
//! and of the file itself. The lexical pass of a file ([`lexed`]) gives
//! its `#define`s and the names it writes, and is cached by content; the
//! shell takes the tree's set from its headers' `#define`s, and each file
//! is indexed against the part of it that the file writes, its own
//! `#define`s judged with it. Each `#define`'s body is read up to
//! [`MAX_MACRO_BODY_TOKENS`] tokens, and a blanked call's arguments up to
//! [`MAX_BLANK_ARGUMENT_BYTES`], so every pass is linear in the bytes.
//!
//! What no rule sees: the members, enumerators and functions a
//! function-like macro generates (`BIT(flag);` in a struct), a macro
//! defined outside the tree (a system or generated header) or only in a
//! source file another file includes, and the segments of a namespace a
//! macro opens, which its definitions lack.

mod encoding;
mod lex;
mod set;

use std::borrow::Cow;
use std::ops::Range;

use lex::{Item, lex};

pub(crate) use encoding::{decode_lexed, encode_lexed, max_lexed_bytes};
pub(crate) use set::{BlankMacros, FileMacros};

/// The tokens of a `#define`'s body read to classify it; a longer body is
/// no attribute.
pub(in crate::code) const MAX_MACRO_BODY_TOKENS: usize = 64;

/// The bytes a blanked function-like macro's arguments may span, from its
/// opening parenthesis; a call longer is left as written.
pub(in crate::code) const MAX_BLANK_ARGUMENT_BYTES: usize = 256;

/// The words a declaration may hold that state no name: a body of them
/// alone is an attribute.
const SPECIFIERS: [&str; 31] = [
    STATIC,
    "inline",
    "extern",
    "const",
    "constexpr",
    "consteval",
    "constinit",
    "volatile",
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
/// that holds it is written as.
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
enum Body {
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
struct Define {
    name: String,
    function_like: bool,
    body: Body,
}

/// The `#define`s of one C or C++ file, as the blank sets are made from
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Defines(Vec<Define>);

/// What the lexical pass of one C or C++ file gives: its `#define`s, and
/// the names it writes outside every directive, comment and literal,
/// which are all its blank can touch: sorted, each once, a line break
/// after each, one text so that a cache read allocates it once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Lexed {
    defines: Defines,
    spelled: String,
}

impl Lexed {
    /// Its `#define`s, which a header gives the tree's set.
    pub(crate) fn into_defines(self) -> Defines {
        self.defines
    }

    /// The names it writes.
    fn spelled(&self) -> impl Iterator<Item = &str> {
        self.spelled.lines()
    }
}

/// Whether the file at `path` is a C or C++ header, whose `#define`s
/// reach every file of its tree.
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
            defines.extend(text.get(start..end).and_then(define_of));
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

/// The `#define` the directive `text` is, when it is one.
fn define_of(text: &str) -> Option<Define> {
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
                    if tokens.get(at) == Some(&":") && tokens.get(at + 1) == Some(&":") {
                        at += 2;
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

/// What a blanked range of a file is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Spaces, its line breaks kept.
    Spaces,
    /// Spaces, its line breaks too: a comment of a continued `#define`.
    Joined,
    /// `static`, then spaces.
    Static,
}

/// `text`, a C or C++ file, with the names of `macros` (and a blanked
/// call's arguments) and the comments of a continued `#define` blanked;
/// borrowed when nothing is.
pub(in crate::code) fn blank<'t>(text: &'t str, macros: &FileMacros) -> Cow<'t, str> {
    let mut blanks = Blanks {
        text,
        macros,
        found: Vec::new(),
        call: None,
    };
    lex(text, true, |item| blanks.take(item));
    blanks.abandon();
    if blanks.found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, fill) in blanks.found {
        out.push_str(text.get(at..range.start).unwrap_or_default());
        let written = text.get(range.start..range.end).unwrap_or_default();
        if fill == Fill::Static {
            out.push_str(STATIC);
        }
        let kept = written.char_indices().skip(match fill {
            Fill::Static => STATIC.len(),
            Fill::Spaces | Fill::Joined => 0,
        });
        for (_, c) in kept {
            match (c, fill) {
                ('\n', Fill::Spaces | Fill::Static) => out.push('\n'),
                // Any other character, and a comment's line break.
                _ => out.extend(std::iter::repeat_n(' ', c.len_utf8())),
            }
        }
        at = range.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    Cow::Owned(out)
}

/// The ranges a blank finds, in order, as the lexer hands it the items of
/// its text.
struct Blanks<'t, 'm> {
    text: &'t str,
    macros: &'m FileMacros,
    found: Vec<(Range<usize>, Fill)>,
    call: Option<Call>,
}

/// A blankable macro's call whose arguments the blank is reading: where
/// its name starts and ends, where its `(` is once met, how deep its
/// parentheses are open, and the names it would blank in them were the
/// call never closed.
struct Call {
    start: usize,
    name_end: usize,
    open: Option<usize>,
    depth: usize,
    names: Vec<(Range<usize>, Fill)>,
}

impl Blanks<'_, '_> {
    /// Take `item`, the next of the text: into the call being read, or else
    /// as one that may start a blank.
    fn take(&mut self, item: Item) {
        if let Some(call) = self.call.as_mut() {
            match call.read(item, self.text, self.macros) {
                Step::Reading => return,
                Step::Closed(end) => {
                    self.found.push((call.start..end, Fill::Spaces));
                    self.call = None;
                    return;
                }
                Step::Abandoned => self.abandon(),
            }
        }
        match item {
            Item::Directive {
                start,
                end,
                continued_comments: true,
            } => {
                for comment in continued_comments(self.text.get(start..end).unwrap_or_default()) {
                    let range = start + comment.start..start + comment.end;
                    self.found.push((range, Fill::Joined));
                }
            }
            Item::Identifier { start, end } => {
                let name = self.text.get(start..end).unwrap_or_default();
                if let Some(fill) = macros_fill(self.macros, name) {
                    self.found.push((start..end, fill));
                } else if self.macros.calls.contains(name) {
                    self.call = Some(Call {
                        start,
                        name_end: end,
                        open: None,
                        depth: 0,
                        names: Vec::new(),
                    });
                }
            }
            Item::Directive { .. } | Item::Token { .. } | Item::Comment { .. } => {}
        }
    }

    /// Give up the call being read: the names in its arguments are blanked
    /// alone.
    fn abandon(&mut self) {
        if let Some(call) = self.call.take() {
            self.found.extend(call.names);
        }
    }
}

/// How `name`, written alone, is blanked by `macros`; `None` when it is
/// not.
fn macros_fill(macros: &FileMacros, name: &str) -> Option<Fill> {
    match (macros.names.contains(name), macros.statics.contains(name)) {
        (true, true) => Some(Fill::Static),
        (true, false) => Some(Fill::Spaces),
        (false, _) => None,
    }
}

/// What reading one more item did to a call.
enum Step {
    Reading,
    /// Its `)` ends here.
    Closed(usize),
    /// It is no call to blank: no `(` right after its name, a `;`, brace or
    /// directive before its `)`, or arguments past their bound.
    Abandoned,
}

impl Call {
    fn read(&mut self, item: Item, text: &str, macros: &FileMacros) -> Step {
        let (start, end) = match item {
            Item::Token { start, end } | Item::Identifier { start, end } => (start, end),
            Item::Comment { .. } if self.open.is_some() => return Step::Reading,
            Item::Comment { .. } | Item::Directive { .. } => return Step::Abandoned,
        };
        let Some(open) = self.open else {
            let gap = text.get(self.name_end..start).unwrap_or_default();
            if text.get(start..end) != Some("(") || !gap.bytes().all(|b| matches!(b, b' ' | b'\t'))
            {
                return Step::Abandoned;
            }
            self.open = Some(start);
            self.depth = 1;
            return Step::Reading;
        };
        if end - open > MAX_BLANK_ARGUMENT_BYTES {
            return Step::Abandoned;
        }
        let token = text.get(start..end).unwrap_or_default();
        match token {
            "(" => self.depth += 1,
            ")" if self.depth == 1 => return Step::Closed(end),
            ")" => self.depth -= 1,
            ";" | "{" | "}" => return Step::Abandoned,
            name => self
                .names
                .extend(macros_fill(macros, name).map(|fill| (start..end, fill))),
        }
        Step::Reading
    }
}

/// The comments of `directive` to blank, when it is a `#define` continued
/// over lines: each before its last continuation. One after it is left,
/// since blanked it would leave the blanks tree-sitter-c reads a body's
/// end at.
fn continued_comments(directive: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if define_of(directive).is_none() {
        return found;
    }
    let last = directive.rfind('\\').unwrap_or(0);
    lex(directive, false, |item| {
        if let Item::Comment { start, end } = item
            && end <= last
        {
            found.push(start..end);
        }
    });
    found
}

#[cfg(test)]
mod tests;
