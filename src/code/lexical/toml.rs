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

use super::Lexical;
use crate::code::index::{DefinitionKind, FileIndex, IndexFault, MAX_NESTING, ScopeId, ScopeTree};

/// The index of `text`, a TOML file: its key paths.
pub(in crate::code) fn index(text: &str, scopes: ScopeTree) -> Result<FileIndex, IndexFault> {
    let document = DeTable::parse(text).map_err(|error| IndexFault::Syntax {
        at: error.span().map_or(0, |span| span.start),
    })?;
    let mut keys = Keys {
        lexical: Lexical::of(text, scopes),
    };
    keys.table(None, document.get_ref(), 0)?;
    Ok(keys.lexical.finish())
}

struct Keys<'t> {
    lexical: Lexical<'t>,
}

impl Keys<'_> {
    /// Define each key of `table` under `parent`; the bytes the table's
    /// keys and values cover.
    fn table(
        &mut self,
        parent: Option<ScopeId>,
        table: &DeTable<'_>,
        depth: usize,
    ) -> Result<Option<Range<usize>>, IndexFault> {
        let mut covered = None;
        for (key, value) in table.iter() {
            let scope = self.lexical.scope(parent, key.get_ref())?;
            let below = self.value(scope, value, depth + 1)?;
            let span = join(join(Some(key.span()), Some(value.span())), below);
            if let Some(span) = &span {
                let hash = self.lexical.hash(span.start..span.end)?;
                self.lexical
                    .define(scope, DefinitionKind::Key, span.start..span.end, hash)?;
            }
            covered = join(covered, span);
        }
        Ok(covered)
    }

    /// Define what `value` holds under `scope`; the bytes it covers. The
    /// recursion is bounded by [`MAX_NESTING`], below the parser's own
    /// limit.
    fn value(
        &mut self,
        scope: ScopeId,
        value: &Spanned<DeValue<'_>>,
        depth: usize,
    ) -> Result<Option<Range<usize>>, IndexFault> {
        if depth > MAX_NESTING {
            return Err(IndexFault::TooDeep);
        }
        Ok(match value.get_ref() {
            DeValue::Table(table) => self.table(Some(scope), table, depth)?,
            DeValue::Array(elements) => {
                let mut covered = None;
                for element in elements {
                    let below = self.value(scope, element, depth + 1)?;
                    covered = join(join(covered, Some(element.span())), below);
                }
                covered
            }
            DeValue::String(_)
            | DeValue::Integer(_)
            | DeValue::Float(_)
            | DeValue::Boolean(_)
            | DeValue::Datetime(_) => None,
        })
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
    use crate::code::index::FileKind;
    use crate::code::testing;

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
        testing::named(FileKind::Toml, text, symbol)
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
    fn a_path_past_the_bound_of_segments_is_refused() {
        let header =
            |segments: usize| format!("[\"{}\"]\nkey = 1\n", vec!["a"; segments].join("::"));

        assert!(
            crate::code::testing::index(FileKind::Toml, header(MAX_NESTING - 1).as_bytes()).is_ok()
        );
        assert_eq!(
            crate::code::testing::index(FileKind::Toml, header(MAX_NESTING).as_bytes()),
            Err(IndexFault::TooDeep)
        );
    }

    #[test]
    fn the_bytes_hashed_are_held_to_128_a_byte_of_the_file() {
        // Each table spans to its sub-table, past every header between, so
        // 600 interleaved tables hash their headers hundreds of times over.
        // A key hashed once before them dilutes that: one byte shorter it
        // is past the bound, and with that byte it is within.
        let headers: String = (0..600).map(|n| format!("[x{n}]\n")).collect();
        let below: String = (0..600).map(|n| format!("[x{n}.z]\n")).collect();
        let text = |value: usize| format!("p = \"{}\"\n{headers}{below}", "a".repeat(value));

        assert!(crate::code::testing::index(FileKind::Toml, text(12_766).as_bytes()).is_ok());
        assert_eq!(
            crate::code::testing::index(FileKind::Toml, text(12_765).as_bytes()),
            Err(IndexFault::TooComplex)
        );
    }

    #[test]
    fn a_document_that_does_not_parse_has_no_index() {
        assert!(matches!(
            crate::code::testing::index(FileKind::Toml, b"a = 1\na = 2\n"),
            Err(IndexFault::Syntax { at }) if at > 0
        ));
        let deep = format!("a = {}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(
            crate::code::testing::index(FileKind::Toml, deep.as_bytes()),
            Err(IndexFault::Syntax { .. })
        ));
    }
}
