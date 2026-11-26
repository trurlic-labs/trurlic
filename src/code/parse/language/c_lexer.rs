//! A C or C++ file's bytes read as the preprocessor reads them before
//! it expands anything: directives (each over its continued lines), comments,
//! literals, identifiers and other tokens, in one pass in which each byte
//! is read a bounded number of times.

use std::ops::Range;

/// What the lexer meets, by byte range.
#[derive(Debug, Clone, Copy)]
pub(super) enum Item {
    /// A preprocessor directive, its continued lines and comments in it
    /// included; whether a token of it follows one of its comments.
    Directive {
        start: usize,
        end: usize,
        inner_comments: bool,
    },
    Comment {
        start: usize,
        end: usize,
    },
    Identifier {
        start: usize,
        end: usize,
    },
    /// A literal, a number or a punctuator, one of several bytes
    /// ([`PUNCTUATORS`]) read whole.
    Token {
        start: usize,
        end: usize,
    },
}

impl Item {
    /// Where it starts.
    pub(super) const fn start(self) -> usize {
        match self {
            Self::Directive { start, .. }
            | Self::Comment { start, .. }
            | Self::Identifier { start, .. }
            | Self::Token { start, .. } => start,
        }
    }
}

/// Hand `each` the range of each token of `text`, a C or C++ text read
/// with no directive in it (a macro's body): each identifier, literal,
/// number and punctuation mark, comments and continuations left out.
pub(super) fn tokens(text: &str, each: &mut dyn FnMut(Range<usize>)) {
    lex(text, false, |item| match item {
        Item::Token { start, end } | Item::Identifier { start, end } => each(start..end),
        Item::Directive { .. } | Item::Comment { .. } => {}
    });
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
            let (end, inner_comments) = directive_end(bytes, at);
            let directive = Item::Directive {
                start: at,
                end,
                inner_comments,
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
        _ => {
            let rest = text.get(at..).unwrap_or_default();
            let length = match PUNCTUATORS.iter().find(|p| rest.starts_with(**p)) {
                Some(punctuator) => punctuator.len(),
                None => rest.chars().next().map_or(1, char::len_utf8),
            };
            token(at + length)
        }
    }
}

/// The punctuators of C and C++ longer than one byte, each one token as
/// the language lexes it, longest first, so a read takes the longest that
/// starts at its place.
const PUNCTUATORS: [&str; 27] = [
    "<<=", ">>=", "...", "->*", "<=>", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&",
    "||", "##", "::", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", ".*",
];

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
/// comments that run past it), and whether a token of it follows one of
/// its comments.
fn directive_end(bytes: &[u8], at: usize) -> (usize, bool) {
    let mut next = at + 1;
    let (mut commented, mut inner_comments) = (false, false);
    while let Some(&byte) = bytes.get(next) {
        match byte {
            b'\n' => break,
            b'\\' if continuation(bytes, next) > 0 => next += continuation(bytes, next),
            b'/' if matches!(bytes.get(next + 1), Some(b'/' | b'*')) => {
                commented = true;
                next = comment_end(bytes, next);
            }
            b' ' | b'\t' | b'\r' | b'\x0c' | b'\x0b' => next += 1,
            _ => {
                inner_comments |= commented;
                next = match byte {
                    b'"' | b'\'' => literal_end(bytes, next),
                    _ => next + 1,
                };
            }
        }
    }
    (next, inner_comments)
}

/// The end of the comment at `at`: a block's close, or a line's end, its
/// continuations followed.
fn comment_end(bytes: &[u8], at: usize) -> usize {
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
fn literal_end(bytes: &[u8], at: usize) -> usize {
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
/// (`R"`, `u8R"`, `LR"`, ...), when one does. An item starts at `at`, so
/// no identifier's character stands right before it.
fn raw_string_start(bytes: &[u8], at: usize) -> Option<usize> {
    let prefix = match bytes.get(at..)? {
        [b'R', b'"', ..] => 1,
        [b'u', b'8', b'R', b'"', ..] => 3,
        [b'u' | b'U' | b'L', b'R', b'"', ..] => 2,
        _ => return None,
    };
    Some(at + prefix)
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

/// The end of the identifier at `at`; a line continuation between two of
/// its characters is in it, as the preprocessor splices it.
fn word_end(bytes: &[u8], at: usize) -> usize {
    let mut next = at;
    loop {
        match bytes.get(next) {
            Some(byte) if is_word(*byte) => next += 1,
            Some(b'\\') if spliced(bytes, next).is_some_and(is_word) => {
                next += continuation(bytes, next);
            }
            _ => return next,
        }
    }
}

const fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The byte after the line continuation at `at`, when one is there.
fn spliced(bytes: &[u8], at: usize) -> Option<u8> {
    match continuation(bytes, at) {
        0 => None,
        length => bytes.get(at + length).copied(),
    }
}

/// The end of the number at `at`, its digit separators (`1'000`) and
/// exponents' signs in it, and a line continuation between two of its
/// characters.
fn number_end(bytes: &[u8], at: usize) -> usize {
    let mut next = at;
    while let Some(&byte) = bytes.get(next) {
        if byte == b'\\' && spliced(bytes, next).is_some_and(is_word) {
            next += continuation(bytes, next);
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Each item of `text`, by its kind and text.
    fn items(text: &str) -> Vec<(&'static str, &str)> {
        let mut found = Vec::new();
        lex(text, true, |item| {
            let (kind, start, end) = match item {
                Item::Directive { start, end, .. } => ("directive", start, end),
                Item::Comment { start, end } => ("comment", start, end),
                Item::Identifier { start, end } => ("identifier", start, end),
                Item::Token { start, end } => ("token", start, end),
            };
            found.push((kind, &text[start..end]));
        });
        found
    }

    #[test]
    fn a_digit_separator_is_part_of_its_number() {
        assert_eq!(
            items("n = 1'000; c = 'a';"),
            [
                ("identifier", "n"),
                ("token", "="),
                ("token", "1'000"),
                ("token", ";"),
                ("identifier", "c"),
                ("token", "="),
                ("token", "'a'"),
                ("token", ";"),
            ]
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_its_literal() {
        assert_eq!(
            items(r#"s = "\"" x"#),
            [
                ("identifier", "s"),
                ("token", "="),
                ("token", r#""\"""#),
                ("identifier", "x"),
            ]
        );
    }

    #[test]
    fn a_raw_string_ends_at_its_delimiter() {
        assert_eq!(
            items(r#"s = R"x(a)y" b)x" t"#),
            [
                ("identifier", "s"),
                ("token", "="),
                ("token", r#"R"x(a)y" b)x""#),
                ("identifier", "t"),
            ]
        );
    }

    #[test]
    fn a_comment_opener_in_a_directives_literal_opens_no_comment() {
        assert_eq!(
            items("#define Q \"/*\"\nint x; /* */\n"),
            [
                ("directive", "#define Q \"/*\""),
                ("identifier", "int"),
                ("identifier", "x"),
                ("token", ";"),
                ("comment", "/* */"),
            ]
        );
    }

    #[test]
    fn a_directive_may_be_indented() {
        assert_eq!(
            items("  #  define X 1\nX"),
            [("directive", "#  define X 1"), ("identifier", "X")]
        );
        assert_eq!(
            items("a #define X\n"),
            [
                ("identifier", "a"),
                ("token", "#"),
                ("identifier", "define"),
                ("identifier", "X"),
            ]
        );
    }

    #[test]
    fn a_line_continuation_continues_a_line_comment_and_a_directive() {
        for line_break in ["\n", "\r\n"] {
            let text = format!("// a \\{line_break} b\n#define A \\{line_break} c\nx");
            let comment_end = text.find("\n#").unwrap();
            let directive_end = text.rfind('\n').unwrap();

            assert_eq!(
                items(&text),
                [
                    ("comment", &text[..comment_end]),
                    ("directive", &text[comment_end + 1..directive_end]),
                    ("identifier", "x"),
                ]
            );
        }
    }

    #[test]
    fn a_line_continuation_inside_a_word_or_a_number_joins_its_halves() {
        assert_eq!(
            items("ab\\\ncd 12\\\n34 e \\\nf"),
            [
                ("identifier", "ab\\\ncd"),
                ("token", "12\\\n34"),
                ("identifier", "e"),
                ("identifier", "f"),
            ]
        );
    }

    #[test]
    fn a_punctuator_of_several_bytes_is_one_token() {
        let tokens = |text| -> Vec<&str> { items(text).into_iter().map(|(_, t)| t).collect() };

        assert_eq!(
            tokens("a->b && c ... d ## e <<= f :: g"),
            [
                "a", "->", "b", "&&", "c", "...", "d", "##", "e", "<<=", "f", "::", "g"
            ]
        );
        assert_eq!(tokens("a & &b"), ["a", "&", "&", "b"]);
        assert_eq!(tokens("a- >b"), ["a", "-", ">", "b"]);
    }

    #[test]
    fn a_directive_states_whether_a_token_follows_a_comment_in_it() {
        let inner = |text| {
            matches!(
                lex_first(text),
                Item::Directive {
                    inner_comments: true,
                    ..
                }
            )
        };

        assert!(inner("#define M(a) ((a) /* c */ + 1)"));
        assert!(inner("#define M(a) ((a) + \\\n  /* slack */ 1)"));
        assert!(!inner("#define M(a) ((a) + 1) /* trailing */ // and this"));
    }

    /// The first item of `text`.
    fn lex_first(text: &str) -> Item {
        let mut first = None;
        lex(text, true, |item| {
            first.get_or_insert(item);
        });
        first.unwrap()
    }

    #[test]
    fn a_block_comment_left_open_runs_to_the_end() {
        assert_eq!(items("a /* b"), [("identifier", "a"), ("comment", "/* b")]);
    }
}
