//! Where a definition is in its file, and the hash of what it holds.
//!
//! A [`SpanHash`] is the hash of a span's normalized text, named by its
//! scheme ([`HASH_SCHEME`]): a change of the normalization is a new
//! scheme, so two hashes of different schemes are never compared as
//! equal. Under `t1` a tree-sitter span hashes its leaf tokens, comments
//! dropped, each with its length, so reformatting the span or editing a
//! comment in it changes nothing; a lexical span hashes its text with
//! every run of whitespace made one space and the ends trimmed.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

/// The normalization a [`SpanHash`] was computed under.
pub(crate) const HASH_SCHEME: &str = "t1";

/// A definition's place in its file: bytes `start..end`, on lines
/// `first_line..=last_line`, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lines {}-{} (bytes {}..{})",
            self.first_line, self.last_line, self.start, self.end
        )
    }
}

/// The line of each byte offset of one text.
pub(super) struct Lines {
    /// The offset each line starts at; the first is 0.
    starts: Vec<usize>,
}

impl Lines {
    pub(super) fn of(text: &[u8]) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.iter()
                .enumerate()
                .filter(|&(_, &byte)| byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        Self { starts }
    }

    /// The span of bytes `start..end`; its last line holds the byte before
    /// `end`, so a span ending with a line break ends on that line.
    pub(super) fn span(&self, start: usize, end: usize) -> Span {
        let line = |at: usize| self.starts.partition_point(|&s| s <= at);
        Span {
            start,
            end,
            first_line: line(start),
            last_line: line(end.saturating_sub(1).max(start)),
        }
    }
}

/// The hash of a span's normalized text under [`HASH_SCHEME`], written
/// `t1:` and 64 hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SpanHash([u8; 32]);

impl SpanHash {
    /// The hash of `tokens`, each a leaf token of a parse tree with the
    /// comments left out, in order.
    pub(super) fn of_tokens<'a>(tokens: impl Iterator<Item = &'a [u8]>) -> Self {
        let mut hasher = blake3::Hasher::new();
        for token in tokens {
            // A length before each token: `a b` and `ab` hash apart.
            hasher.update(&(token.len() as u64).to_le_bytes());
            hasher.update(token);
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// The hash of `text` with its runs of whitespace made one space and
    /// its ends trimmed.
    pub(super) fn of_text(text: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        for (n, word) in text.split_whitespace().enumerate() {
            if n > 0 {
                hasher.update(b" ");
            }
            hasher.update(word.as_bytes());
        }
        Self(*hasher.finalize().as_bytes())
    }
}

impl fmt::Display for SpanHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{HASH_SCHEME}:{}",
            blake3::Hash::from_bytes(self.0).to_hex()
        )
    }
}

/// A text that is not `t1:` and 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotASpanHash;

impl fmt::Display for NotASpanHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not `{HASH_SCHEME}:` and 64 lowercase hex digits")
    }
}

impl FromStr for SpanHash {
    type Err = NotASpanHash;

    fn from_str(text: &str) -> Result<Self, NotASpanHash> {
        text.strip_prefix(HASH_SCHEME)
            .and_then(|rest| rest.strip_prefix(':'))
            .filter(|hex| !hex.bytes().any(|byte| byte.is_ascii_uppercase()))
            .and_then(|hex| blake3::Hash::from_hex(hex).ok())
            .map(|hash| Self(*hash.as_bytes()))
            .ok_or(NotASpanHash)
    }
}

impl Serialize for SpanHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SpanHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <&str>::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_names_the_lines_it_covers_counted_from_one() {
        let lines = Lines::of(b"a\nbc\n\nd");

        assert_eq!(
            lines.span(0, 1),
            Span {
                start: 0,
                end: 1,
                first_line: 1,
                last_line: 1
            }
        );
        assert_eq!(
            lines.span(2, 5),
            Span {
                start: 2,
                end: 5,
                first_line: 2,
                last_line: 2
            }
        );
        assert_eq!(
            lines.span(2, 7),
            Span {
                start: 2,
                end: 7,
                first_line: 2,
                last_line: 4
            }
        );
        assert_eq!(lines.span(6, 6).last_line, 4);
    }

    #[test]
    fn a_token_hash_sees_each_token_boundary() {
        let split = SpanHash::of_tokens([&b"a"[..], b"b"].into_iter());
        let joined = SpanHash::of_tokens([&b"ab"[..]].into_iter());

        assert_ne!(split, joined);
    }

    #[test]
    fn a_text_hash_ignores_how_whitespace_is_laid_out() {
        assert_eq!(
            SpanHash::of_text("ci: test\n\tcargo  test\n"),
            SpanHash::of_text("  ci: test \n\n\tcargo test")
        );
        assert_ne!(SpanHash::of_text("ci: test"), SpanHash::of_text("ci:test"));
    }

    #[test]
    fn a_hash_reads_back_from_its_text_and_refuses_another_scheme() {
        let hash = SpanHash::of_text("x");
        let text = hash.to_string();

        assert!(text.starts_with("t1:"));
        assert_eq!(text.parse(), Ok(hash));
        assert_eq!(
            text.replace("t1:", "t2:").parse::<SpanHash>(),
            Err(NotASpanHash)
        );
        assert_eq!(text.to_uppercase().parse::<SpanHash>(), Err(NotASpanHash));
        assert_eq!(
            text[..text.len() - 1].parse::<SpanHash>(),
            Err(NotASpanHash)
        );
    }
}
