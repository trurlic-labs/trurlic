//! TOML definitions: every key path from the document root
//! (`profile::release::panic`), read with the `toml` crate's spans.
//!
//! A key's span runs from its first byte, or its table's header, to the
//! last byte of its value and of everything below it, so a table holds
//! its sub-tables wherever their headers are, and an implicit table
//! (`a` of `[a.b]`) is defined by its first key. An array of tables, or
//! of inline tables, is one definition of its key over every element, and
//! each element defines its keys under the array's path with no index:
//! a key two elements hold is two definitions, which a symbol names as
//! ambiguous.
//!
//! The crate's parser refuses a nesting past its own recursion limit, which
//! is below [`MAX_NESTING`]; the walk refuses one past that bound whatever
//! the parser allows.

use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::index::{Definition, IndexFault, MAX_NESTING};
use super::kinds::DefinitionKind;
use super::span::Lines;
use super::symbol::{Segment, name_segments};

/// The key paths of `text`.
pub(super) fn definitions(text: &str) -> Result<Vec<Definition>, IndexFault> {
    let document = DeTable::parse(text).map_err(|error| IndexFault::Syntax {
        at: error.span().map_or(0, |span| span.start),
    })?;
    let mut keys = Keys {
        text,
        lines: Lines::of(text.as_bytes()),
        definitions: Vec::new(),
        too_deep: false,
    };
    keys.table(&[], document.get_ref(), 0);
    match keys.too_deep {
        true => Err(IndexFault::TooDeep),
        false => Ok(keys.definitions),
    }
}

struct Keys<'t> {
    text: &'t str,
    lines: Lines,
    definitions: Vec<Definition>,
    too_deep: bool,
}

impl Keys<'_> {
    /// Define each key of `table` under `prefix`; the bytes the table's
    /// keys and values cover.
    fn table(
        &mut self,
        prefix: &[Segment],
        table: &DeTable<'_>,
        depth: usize,
    ) -> Option<Range<usize>> {
        let mut covered = None;
        for (key, value) in table.iter() {
            let mut path = prefix.to_vec();
            path.extend(name_segments(key.get_ref()));
            let below = self.value(&path, value, depth + 1);
            let span = join(join(Some(key.span()), Some(value.span())), below);
            if let Some(span) = &span {
                self.definitions.push(Definition::lexical(
                    path,
                    DefinitionKind::Key,
                    self.text,
                    &self.lines,
                    span.start..span.end,
                ));
            }
            covered = join(covered, span);
        }
        covered
    }

    /// Define what `value` holds under `path`; the bytes it covers.
    fn value(
        &mut self,
        path: &[Segment],
        value: &Spanned<DeValue<'_>>,
        depth: usize,
    ) -> Option<Range<usize>> {
        if depth > MAX_NESTING {
            self.too_deep = true;
            return None;
        }
        match value.get_ref() {
            DeValue::Table(table) => self.table(path, table, depth),
            DeValue::Array(elements) => elements.iter().fold(None, |covered, element| {
                let below = self.value(path, element, depth + 1);
                join(join(covered, Some(element.span())), below)
            }),
            DeValue::String(_)
            | DeValue::Integer(_)
            | DeValue::Float(_)
            | DeValue::Boolean(_)
            | DeValue::Datetime(_) => None,
        }
    }
}

/// The bytes from the first start to the last end of two ranges.
fn join(one: Option<Range<usize>>, two: Option<Range<usize>>) -> Option<Range<usize>> {
    match (one, two) {
        (Some(one), Some(two)) => Some(one.start.min(two.start)..one.end.max(two.end)),
        (one, None) => one,
        (None, two) => two,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::{FileKind, index};
    use crate::code::symbol::{Symbol, Syntax};

    const MANIFEST: &str = "\
[package]
name = 'x'

[profile.release]
panic = 'abort'
lto = true

[[bin]]
name = 'a'

[[bin]]
name = 'b'
tables = [{ key = 1 }, { key = 2 }]

[workspace]
members = ['a']

[workspace.lints.clippy]
unwrap_used = 'deny'
# panic = 'deny'

[workspace.lints.rust]
unsafe_code = 'forbid'
";

    /// The text of each definition `symbol` names in `text`.
    fn named<'t>(text: &'t str, symbol: &str) -> Vec<&'t str> {
        let symbol = Symbol::parse(symbol, Syntax::Plain).unwrap();
        index(FileKind::Toml, text.as_bytes())
            .unwrap()
            .definitions
            .iter()
            .filter(|d| symbol.names(&d.path))
            .map(|d| &text[d.span.start..d.span.end])
            .collect()
    }

    #[test]
    fn a_key_is_named_by_any_end_of_its_path() {
        assert_eq!(
            named(MANIFEST, "profile::release::panic"),
            ["panic = 'abort'"]
        );
        assert_eq!(named(MANIFEST, "release::panic"), ["panic = 'abort'"]);
        assert_eq!(named(MANIFEST, "lto"), ["lto = true"]);
        assert_eq!(named(MANIFEST, "unsafe_code"), ["unsafe_code = 'forbid'"]);
        assert!(named(MANIFEST, "release::lto::x").is_empty());
        assert!(named(MANIFEST, "package::panic").is_empty());
        assert!(
            named(MANIFEST, "clippy::panic").is_empty(),
            "a comment defines nothing"
        );
    }

    #[test]
    fn a_table_spans_its_header_and_every_table_below_it() {
        assert_eq!(
            named(MANIFEST, "profile::release"),
            ["[profile.release]\npanic = 'abort'\nlto = true"]
        );
        let lints = named(MANIFEST, "workspace::lints");
        assert_eq!(lints.len(), 1, "{lints:?}");
        assert!(
            lints[0].starts_with("[workspace.lints.clippy]"),
            "{lints:?}"
        );
        assert!(lints[0].ends_with("unsafe_code = 'forbid'"));
        assert!(named(MANIFEST, "workspace")[0].starts_with("[workspace]\nmembers"));
    }

    #[test]
    fn each_element_of_an_array_of_tables_defines_its_keys() {
        assert_eq!(named(MANIFEST, "bin::name"), ["name = 'a'", "name = 'b'"]);
        assert_eq!(named(MANIFEST, "bin").len(), 1);
        assert!(named(MANIFEST, "bin")[0].starts_with("[[bin]]\nname = 'a'"));
        assert_eq!(named(MANIFEST, "tables::key"), ["key = 1", "key = 2"]);
    }

    #[test]
    fn a_quoted_key_is_one_segment_unless_it_holds_the_separator() {
        let text = "\"a.b\" = 1\n\"c::d\" = 2\n";

        assert_eq!(named(text, "a.b"), ["\"a.b\" = 1"]);
        assert_eq!(named(text, "c::d"), ["\"c::d\" = 2"]);
        assert_eq!(named(text, "d"), ["\"c::d\" = 2"]);
    }

    #[test]
    fn a_document_that_does_not_parse_has_no_index() {
        assert!(matches!(
            index(FileKind::Toml, b"a = 1\na = 2\n"),
            Err(IndexFault::Syntax { at }) if at > 0
        ));
        let deep = format!("a = {}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(
            index(FileKind::Toml, deep.as_bytes()),
            Err(IndexFault::Syntax { .. })
        ));
    }
}
