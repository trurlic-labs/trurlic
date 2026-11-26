//! What tree-sitter-python's scanner needs of a file before a parse: its
//! indentation widths checked, past which the scanner aborts the process,
//! which no budget of the parse can stop, and its line ends read as Python
//! reads them. They are the only rules of a language the general resolver
//! reads that are code, not its query: no query runs before the parse.
//!
//! The scanner keeps the indentations of the open blocks on a stack and,
//! after each token it makes, writes them two bytes each after the open
//! string delimiters (at most 255 bytes) into tree-sitter's buffer of
//! 1,024 bytes. Its loop stops at the buffer's size only on an even count
//! of bytes: under an odd number of open strings, some 380 open blocks
//! write one byte past it, and tree-sitter's assertion on the length
//! aborts, which no unwind catches. The stack holds strictly increasing
//! widths, each measured from a line break, so it is no deeper than the
//! distinct widths the file's lines start at, counted as the scanner
//! counts them. A file with more than [`MAX_INDENTATIONS`] is refused:
//! that many entries and every delimiter fit the buffer with room.
//!
//! The scanner ends no line at a carriage return that no line feed
//! follows, so the parse reads a file's line ends as Python's tokenizer
//! does ([`line_ends`]).

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::code::index::IndexFault;

/// The distinct widths a Python file's lines may start at.
pub(in crate::code) const MAX_INDENTATIONS: usize = 256;

/// Refuse `text` when its lines start at more than [`MAX_INDENTATIONS`]
/// widths. A width is what the scanner measures: from a line feed,
/// carriage return, form feed or NUL, a space counts 1 and a tab 8 in 16
/// bits that wrap. A backslash before a line break continues a width
/// that is open; after any other byte (a comment's last, `# \`) the line
/// break starts one, as the scanner sees it. Every line counts, a line
/// inside a string or brackets too, so the widths the scanner can push
/// are among the ones counted.
pub(super) fn check_indentations(text: &str) -> Result<(), IndexFault> {
    let bytes = text.as_bytes();
    let mut widths = BTreeSet::new();
    // The file's start is a line's.
    let mut width: Option<u16> = Some(0);
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        at += 1;
        let open = width.is_some();
        let continued = match (byte, bytes.get(at), bytes.get(at + 1)) {
            (b' ', ..) => Some(1),
            (b'\t', ..) => Some(8),
            (b'\\', Some(b'\n'), _) | (b'\\', None, _) if open => Some(0).inspect(|_| at += 1),
            (b'\\', Some(b'\r'), Some(b'\n')) if open => Some(0).inspect(|_| at += 2),
            _ => None,
        };
        match continued {
            Some(added) => width = width.map(|w| w.wrapping_add(added)),
            // A line break starts a width again; any other byte ends the
            // width, which the scanner may push, a NUL too (one ends a
            // comment), and a NUL starts a width as the comment's end does.
            None => {
                let line_break = matches!(byte, b'\n' | b'\r' | b'\x0c');
                if let Some(ended) = width.filter(|w| *w > 0 && !line_break) {
                    widths.insert(ended);
                    if widths.len() > MAX_INDENTATIONS {
                        return Err(IndexFault::TooManyIndentations {
                            max: MAX_INDENTATIONS,
                        });
                    }
                }
                width = (line_break || byte == b'\0').then_some(0);
            }
        }
    }
    Ok(())
}

/// `text` as Python's tokenizer reads it: each carriage return that no
/// line feed follows is a line feed, one byte for one, so every span of
/// the file stays exact and its lines are Python's; `text` itself when it
/// holds none. Python reads one so in a string too: `'''a\rb'''` is
/// `'a\nb'`.
pub(in crate::code) fn line_ends(text: &str) -> Cow<'_, str> {
    // Each piece but the first follows a carriage return.
    let follow_returns = || text.split('\r').skip(1);
    if follow_returns().all(|piece| piece.starts_with('\n')) {
        return Cow::Borrowed(text);
    }
    let mut read = String::with_capacity(text.len());
    read.push_str(text.split('\r').next().unwrap_or_default());
    for piece in follow_returns() {
        read.push(if piece.starts_with('\n') { '\r' } else { '\n' });
        read.push_str(piece);
    }
    Cow::Owned(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::FileKind;
    use crate::code::testing::nested_ifs;

    /// A file whose lines start at `count` distinct widths of spaces.
    fn widths(count: usize) -> String {
        (1..=count)
            .map(|n| format!("{}x\n", " ".repeat(n)))
            .collect()
    }

    const PAST: Result<(), IndexFault> = Err(IndexFault::TooManyIndentations {
        max: MAX_INDENTATIONS,
    });

    #[test]
    fn a_file_past_the_bound_of_distinct_indentations_is_refused() {
        assert_eq!(check_indentations(&widths(MAX_INDENTATIONS)), Ok(()));
        assert_eq!(check_indentations(&widths(MAX_INDENTATIONS + 1)), PAST);
    }

    #[test]
    fn widths_are_counted_as_the_scanner_counts_them() {
        // The widths 1 to 255 and 300, the bound's worth: a line at one of
        // them is within it, a line at any other width past it.
        let one = |line: &str| {
            let mut text = widths(MAX_INDENTATIONS - 1);
            text.push_str(&format!("{}x\n", " ".repeat(300)));
            text.push_str(line);
            check_indentations(&text)
        };
        let spaces = |count: usize| " ".repeat(count);

        assert_eq!(one(&format!("{}x\n", spaces(256))), PAST);
        // A tab is 8 and adds to the spaces beside it.
        assert_eq!(one(&format!("\t{}x\n", spaces(292))), Ok(()));
        assert_eq!(one(&format!("{}x\n", "\t".repeat(40))), PAST);
        // A continued line adds to the width it continues.
        assert_eq!(one(&format!("{}\\\n{}x\n", spaces(296), spaces(4))), Ok(()));
        // A form feed, a carriage return and a NUL start a width again.
        assert_eq!(one(&format!("{}\x0c{}x\n", spaces(5), spaces(300))), Ok(()));
        assert_eq!(one(&format!("{}\rx\n", spaces(301))), Ok(()));
        assert_eq!(one(&format!("# c\0{}x\n", spaces(300))), Ok(()));
        assert_eq!(one(&format!("# c\0{}x\n", spaces(301))), PAST);
        // A width is 16 bits that wrap.
        assert_eq!(one(&format!("{}x\n", spaces(300 + 65_536))), Ok(()));
    }

    #[test]
    fn a_lone_carriage_return_is_read_as_a_line_feed_and_no_other_byte_moves() {
        assert_eq!(line_ends("a\rb\r\nc\r"), "a\nb\r\nc\n");
        assert!(matches!(line_ends("a\r\nb\n"), Cow::Borrowed(_)));
    }

    /// [`nested_ifs`], each `if` line ending in a comment that ends in a
    /// backslash, after a newline or a CR and a newline.
    fn commented_ifs(depth: usize, line_break: &str) -> String {
        nested_ifs(depth).replace("if x:\n", &format!("if x:  # \\{line_break}"))
    }

    #[test]
    fn a_comment_ending_in_a_backslash_continues_no_width_the_bound_counts() {
        let refused = Err(IndexFault::TooManyIndentations {
            max: MAX_INDENTATIONS,
        });

        for line_break in ["\n", "\r\n"] {
            let text = commented_ifs(600, line_break);
            assert_eq!(
                crate::code::testing::index(FileKind::Python, text.as_bytes()),
                refused
            );
        }
        let own_line = nested_ifs(600).replace("if x:\n", "if x:\n# \\\n");
        assert_eq!(
            crate::code::testing::index(FileKind::Python, own_line.as_bytes()),
            refused
        );
    }

    #[test]
    fn indentations_past_the_scanners_bound_are_refused_before_the_parse() {
        assert!(
            crate::code::testing::index(
                FileKind::Python,
                nested_ifs(MAX_INDENTATIONS - 1).as_bytes()
            )
            .is_ok()
        );
        for depth in [MAX_INDENTATIONS + 1, 600] {
            assert_eq!(
                crate::code::testing::index(FileKind::Python, nested_ifs(depth).as_bytes()),
                Err(IndexFault::TooManyIndentations {
                    max: MAX_INDENTATIONS
                })
            );
        }
    }
}
