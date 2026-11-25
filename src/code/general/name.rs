//! How a definition's name node becomes the text of its segments: the
//! text of its leaves, comments and the subtrees of the pattern's skipped
//! kinds left out (a template's arguments), one space where bytes lie
//! between two leaves that a word character ends and starts, so
//! `operator ==` and `operator==` are one name; split at each leaf whose text is the pattern's
//! separator (`namespace A.B`); and a part written as a string literal
//! taken without its quotes (`describe("opens")`, `declare module "x"`).

use tree_sitter::Node;

use crate::code::index::IndexFault;
use crate::code::syntax;
use crate::code::work::Work;

/// The quotes a string literal opens and closes with.
const QUOTES: [char; 3] = ['"', '\'', '`'];

/// The text of each segment of the name `node`, in `text`, split at each
/// leaf whose text is `separator`, the subtrees of the `skipped` kinds
/// left out. Its bytes are charged to `work`, so a
/// name read is never more than the file's budget.
pub(super) fn parts(
    node: Node<'_>,
    text: &str,
    (separator, skipped): (Option<&str>, &[u16]),
    work: &mut Work,
) -> Result<Vec<String>, IndexFault> {
    work.spend(node.end_byte() - node.start_byte())?;
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut last_end: Option<usize> = None;
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        let left_out = syntax::is_comment(current) || skipped.contains(&current.kind_id());
        if !left_out && cursor.goto_first_child() {
            continue;
        }
        let leaf = match left_out {
            true => "",
            false => syntax::text_of(current, text),
        };
        if !left_out && Some(leaf) == separator {
            parts.push(unquoted(&part).to_owned());
            part.clear();
            last_end = None;
        } else if !leaf.is_empty() {
            let word = |c: char| c.is_alphanumeric() || c == '_';
            if last_end.is_some_and(|end| end < current.start_byte())
                && part.chars().last().is_some_and(word)
                && leaf.chars().next().is_some_and(word)
            {
                part.push(' ');
            }
            part.push_str(leaf);
            last_end = Some(current.end_byte());
        }
        if !next_leaf(&mut cursor, node) {
            parts.push(unquoted(&part).to_owned());
            return Ok(parts);
        }
    }
}

/// Move `cursor` past the subtree it is at to the next node of `top`'s
/// walk; `false` at the walk's end.
fn next_leaf(cursor: &mut tree_sitter::TreeCursor<'_>, top: Node<'_>) -> bool {
    loop {
        if cursor.node() == top {
            return false;
        }
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
    }
}

/// `part` without the quotes around it when it is a string literal, one
/// quote character opening and closing it.
fn unquoted(part: &str) -> &str {
    QUOTES
        .iter()
        .find_map(|quote| part.strip_prefix(*quote)?.strip_suffix(*quote))
        .unwrap_or(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_literal_is_named_without_its_quotes_and_nothing_else_is() {
        for (part, name) in [
            ("\"opens\"", "opens"),
            ("'a-b'", "a-b"),
            ("`x ${y}`", "x ${y}"),
            ("\"say \\\"hi\\\"\"", "say \\\"hi\\\""),
            ("\"\"", ""),
            ("\"", "\""),
            ("\"a'", "\"a'"),
            ("plain", "plain"),
        ] {
            assert_eq!(unquoted(part), name, "{part}");
        }
    }
}
