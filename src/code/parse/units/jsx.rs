//! The text of JSX, hashed as JSX renders it: whitespace around a line
//! break trimmed and joined, a character reference among the text read
//! as part of it.

use std::ops::Range;

use tree_sitter::Node;

use super::Unit;

/// The JSX text a node marked by its characters ended: where it ended,
/// whether a character but whitespace came before, and the run of
/// whitespace at its end that holds a line break, from its first byte.
#[derive(Debug, Clone, Copy)]
pub(super) struct JsxText {
    end: usize,
    seen: bool,
    broken: Option<usize>,
}

/// The whitespace JSX trims around a line break: a space, a tab and the
/// two line-break characters both TypeScript's and Babel's transforms
/// trim. Any other character is text.
const fn is_jsx_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Push the units of `node`'s text as JSX renders it, after the JSX text
/// `last` the previous such node ended, which it continues when only
/// whitespace lies between (a character reference among the text). A
/// character is a unit of its own, a space an empty one, so text that a
/// node left out of the hash splits (`a{/* c */}b`) hashes as the text
/// joined. A run of whitespace that holds a line break is one space
/// between text and is dropped at an end of the text; any other run is
/// kept as written. A run a node ends with no line break in it is kept
/// whatever follows, so it is pushed with the node.
pub(super) fn push_chars(
    units: &mut Vec<Unit>,
    node: Node<'_>,
    text: &str,
    last: &mut Option<JsxText>,
) {
    let start = node.start_byte();
    let (Some(chars), gap) = (
        text.get(node.byte_range()),
        last.and_then(|last| text.get(last.end..start)),
    ) else {
        return;
    };
    let continued = last
        .take()
        .filter(|_| gap.is_some_and(|gap| gap.chars().all(is_jsx_space)));
    let mut seen = continued.is_some_and(|last| last.seen);
    let mut run = continued.and_then(|last| last.broken.or((start > last.end).then_some(last.end)));
    for (at, c) in chars.char_indices().map(|(at, c)| (start + at, c)) {
        if is_jsx_space(c) {
            run = run.or(Some(at));
            continue;
        }
        if let Some(from) = run.take() {
            push_space(units, text, from..at, seen);
        }
        seen = true;
        units.push(Unit::text(at, at + c.len_utf8()));
    }
    let end = node.end_byte();
    let broken = run.filter(|from| {
        text.get(*from..end)
            .is_some_and(|run| run.contains(['\n', '\r']))
    });
    if let (Some(from), None) = (run, broken) {
        push_space(units, text, from..end, seen);
    }
    *last = Some(JsxText { end, seen, broken });
}

/// Push the units of the run of JSX whitespace over `bytes` of `text`:
/// one empty unit, a space, when it holds a line break and text came
/// before it (`seen`), none when it holds one and none came; else an
/// empty unit for each space and a unit for each tab, as written.
fn push_space(units: &mut Vec<Unit>, text: &str, bytes: Range<usize>, seen: bool) {
    let Some(run) = text.get(bytes.start..bytes.end) else {
        return;
    };
    match run.contains(['\n', '\r']) {
        true if seen => units.push(Unit::text(bytes.end, bytes.end)),
        true => {}
        false => units.extend(run.char_indices().map(|(at, c)| match c {
            ' ' => Unit::text(bytes.start + at, bytes.start + at),
            _ => Unit::text(bytes.start + at, bytes.start + at + c.len_utf8()),
        })),
    }
}
