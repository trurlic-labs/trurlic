//! The lexical resolvers, each reading its kind without a grammar: TOML
//! (`toml.rs`), JSON (`json.rs`), Makefiles (`make.rs`) and Markdown
//! (`markdown.rs`). What each builds an index with ([`Lexical`]): the
//! scopes of its names, each split at `::` as a symbol is, and its
//! definitions, each hashed from its whitespace-normalized text, every
//! byte hashed spent from the file's budget of work, each scope and
//! definition charged to the file's room by the scope tree.

pub(in crate::code) mod json;
pub(in crate::code) mod make;
pub(in crate::code) mod markdown;
pub(in crate::code) mod toml;

use std::ops::Range;

use crate::code::index::{
    Definition, DefinitionKind, FileIndex, IndexFault, LEXICAL_WORK_PER_BYTE, Lines, MIN_WORK,
    Role, ScopeId, ScopeTree, SpanHash, Work, name_segments,
};

struct Lexical<'t> {
    text: &'t str,
    lines: Lines,
    scopes: ScopeTree,
    definitions: Vec<Definition>,
    work: Work,
}

impl<'t> Lexical<'t> {
    /// The builder of `text`'s index, into `scopes`.
    fn of(text: &'t str, scopes: ScopeTree) -> Self {
        Self {
            text,
            lines: Lines::of(text.as_bytes()),
            scopes,
            definitions: Vec::new(),
            work: Work::of(text.len(), LEXICAL_WORK_PER_BYTE, MIN_WORK),
        }
    }

    /// The scope of `name`, a key, a target or a heading as the file
    /// holds it, under `parent`.
    fn scope(&mut self, parent: Option<ScopeId>, name: &str) -> Result<ScopeId, IndexFault> {
        self.scopes.enter(parent, name_segments(name).collect())
    }

    /// The hash of bytes `range` of the text.
    fn hash(&mut self, range: Range<usize>) -> Result<SpanHash, IndexFault> {
        self.work.spend(range.len())?;
        Ok(SpanHash::of_text(self.text.get(range).unwrap_or_default()))
    }

    /// Define `scope` as a `kind` over bytes `range` whose hash is `hash`.
    fn define(
        &mut self,
        scope: ScopeId,
        kind: DefinitionKind,
        range: Range<usize>,
        hash: SpanHash,
    ) -> Result<(), IndexFault> {
        let definition = Definition {
            scope,
            kind,
            span: self.lines.span(range.start, range.end),
            hash,
            role: Role::Code,
            visibility: None,
        };
        self.scopes.admit_definition(&definition)?;
        self.definitions.push(definition);
        Ok(())
    }

    fn finish(self) -> FileIndex {
        FileIndex {
            scopes: self.scopes.into_scopes(),
            definitions: self.definitions,
            ..FileIndex::default()
        }
    }
}
