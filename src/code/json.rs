//! JSON definitions: every key path from the document root, from a
//! scanner of its own that keeps each key's span, from the key to the end
//! of its value. An array's elements define their keys under the array's
//! path with no index, as an array of tables does in TOML (`toml.rs`).
//!
//! The scanner holds the containers it is in on a stack of its own, up to
//! [`MAX_NESTING`], and refuses anything that is not JSON (RFC 8259) at
//! the byte it fails at.

use super::index::{FileIndex, IndexFault};
use super::kinds::DefinitionKind;
use super::lexical::Lexical;
use super::scope::{MAX_NESTING, ScopeId};

/// The index of `text`, a JSON file: its key paths.
pub(super) fn index(text: &str) -> Result<FileIndex, IndexFault> {
    let mut scanner = Scanner {
        text,
        bytes: text.as_bytes(),
        at: 0,
        open: Vec::new(),
        lexical: Lexical::of(text),
    };
    scanner.document()?;
    Ok(scanner.lexical.finish())
}

/// A container the scanner is in.
enum Open {
    /// An object, and the member whose value is being read: its path and
    /// where its key starts.
    Object {
        path: Option<ScopeId>,
        member: Option<(ScopeId, usize)>,
    },
    Array {
        path: Option<ScopeId>,
    },
}

struct Scanner<'t> {
    text: &'t str,
    bytes: &'t [u8],
    at: usize,
    open: Vec<Open>,
    lexical: Lexical<'t>,
}

impl Scanner<'_> {
    fn document(&mut self) -> Result<(), IndexFault> {
        let mut path = None;
        loop {
            self.skip_space();
            let Some(end) = self.value(path)? else {
                path = self.next_path()?;
                continue;
            };
            match self.close_values(end)? {
                Some(next) => path = next,
                None => break,
            }
        }
        self.skip_space();
        match self.at == self.bytes.len() {
            true => Ok(()),
            false => Err(self.fault()),
        }
    }

    /// Read the value at the cursor: its end when it is a scalar or an
    /// empty container, `None` after opening a container with something in
    /// it.
    fn value(&mut self, path: Option<ScopeId>) -> Result<Option<usize>, IndexFault> {
        match self.bytes.get(self.at) {
            Some(b'{') | Some(b'[') => {
                let object = self.bytes.get(self.at) == Some(&b'{');
                self.at += 1;
                self.skip_space();
                let close = if object { b'}' } else { b']' };
                if self.bytes.get(self.at) == Some(&close) {
                    self.at += 1;
                    return Ok(Some(self.at));
                }
                if self.open.len() == MAX_NESTING {
                    return Err(IndexFault::TooDeep);
                }
                self.open.push(match object {
                    true => Open::Object { path, member: None },
                    false => Open::Array { path },
                });
                Ok(None)
            }
            Some(b'"') => self.string().map(|_| Some(self.at)),
            Some(b't') => self.word("true").map(Some),
            Some(b'f') => self.word("false").map(Some),
            Some(b'n') => self.word("null").map(Some),
            Some(b'-' | b'0'..=b'9') => self.number().map(Some),
            _ => Err(self.fault()),
        }
    }

    /// The path of the next value in the innermost container, opened or
    /// past a `,`: an element's, or a member's, whose key it reads.
    fn next_path(&mut self) -> Result<Option<ScopeId>, IndexFault> {
        match self.open.last_mut() {
            Some(Open::Array { path }) => Ok(*path),
            Some(Open::Object { .. }) => self.key(),
            None => Err(self.fault()),
        }
    }

    /// Read a member's key and its `:`; the path of its value.
    fn key(&mut self) -> Result<Option<ScopeId>, IndexFault> {
        self.skip_space();
        let start = self.at;
        let key = self.string()?;
        self.skip_space();
        self.expect(b':')?;
        let Some(Open::Object { path, .. }) = self.open.last() else {
            return Err(self.fault());
        };
        let scope = self.lexical.scope(*path, &key)?;
        if let Some(Open::Object { member, .. }) = self.open.last_mut() {
            *member = Some((scope, start));
        }
        Ok(Some(scope))
    }

    /// A value ended at `end`: define the member it is the value of, and
    /// close each container it ends; the path of the next value, or `None`
    /// when the document's value is read.
    fn close_values(&mut self, mut end: usize) -> Result<Option<Option<ScopeId>>, IndexFault> {
        loop {
            let close = match self.open.last_mut() {
                None => return Ok(None),
                Some(Open::Object { member, .. }) => {
                    if let Some((scope, start)) = member.take() {
                        let hash = self.lexical.hash(start..end)?;
                        self.lexical
                            .define(scope, DefinitionKind::Key, start..end, hash)?;
                    }
                    b'}'
                }
                Some(Open::Array { .. }) => b']',
            };
            self.skip_space();
            match self.bytes.get(self.at) {
                Some(b',') => {
                    self.at += 1;
                    return self.next_path().map(Some);
                }
                Some(&byte) if byte == close => {
                    self.at += 1;
                    end = self.at;
                    self.open.pop();
                }
                _ => return Err(self.fault()),
            }
        }
    }

    /// Read a string; its text, escapes decoded.
    fn string(&mut self) -> Result<String, IndexFault> {
        self.expect(b'"')?;
        let mut decoded = String::new();
        loop {
            let run = self.at;
            while let Some(&byte) = self.bytes.get(self.at) {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.at += 1;
            }
            decoded.push_str(self.text.get(run..self.at).unwrap_or_default());
            match self.bytes.get(self.at) {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(decoded);
                }
                Some(b'\\') => decoded.push(self.escape()?),
                _ => return Err(self.fault()),
            }
        }
    }

    /// Read an escape after its `\`; the character it stands for, a lone
    /// surrogate as U+FFFD.
    fn escape(&mut self) -> Result<char, IndexFault> {
        self.at += 1;
        let Some(&kind) = self.bytes.get(self.at) else {
            return Err(self.fault());
        };
        self.at += 1;
        Ok(match kind {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(),
            _ => return Err(self.fault()),
        })
    }

    fn unicode_escape(&mut self) -> Result<char, IndexFault> {
        let high = self.hex4()?;
        if !(0xD800..0xDC00).contains(&high) {
            return Ok(char::from_u32(high).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
        if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
            return Ok(char::REPLACEMENT_CHARACTER);
        }
        self.at += 2;
        let low = self.hex4()?;
        let pair = 0x10000 + ((high - 0xD800) << 10) + low.wrapping_sub(0xDC00);
        Ok(match (0xDC00..0xE000).contains(&low) {
            true => char::from_u32(pair).unwrap_or(char::REPLACEMENT_CHARACTER),
            false => char::REPLACEMENT_CHARACTER,
        })
    }

    fn hex4(&mut self) -> Result<u32, IndexFault> {
        let value = self.text.get(self.at..self.at + 4).and_then(|digits| {
            digits
                .chars()
                .try_fold(0, |value, digit| Some(value * 16 + digit.to_digit(16)?))
        });
        let value = value.ok_or_else(|| self.fault())?;
        self.at += 4;
        Ok(value)
    }

    /// Read a number; where it ends.
    fn number(&mut self) -> Result<usize, IndexFault> {
        let digits = |scanner: &mut Self| {
            let start = scanner.at;
            while scanner
                .bytes
                .get(scanner.at)
                .is_some_and(u8::is_ascii_digit)
            {
                scanner.at += 1;
            }
            scanner.at - start
        };
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        match self.bytes.get(self.at) {
            // A leading zero is the whole integer part.
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.fault()),
        }
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            if digits(self) == 0 {
                return Err(self.fault());
            }
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digits(self) == 0 {
                return Err(self.fault());
            }
        }
        Ok(self.at)
    }

    /// Read `word`; where it ends.
    fn word(&mut self, word: &str) -> Result<usize, IndexFault> {
        match self.bytes.get(self.at..self.at + word.len()) == Some(word.as_bytes()) {
            true => {
                self.at += word.len();
                Ok(self.at)
            }
            false => Err(self.fault()),
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), IndexFault> {
        match self.bytes.get(self.at) == Some(&byte) {
            true => {
                self.at += 1;
                Ok(())
            }
            false => Err(self.fault()),
        }
    }

    fn skip_space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn fault(&self) -> IndexFault {
        IndexFault::Syntax { at: self.at }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::{FileKind, index};
    use crate::code::testing;

    const DOCUMENT: &str = r#"{
  "stages": {"order": ["a", "b"], "parallel": true},
  "items": [{"name": 1}, {"name": -2.5e3}, [], {}],
  "esc\"aped\u00e9": null,
  "empty": {}
}"#;

    fn named<'t>(text: &'t str, symbol: &str) -> Vec<&'t str> {
        testing::named(FileKind::Json, text, symbol)
    }

    #[test]
    fn a_key_spans_its_value_and_is_named_by_any_end_of_its_path() {
        assert_eq!(named(DOCUMENT, "stages::order"), [r#""order": ["a", "b"]"#]);
        assert_eq!(named(DOCUMENT, "parallel"), [r#""parallel": true"#]);
        assert!(named(DOCUMENT, "stages")[0].ends_with("true}"));
        assert_eq!(named(DOCUMENT, "empty"), [r#""empty": {}"#]);
        assert_eq!(named(DOCUMENT, "esc\"aped\u{e9}").len(), 1);
        assert!(named(DOCUMENT, "order::stages").is_empty());
    }

    #[test]
    fn each_element_of_an_array_defines_its_keys() {
        assert_eq!(
            named(DOCUMENT, "items::name"),
            [r#""name": 1"#, r#""name": -2.5e3"#]
        );
    }

    #[test]
    fn a_document_that_is_not_json_is_refused_where_it_fails() {
        for (text, at) in [
            ("", 0),
            ("{", 1),
            ("{\"a\" 1}", 5),
            ("{\"a\": 1,}", 8),
            ("[1 2]", 3),
            ("01", 1),
            ("1.", 2),
            ("\"\\x\"", 3),
            ("\"a\u{1}\"", 2),
            ("{} {}", 3),
            ("tru", 0),
        ] {
            assert_eq!(
                index(
                    FileKind::Json,
                    text.as_bytes(),
                    &crate::code::BlankMacros::default()
                ),
                Err(IndexFault::Syntax { at }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_nesting_past_the_bound_is_refused() {
        let nested = |depth: usize| format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));

        let indexed = |text: String| {
            index(
                FileKind::Json,
                text.as_bytes(),
                &crate::code::BlankMacros::default(),
            )
            .map(|i| i.definitions.len())
        };

        assert_eq!(indexed(nested(MAX_NESTING)), Ok(MAX_NESTING));
        assert_eq!(indexed(nested(MAX_NESTING + 1)), Err(IndexFault::TooDeep));
        assert_eq!(indexed(nested(500_000)), Err(IndexFault::TooDeep));
    }
}
