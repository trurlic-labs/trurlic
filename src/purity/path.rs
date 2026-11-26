//! The paths a file's tokens name, and where in the crate one goes.

use super::read::{Read, Token, is_word, text_at};

/// A path a file's non-test code names.
pub(super) struct Named<'a> {
    pub(super) line: usize,
    /// Its segments as spelled, a glob as `*` and an alias's `as` kept
    /// last (`std::fs::as` for `use std::fs as disk`).
    pub(super) segments: Vec<String>,
    /// The path from the crate root of the module its first token stands
    /// in, an inline module's included.
    pub(super) module: Vec<&'a str>,
    /// Whether it is the tree of a `use` a visibility opens (`pub use`,
    /// `pub(crate) use`, `pub(in ..) use`): a re-export.
    pub(super) exported: bool,
}

/// Each path `file`'s tokens name: every path or `use` tree that starts at
/// a token ([`starts_path`]), a list's entries each one.
pub(super) fn paths(file: &Read) -> Vec<Named<'_>> {
    let tokens = &file.tokens;
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        if !starts_path(tokens, at) {
            continue;
        }
        let module: Vec<&str> = file
            .modules
            .get(token.module)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(String::as_str)
            .collect();
        let exported = is_exported(tokens, at);
        let mut next = at;
        found.extend(
            read_path(tokens, &mut next, Vec::new())
                .into_iter()
                .map(|segments| Named {
                    line: token.line,
                    segments,
                    module: module.clone(),
                    exported,
                }),
        );
    }
    found
}

/// Whether a path starts at `at`: a word or `$crate` that no `::` or `.`
/// comes before, and no visibility holds (`pub(crate)`, `pub(super)`,
/// `pub(self)`, `pub(in ..)`). A tuple field's parenthesised type
/// (`pub (crate::x::T)`) is a path.
fn starts_path(tokens: &[Token], at: usize) -> bool {
    let back = |by: usize| at.checked_sub(by).map_or("", |n| text_at(tokens, n));
    let first = back(0);
    let word = first == "$crate" || first == "::" || is_word(first);
    let continues = matches!(back(1), "::" | ".");
    let restricted = matches!(first, "crate" | "super" | "self")
        && back(1) == "("
        && back(2) == "pub"
        && text_at(tokens, at + 1) == ")";
    let visibility = restricted || (back(1) == "in" && back(2) == "(" && back(3) == "pub");
    word && !continues && !visibility
}

/// Whether the path at `at` is the tree of a `use` that a visibility
/// opens.
fn is_exported(tokens: &[Token], at: usize) -> bool {
    let back = |by: usize| at.checked_sub(by).map_or("", |n| text_at(tokens, n));
    match (back(1), back(2)) {
        ("use", "pub") => true,
        ("use", ")") => (3..)
            .find(|&by| matches!(back(by), "(" | ""))
            .is_some_and(|by| back(by + 1) == "pub"),
        _ => false,
    }
}

/// The paths a path or `use` tree from `at` names, each prefixed by
/// `prefix`, a list's entries each one; `at` is left past it.
fn read_path(tokens: &[Token], at: &mut usize, prefix: Vec<String>) -> Vec<Vec<String>> {
    let mut segments = prefix;
    if text_at(tokens, *at) == "::" {
        *at += 1;
    }
    loop {
        match text_at(tokens, *at) {
            "{" => {
                *at += 1;
                let mut found = Vec::new();
                while !matches!(text_at(tokens, *at), "}" | "") {
                    found.extend(read_path(tokens, at, segments.clone()));
                    if text_at(tokens, *at) == "," {
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
                if text_at(tokens, *at) == "as" {
                    segments.push("as".to_owned());
                    *at += 2;
                    return vec![segments];
                }
                if text_at(tokens, *at) != "::" {
                    return vec![segments];
                }
                *at += 1;
            }
            _ => return vec![segments],
        }
    }
}

/// The segments `path`, named in the module `module`, names from the
/// crate root, when it goes into the crate: `crate` or `$crate` first, or
/// `super`s, read from `module`. `None` for a path that starts anywhere
/// else. A `self` segment is no step.
pub(super) fn from_crate<'a>(path: &'a [String], module: &[&'a str]) -> Option<Vec<&'a str>> {
    let segments: Vec<&str> = path
        .iter()
        .map(String::as_str)
        .filter(|segment| *segment != "self")
        .collect();
    let supers = segments
        .iter()
        .take_while(|segment| **segment == "super")
        .count();
    match segments.as_slice() {
        ["crate" | "$crate", rest @ ..] => Some(rest.to_vec()),
        [_, ..] if supers > 0 => {
            let above = module
                .get(..module.len().saturating_sub(supers))
                .unwrap_or_default();
            Some(
                above
                    .iter()
                    .chain(segments.get(supers..)?)
                    .copied()
                    .collect(),
            )
        }
        [] | [_, ..] => None,
    }
}
