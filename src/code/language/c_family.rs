//! What C and C++ need done to a file's bytes before the parse, since the
//! index runs no preprocessor and no query runs before the parse: the
//! macros the tree itself defines as attributes or specifiers
//! (`LEVELDB_EXPORT`, `FMT_CONSTEXPR`, `CURL_PRINTF(2, 3)`) or as a
//! namespace's opening and closing (`FMT_BEGIN_NAMESPACE`) are blanked,
//! so `class LEVELDB_EXPORT DB {` parses as the class it is; and the
//! comments of a `#define` continued over lines are blanked, which
//! tree-sitter's C grammars read as the directive's end.
//!
//! A blank is spaces in place of the bytes, so every offset holds, and
//! the parse and the hash read the blanked text: a blanked name is no
//! token. A name is blanked only where it stands outside every
//! directive, comment and literal, so the `#define` that made it blankable
//! and an `#ifdef` that tests it keep it.
//!
//! Which names are blankable is a fact of the whole tree: `CURL_EXTERN` is
//! defined in one header and written in many files. Each file's
//! `#define`s are read alone ([`defines`]), and the shell takes the
//! fixpoint over the tree's ([`BlankMacros::of`]) and hands the set to
//! every index of a C or C++ file, whose cache key holds it. A name is
//! blankable when every `#define` of it in the tree is empty or only
//! specifiers, attribute forms and other blankable names, a function-like
//! one with at least one such body not empty (an empty one alone is a
//! function's stand-in in another arm); or when every `#define` of it is
//! only `namespace x {` openings or only closing braces, and the tree
//! defines such an opening. Each `#define`'s body is read up to
//! [`MAX_MACRO_BODY_TOKENS`] tokens, and a blanked call's arguments up to
//! [`MAX_BLANK_ARGUMENT_BYTES`], so both passes are linear in the bytes.
//!
//! What no rule sees: the members, enumerators and functions a
//! function-like macro generates (`BIT(flag);` in a struct), a macro
//! defined outside the tree (a system or generated header), and the
//! segments of a namespace a macro opens, which its definitions lack.

mod lex;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use lex::{Item, comment_end, lex, literal_end};

/// The tokens of a `#define`'s body read to classify it; a longer body is
/// no attribute.
pub(in crate::code) const MAX_MACRO_BODY_TOKENS: usize = 64;

/// The bytes a blanked function-like macro's arguments may span, from its
/// opening parenthesis; a call longer is left as written.
pub(in crate::code) const MAX_BLANK_ARGUMENT_BYTES: usize = 256;

/// The words a declaration may hold that state no name: a body of them
/// alone is an attribute.
const SPECIFIERS: [&str; 31] = [
    "static",
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

/// What a `#define`'s body is, as the blank rule reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// Specifiers and attribute forms, and these names: written alone, or
    /// called with arguments.
    Attribute {
        names: Vec<String>,
        calls: Vec<String>,
        empty: bool,
    },
    /// Only `namespace x {` and `extern "C" {` openings.
    Opens,
    /// Only closing braces.
    Closes,
    Other,
}

/// One `#define` of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Define {
    name: String,
    function_like: bool,
    body: Body,
}

/// The `#define`s of one C or C++ file, as the tree's blank set is made
/// from them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Defines(Vec<Define>);

/// The names a C or C++ file's index blanks: written alone, or called
/// with their arguments; and their identity, in the cache key of every C
/// and C++ file of the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlankMacros {
    names: BTreeSet<String>,
    calls: BTreeSet<String>,
    identity: [u8; 32],
}

/// The `#define`s of `bytes`, a C or C++ file; none of bytes that are not
/// UTF-8, which no index reads either.
pub(crate) fn defines(bytes: &[u8]) -> Defines {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Defines::default();
    };
    let mut found = Vec::new();
    lex(text, true, |item| {
        if let Item::Directive { start, end, .. } = item {
            found.extend(text.get(start..end).and_then(define_of));
        }
    });
    Defines(found)
}

impl BlankMacros {
    /// The blankable names of a tree whose files' `#define`s are `files`,
    /// by the fixpoint the module states, in work linear in them.
    pub(crate) fn of(files: impl IntoIterator<Item = Defines>) -> Self {
        let mut by_name: BTreeMap<String, Vec<(bool, Body)>> = BTreeMap::new();
        for define in files.into_iter().flat_map(|file| file.0) {
            by_name
                .entry(define.name)
                .or_default()
                .push((define.function_like, define.body));
        }
        let mut names = namespaces(&by_name);
        let calls = attributes(&by_name, &mut names);
        let mut hasher = blake3::Hasher::new();
        for (set, mark) in [(&names, b'n'), (&calls, b'c')] {
            for name in set {
                hasher.update(&[mark]);
                hasher.update(&(name.len() as u64).to_le_bytes());
                hasher.update(name.as_bytes());
            }
        }
        Self {
            names,
            calls,
            identity: *hasher.finalize().as_bytes(),
        }
    }

    /// Its identity, in a cache key.
    pub(crate) const fn identity(&self) -> &[u8; 32] {
        &self.identity
    }
}

/// The names every `#define` of which opens or closes a namespace, when
/// one of them opens one; an empty arm beside them is allowed.
fn namespaces(by_name: &BTreeMap<String, Vec<(bool, Body)>>) -> BTreeSet<String> {
    let opens = shaped(by_name, &Body::Opens);
    if opens.is_empty() {
        return opens;
    }
    opens
        .into_iter()
        .chain(shaped(by_name, &Body::Closes))
        .collect()
}

/// The names every `#define` of which is object-like and `wanted` or
/// empty, one at least `wanted`.
fn shaped(by_name: &BTreeMap<String, Vec<(bool, Body)>>, wanted: &Body) -> BTreeSet<String> {
    by_name
        .iter()
        .filter(|(_, bodies)| {
            bodies
                .iter()
                .all(|(function_like, body)| !function_like && (body == wanted || is_empty(body)))
                && bodies.iter().any(|(_, body)| body == wanted)
        })
        // clone: the set owns the names apart from the map of bodies.
        .map(|(name, _)| name.clone())
        .collect()
}

fn is_empty(body: &Body) -> bool {
    matches!(body, Body::Attribute { empty: true, .. })
}

/// What a candidate name needs to be blanked: whether it is function-like,
/// and the names its bodies write, each blankable in the form it is
/// written in (`true` for called with arguments).
type Needs<'n> = (bool, BTreeSet<(&'n str, bool)>);

/// The attribute names of `by_name` added to `names` (written alone), and
/// those called with arguments returned: a least fixpoint over a worklist,
/// so a cycle blanks nothing and each name is settled once.
fn attributes(
    by_name: &BTreeMap<String, Vec<(bool, Body)>>,
    names: &mut BTreeSet<String>,
) -> BTreeSet<String> {
    // Each candidate's form and what it needs: (name, called) pairs.
    let mut needs: BTreeMap<&str, Needs<'_>> = BTreeMap::new();
    for (name, bodies) in by_name {
        if let Some(candidate) = candidate(bodies) {
            needs.insert(name.as_str(), candidate);
        }
    }
    let mut needed_by: BTreeMap<(&str, bool), Vec<&str>> = BTreeMap::new();
    let mut left: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ready = Vec::new();
    for (name, (_, wants)) in &needs {
        for want in wants {
            needed_by.entry(*want).or_default().push(name);
        }
        left.insert(name, wants.len());
        if wants.is_empty() {
            ready.push(*name);
        }
    }
    let mut calls = BTreeSet::new();
    while let Some(name) = ready.pop() {
        let Some((function_like, _)) = needs.get(name) else {
            continue;
        };
        match function_like {
            true => calls.insert(name.to_owned()),
            false => names.insert(name.to_owned()),
        };
        for waiting in needed_by
            .remove(&(name, *function_like))
            .unwrap_or_default()
        {
            if let Some(count) = left.get_mut(waiting) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(waiting);
                }
            }
        }
    }
    calls
}

/// Whether the `#define`s `bodies` of one name make a candidate: its form
/// (function-like) and the names it needs blankable, each with the form
/// it is written in; `None` when no body or mix of them can be blanked.
fn candidate(bodies: &[(bool, Body)]) -> Option<Needs<'_>> {
    let function_like = bodies.first()?.0;
    let mut wants = BTreeSet::new();
    let mut written = false;
    for (form, body) in bodies {
        let Body::Attribute {
            names,
            calls,
            empty,
        } = body
        else {
            return None;
        };
        if *form != function_like {
            return None;
        }
        written |= !empty;
        wants.extend(names.iter().map(|name| (name.as_str(), false)));
        wants.extend(calls.iter().map(|name| (name.as_str(), true)));
    }
    (!function_like || written).then_some((function_like, wants))
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
    let (name, rest) = rest.split_at(name_end);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
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
    let mut tokens = Vec::new();
    lex(body, false, |item| {
        if let Item::Token { start, end } | Item::Identifier { start, end } = item
            && tokens.len() <= MAX_MACRO_BODY_TOKENS
        {
            tokens.extend(body.get(start..end));
        }
    });
    tokens
}

/// What a body of `tokens` is, in a macro of `parameters`.
fn classify(tokens: &[&str], parameters: &[&str]) -> Body {
    if tokens.len() > MAX_MACRO_BODY_TOKENS {
        return Body::Other;
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
            _ => return Body::Other,
        }
    }
    Body::Attribute {
        names,
        calls,
        empty: tokens.is_empty(),
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

fn is_identifier(token: &str) -> bool {
    token.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
}

/// `text`, a C or C++ file, with the names of `macros` (and a blanked
/// call's arguments) and the comments of a continued `#define` blanked;
/// borrowed when nothing is.
pub(in crate::code) fn blank<'t>(text: &'t str, macros: &BlankMacros) -> Cow<'t, str> {
    let mut out: Option<Vec<u8>> = None;
    let mut blank_range = |start: usize, end: usize| {
        let out = out.get_or_insert_with(|| text.as_bytes().to_vec());
        for byte in out.get_mut(start..end).into_iter().flatten() {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    };
    lex(text, true, |item| match item {
        Item::Directive {
            start,
            end,
            continued_comments: true,
        } => {
            for (from, to) in continued_comments(text.get(start..end).unwrap_or_default()) {
                blank_range(start + from, start + to);
            }
        }
        Item::Identifier { start, end } => {
            let name = text.get(start..end).unwrap_or_default();
            if macros.names.contains(name) {
                blank_range(start, end);
            } else if macros.calls.contains(name)
                && let Some(close) = arguments_end(text, end)
            {
                blank_range(start, close);
            }
        }
        Item::Directive { .. } | Item::Token { .. } | Item::Comment { .. } => {}
    });
    match out.and_then(|bytes| String::from_utf8(bytes).ok()) {
        Some(blanked) => Cow::Owned(blanked),
        None => Cow::Borrowed(text),
    }
}

/// The comments of `directive` to blank, when it is a `#define` continued
/// over lines: each before its last continuation. One after it is left,
/// since blanked it would leave the blanks tree-sitter-c reads a body's
/// end at.
fn continued_comments(directive: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    if define_of(directive).is_none() {
        return found;
    }
    let last = directive.rfind('\\').unwrap_or(0);
    lex(directive, false, |item| {
        if let Item::Comment { start, end } = item
            && end <= last
        {
            found.push((start, end));
        }
    });
    found
}

/// The end of the parenthesized arguments that open after blanks at
/// `from`, within [`MAX_BLANK_ARGUMENT_BYTES`]; `None` when none do.
fn arguments_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let open = from
        + bytes
            .get(from..)?
            .iter()
            .position(|b| !matches!(b, b' ' | b'\t'))?;
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let end = open
        .saturating_add(MAX_BLANK_ARGUMENT_BYTES)
        .min(bytes.len());
    let mut depth = 0_usize;
    let mut next = open;
    while next < end {
        match bytes.get(next) {
            Some(b'(') => depth += 1,
            Some(b')') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(next + 1);
                }
            }
            Some(b'"' | b'\'') => next = literal_end(bytes, next).saturating_sub(1),
            Some(b'/') if matches!(bytes.get(next + 1), Some(b'/' | b'*')) => {
                next = comment_end(bytes, next).saturating_sub(1);
            }
            Some(b';' | b'{' | b'}' | b'#') => return None,
            _ => {}
        }
        next += 1;
    }
    None
}

#[cfg(test)]
mod tests;
