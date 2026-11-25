//! The bound on a Python file's indentations, checked before the parse:
//! tree-sitter-python's scanner aborts the process past it. It is the one
//! rule of a language the general resolver reads that is code, not its
//! query: no query runs before the parse.
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

use std::collections::BTreeSet;

use super::index::IndexFault;

/// The distinct widths a Python file's lines may start at.
pub(super) const MAX_INDENTATIONS: usize = 256;

/// Refuse `text` when its lines start at more than [`MAX_INDENTATIONS`]
/// widths. A width is what the scanner measures: from a line feed,
/// carriage return, form feed or NUL, a space counts 1 and a tab 8 in 16
/// bits that wrap, and a backslash before a line break continues the
/// run. Every line counts, a line inside a string or brackets too, so the
/// widths the scanner can push are among the ones counted.
pub(super) fn within_bound(text: &str) -> Result<(), IndexFault> {
    let bytes = text.as_bytes();
    let mut widths = BTreeSet::new();
    // The file's start is a line's.
    let mut width: Option<u16> = Some(0);
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        at += 1;
        let continued = match (byte, bytes.get(at), bytes.get(at + 1)) {
            (b' ', ..) => Some(1),
            (b'\t', ..) => Some(8),
            (b'\\', Some(b'\n'), _) | (b'\\', None, _) => Some(0).inspect(|_| at += 1),
            (b'\\', Some(b'\r'), Some(b'\n')) => Some(0).inspect(|_| at += 2),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose lines start at `count` distinct widths of spaces.
    fn widths(count: usize) -> String {
        (1..=count)
            .map(|n| format!("{}x\n", " ".repeat(n)))
            .collect()
    }

    #[test]
    fn a_file_past_the_bound_of_distinct_indentations_is_refused() {
        assert_eq!(within_bound(&widths(MAX_INDENTATIONS)), Ok(()));
        assert_eq!(
            within_bound(&widths(MAX_INDENTATIONS + 1)),
            Err(IndexFault::TooManyIndentations {
                max: MAX_INDENTATIONS
            })
        );
    }

    #[test]
    fn widths_are_counted_as_the_scanner_counts_them() {
        // A tab is 8, a continued line adds to the width it continues, and
        // a form feed or a carriage return starts a width again.
        let one = |text: &str| {
            let mut found = String::new();
            for n in 0..MAX_INDENTATIONS {
                found.push_str(&format!("{}x\n", " ".repeat(n + 1000)));
            }
            found.push_str(text);
            within_bound(&found)
        };

        assert_eq!(
            one("\tx\n"),
            Err(IndexFault::TooManyIndentations {
                max: MAX_INDENTATIONS
            })
        );
        assert_eq!(one(&format!("{}x\n", " ".repeat(1008))), Ok(()));
        assert_eq!(one(&format!("\t{}x\n", " ".repeat(1000))), Ok(()));
        assert_eq!(
            one(&format!("{}\\\n{}x\n", " ".repeat(1000), " ".repeat(8))),
            Ok(())
        );
        assert_eq!(
            one(&format!("{}\x0c{}x\n", " ".repeat(5), " ".repeat(1000))),
            Ok(())
        );
        assert_eq!(one(&format!("{}\r\n", " ".repeat(5))), Ok(()));
        assert_eq!(one(&format!("\0{}x\n", " ".repeat(1000))), Ok(()));
        assert_eq!(one(&format!("{}x\n", " ".repeat(1000 + 65_536))), Ok(()));
        assert_eq!(
            one("  x\n"),
            Err(IndexFault::TooManyIndentations {
                max: MAX_INDENTATIONS
            })
        );
    }
}
