//! A C or C++ file's bytes read as the preprocessor reads them before
//! it expands anything: directives (each over its continued lines), comments,
//! literals, identifiers and other tokens, in one pass in which each byte
//! is read a bounded number of times.

/// What the lexer meets, by byte range.
#[derive(Debug, Clone, Copy)]
pub(super) enum Item {
    /// A preprocessor directive, its continued lines and comments in it
    /// included; whether a comment stands before one of its line
    /// continuations.
    Directive {
        start: usize,
        end: usize,
        continued_comments: bool,
    },
    Comment {
        start: usize,
        end: usize,
    },
    Identifier {
        start: usize,
        end: usize,
    },
    /// A literal, a number or a punctuation mark.
    Token {
        start: usize,
        end: usize,
    },
}

/// Hand `each` the items of `text`, in one pass: each byte is read a
/// bounded number of times. A `#` opens a directive at a line's start,
/// and at the text's when `line_start` says it is one.
pub(super) fn lex(text: &str, mut line_start: bool, mut each: impl FnMut(Item)) {
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let blank = matches!(byte, b' ' | b'\t' | b'\r' | b'\x0c' | b'\x0b');
        let (end, item) = match byte {
            b'\n' => (at + 1, None),
            _ if blank => (at + 1, None),
            b'\\' if continuation(bytes, at) > 0 => (at + continuation(bytes, at), None),
            _ => item_at(text, at, line_start),
        };
        if let Some(item) = item {
            each(item);
        }
        line_start = byte == b'\n' || (line_start && blank);
        at = end;
    }
}

/// The item that starts at `at` in `text`, which is no blank, and where it
/// ends.
fn item_at(text: &str, at: usize, line_start: bool) -> (usize, Option<Item>) {
    let bytes = text.as_bytes();
    let token = |end| (end, Some(Item::Token { start: at, end }));
    match bytes.get(at) {
        Some(b'#') if line_start => {
            let (end, continued_comments) = directive_end(bytes, at);
            let directive = Item::Directive {
                start: at,
                end,
                continued_comments,
            };
            (end, Some(directive))
        }
        Some(b'/') if matches!(bytes.get(at + 1), Some(b'/' | b'*')) => {
            let end = comment_end(bytes, at);
            (end, Some(Item::Comment { start: at, end }))
        }
        Some(b'"' | b'\'') => token(literal_end(bytes, at)),
        Some(b'R' | b'u' | b'U' | b'L') if raw_string_start(bytes, at).is_some() => {
            token(raw_string_end(bytes, at))
        }
        Some(b'_' | b'a'..=b'z' | b'A'..=b'Z') => {
            let end = word_end(bytes, at);
            (end, Some(Item::Identifier { start: at, end }))
        }
        Some(b'0'..=b'9') => token(number_end(bytes, at)),
        _ => token(
            at + text
                .get(at..)
                .and_then(|rest| rest.chars().next())
                .map_or(1, char::len_utf8),
        ),
    }
}

/// The bytes of a line continuation at `at`: a backslash right before a
/// line break; 0 when none is there.
fn continuation(bytes: &[u8], at: usize) -> usize {
    match bytes.get(at..) {
        Some([b'\\', b'\n', ..]) => 2,
        Some([b'\\', b'\r', b'\n', ..]) => 3,
        _ => 0,
    }
}

/// The end of the directive at `at` (its line, continued, and the block
/// comments that run past it), and whether a comment comes before one of
/// its continuations.
fn directive_end(bytes: &[u8], at: usize) -> (usize, bool) {
    let mut next = at + 1;
    let (mut commented, mut continued_comments) = (false, false);
    while let Some(&byte) = bytes.get(next) {
        match byte {
            b'\n' => break,
            b'\\' if continuation(bytes, next) > 0 => {
                continued_comments |= commented;
                next += continuation(bytes, next);
            }
            b'/' if matches!(bytes.get(next + 1), Some(b'/' | b'*')) => {
                commented = true;
                next = comment_end(bytes, next);
            }
            b'"' | b'\'' => next = literal_end(bytes, next),
            _ => next += 1,
        }
    }
    (next, continued_comments)
}

/// The end of the comment at `at`: a block's close, or a line's end, its
/// continuations followed.
pub(super) fn comment_end(bytes: &[u8], at: usize) -> usize {
    let mut next = at + 2;
    match bytes.get(at + 1) {
        Some(b'*') => {
            while next < bytes.len() && bytes.get(next..next + 2) != Some(b"*/") {
                next += 1;
            }
            (next + 2).min(bytes.len())
        }
        _ => {
            while let Some(&byte) = bytes.get(next) {
                match byte {
                    b'\n' => break,
                    b'\\' if continuation(bytes, next) > 0 => next += continuation(bytes, next),
                    _ => next += 1,
                }
            }
            next
        }
    }
}

/// The end of the string or character literal at `at`, at its closing
/// quote or the line's end.
pub(super) fn literal_end(bytes: &[u8], at: usize) -> usize {
    let quote = bytes.get(at).copied();
    let mut next = at + 1;
    while let Some(&byte) = bytes.get(next) {
        match byte {
            b'\\' => next += 2,
            b'\n' => return next,
            _ if Some(byte) == quote => return next + 1,
            _ => next += 1,
        }
    }
    next.min(bytes.len())
}

/// The place of the `"` of the raw string literal that opens at `at`
/// (`R"`, `u8R"`, `LR"`, ...), when one does.
fn raw_string_start(bytes: &[u8], at: usize) -> Option<usize> {
    let prefix = match bytes.get(at..)? {
        [b'R', b'"', ..] => 1,
        [b'u', b'8', b'R', b'"', ..] => 3,
        [b'u' | b'U' | b'L', b'R', b'"', ..] => 2,
        _ => return None,
    };
    let before = at.checked_sub(1).and_then(|before| bytes.get(before));
    match before {
        Some(b) if b.is_ascii_alphanumeric() || *b == b'_' => None,
        _ => Some(at + prefix),
    }
}

/// The end of the raw string literal at `at`: past `)delimiter"`, the
/// delimiter at most 16 bytes as the language allows; at the line's end
/// for a malformed opening.
fn raw_string_end(bytes: &[u8], at: usize) -> usize {
    let Some(quote) = raw_string_start(bytes, at) else {
        return at + 1;
    };
    let rest = bytes.get(quote + 1..).unwrap_or_default();
    let Some(open) = rest.iter().take(17).position(|b| *b == b'(') else {
        return literal_end(bytes, quote);
    };
    let delimiter = rest.get(..open).unwrap_or_default();
    let body = quote + 2 + open;
    let mut next = body;
    while next < bytes.len() {
        if bytes.get(next) == Some(&b')')
            && bytes.get(next + 1..next + 1 + delimiter.len()) == Some(delimiter)
            && bytes.get(next + 1 + delimiter.len()) == Some(&b'"')
        {
            return next + 2 + delimiter.len();
        }
        next += 1;
    }
    bytes.len()
}

fn word_end(bytes: &[u8], at: usize) -> usize {
    let mut next = at;
    while bytes
        .get(next)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        next += 1;
    }
    next
}

/// The end of the number at `at`, its digit separators (`1'000`) and
/// exponents' signs in it.
fn number_end(bytes: &[u8], at: usize) -> usize {
    let mut next = at;
    while let Some(&byte) = bytes.get(next) {
        let separator = byte == b'\''
            && bytes
                .get(next + 1)
                .is_some_and(|b| b.is_ascii_alphanumeric());
        let sign = matches!(byte, b'+' | b'-')
            && next > at
            && bytes
                .get(next - 1)
                .is_some_and(|b| matches!(b, b'e' | b'E' | b'p' | b'P'));
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.') || separator || sign) {
            break;
        }
        next += 1;
    }
    next
}
