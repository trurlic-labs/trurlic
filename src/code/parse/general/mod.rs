//! The general resolver: the definitions of a file of any language a
//! query file describes (`language/`, `query/`), in one walk of its
//! tree and one sweep of what the walk found.
//!
//! The walk (`walk.rs`) visits each node once and records what each
//! pattern captures, each node's frame keeping what its ancestors say of
//! it (`frame.rs`). The sweep (`place.rs`) places the definitions and the
//! unnamed scopes by the nodes they captured (`site.rs`). A definition's
//! path is its container's and its name's segments (`name.rs`).

mod frame;
mod name;
mod place;
mod site;
mod walk;

use super::language::Language;
use super::parser;
use super::tree_index::TreeIndex;
use super::units::Leaves;
use crate::code::index::{FileIndex, IndexFault, Lines, ScopeTree};

use walk::collect;

pub(in crate::code) use walk::MATCH_LIMIT;

/// A file as the resolver reads it: the text its grammar parses and its
/// hash reads, the text as written, of the same length, whose lines a
/// span counts, and the places a rewrite before the parse marked private,
/// in order (`language/c_family/`).
pub(in crate::code) struct Source<'t> {
    pub(in crate::code) parsed: &'t str,
    pub(in crate::code) written: &'t str,
    pub(in crate::code) private: &'t [usize],
}

impl<'t> Source<'t> {
    /// `text` read as written.
    pub(in crate::code) const fn plain(text: &'t str) -> Self {
        Self {
            parsed: text,
            written: text,
            private: &[],
        }
    }
}

/// The index of `source`, a file of `language`, built into `scopes`.
pub(in crate::code) fn index(
    language: &Language,
    source: &Source<'_>,
    mut scopes: ScopeTree,
) -> Result<FileIndex, IndexFault> {
    let text = source.parsed;
    for bound in language.before_parse {
        bound(text)?;
    }
    // Each language's query is compiled against the contract in its
    // test, so a fault here is a build's, which no file's bytes cause and
    // whose cause the test names.
    let Ok(rules) = language.rules() else {
        return Err(IndexFault::Grammar);
    };
    let tree = parser::parse(text, &rules.grammar)?;
    let mut found = collect(rules, &tree, text)?;
    found.private = source.private;
    let placed = place::place(&found, &mut scopes)?;
    let mut marks = found.marks;
    for definition in &placed {
        marks.entry(definition.span.first).or_default().opens = true;
        marks.entry(definition.span.last).or_default().closes = true;
    }
    let (leaves, units) = Leaves::of_marked(&tree, (text, &language.lexicon), &marks);
    let lines = Lines::of(source.written.as_bytes());
    let mut build = TreeIndex::new(text, lines, leaves, scopes);
    for definition in placed {
        let span = definition.span;
        build.define_units(
            definition.scope,
            &(span.start..span.end),
            units.between(span.first, span.last),
            definition.what,
        )?;
    }
    Ok(build.finish())
}

#[cfg(test)]
mod tests;
