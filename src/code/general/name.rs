//! How a definition's name node becomes the text of its segments: the
//! text of its leaves, comments left out, one space where bytes lie
//! between two leaves; split at each leaf whose text is the pattern's
//! separator (`namespace A.B`); and a part written as a string literal
//! taken without its quotes (`describe("opens")`, `declare module "x"`).

use tree_sitter::Node;

use crate::code::index::IndexFault;
use crate::code::syntax;
use crate::code::work::Work;

/// The quotes a string literal opens and closes with.
const QUOTES: [char; 3] = ['"', '\'', '`'];

/// The text of each segment of the name `node`, in `text`, split at each
/// leaf whose text is `separator`. Its bytes are charged to `work`, so a
/// name read is never more than the file's budget.
pub(super) fn parts(
    node: Node<'_>,
    text: &str,
    separator: Option<&str>,
    work: &mut Work,
) -> Result<Vec<String>, IndexFault> {
    work.spend(node.end_byte() - node.start_byte())?;
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut last_end: Option<usize> = None;
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        let comment = syntax::is_comment(current);
        if !comment && cursor.goto_first_child() {
            continue;
        }
        let leaf = syntax::text_of(current, text);
        if !comment && Some(leaf) == separator {
            parts.push(unquoted(&part).to_owned());
            part.clear();
            last_end = None;
        } else if !comment && !leaf.is_empty() {
            if last_end.is_some_and(|end| end < current.start_byte()) {
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
