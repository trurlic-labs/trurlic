//! Where a definition is in its file, and the hash of what it holds.
//!
//! A [`SpanHash`] is the hash of a span's normalized text, named by its
//! scheme ([`HASH_SCHEME`]), fixed from its first release. A change of
//! the normalization is a new scheme, so two hashes of different schemes
//! are never compared as equal. Under `t1` a tree-sitter span hashes its
//! tokens (`parse/tokens.rs`), each with its length, and in a language whose
//! layout is syntax the marks that open and close each group its query
//! names (a statement, a block), each definition nested in it by its own
//! hash instead. So reformatting the span or editing a comment in it
//! changes nothing. A lexical span hashes its text with every run of
//! whitespace made one space and the ends trimmed. The definitions of
//! one path a symbol names hash as one
//! ([`SpanHash::of_members`]): one alone by its own hash, several by the
//! hash of their hashes in the order of the hashes.

use std::fmt;
use std::str::FromStr;

/// The normalization a [`SpanHash`] was computed under.
pub(crate) const HASH_SCHEME: &str = "t1";

/// The mark before a nested definition's hash among a definition's
/// units: a length no token has.
pub(in crate::code) const NESTED_MARK: [u8; 8] = u64::MAX.to_le_bytes();

/// The units that open and close a group of a marked tree (a statement, a
/// block) in a hash: lengths no token has, nor [`NESTED_MARK`].
pub(in crate::code) const OPEN_MARK: [u8; 8] = (u64::MAX - 1).to_le_bytes();
pub(in crate::code) const CLOSE_MARK: [u8; 8] = (u64::MAX - 2).to_le_bytes();

/// A definition's place in its file: bytes `start..end`, on lines
/// `first_line..=last_line`, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Span {
    pub(in crate::code) start: usize,
    pub(in crate::code) end: usize,
    pub(crate) first_line: usize,
    pub(in crate::code) last_line: usize,
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
pub(in crate::code) struct Lines {
    /// The offset each line starts at; the first is 0.
    starts: Vec<usize>,
}

impl Lines {
    pub(in crate::code) fn of(text: &[u8]) -> Self {
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
    pub(in crate::code) fn span(&self, start: usize, end: usize) -> Span {
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
    /// The hash of `units`, a definition's tokens and nested hashes, each
    /// after its length mark (`Leaves::hash_each`), so `a b` and `ab` hash
    /// apart.
    pub(in crate::code) fn of_units(units: &[u8]) -> Self {
        Self(*blake3::hash(units).as_bytes())
    }

    /// The hash of the definitions of one path, `members` sorted:
    /// one alone is its own; several are hashed as a definition's units
    /// would be that held them and no token of its own, each hash after
    /// [`NESTED_MARK`], so no single definition's hash, which holds its
    /// tokens, is one of a set's.
    pub(in crate::code) fn of_members(members: &[Self]) -> Self {
        match members {
            [one] => *one,
            several => {
                let mut units = Vec::with_capacity(several.len() * 40);
                for member in several {
                    units.extend_from_slice(&NESTED_MARK);
                    units.extend_from_slice(&member.0);
                }
                Self::of_units(&units)
            }
        }
    }

    /// The hash its 32 bytes are.
    pub(in crate::code) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(in crate::code) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The hash of `text` with its runs of whitespace made one space and
    /// its ends trimmed.
    pub(in crate::code) fn of_text(text: &str) -> Self {
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
    fn a_set_of_one_is_its_member_and_of_several_is_none_of_them() {
        let (a, b) = (SpanHash::of_text("a"), SpanHash::of_text("b"));
        let both = SpanHash::of_members(&[a, b]);

        assert_eq!(SpanHash::of_members(&[a]), a);
        assert_ne!(both, a);
        assert_ne!(both, b);
        assert_ne!(
            both,
            SpanHash::of_members(&[b, a]),
            "the span order is part of it"
        );
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
