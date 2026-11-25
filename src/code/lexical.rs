//! What a lexical resolver (TOML, JSON, Makefiles, Markdown) builds an
//! index with: the scopes of its names, each split at `::` as a symbol
//! is, and its definitions, each hashed from its whitespace-normalized
//! text, every byte hashed spent from the file's budget of work, each
//! scope and definition charged to the file's room by the scope tree.

use std::ops::Range;

use super::index::{Definition, FileIndex, IndexFault};
use super::kinds::{DefinitionKind, Role};
use super::scope::{ScopeId, ScopeTree};
use super::span::{Lines, SpanHash};
use super::symbol::name_segments;
use super::work::{LEXICAL_WORK_PER_BYTE, Work};

pub(super) struct Lexical<'t> {
    text: &'t str,
    lines: Lines,
    scopes: ScopeTree,
    definitions: Vec<Definition>,
    work: Work,
}

impl<'t> Lexical<'t> {
    pub(super) fn of(text: &'t str) -> Self {
        Self {
            text,
            lines: Lines::of(text.as_bytes()),
            scopes: ScopeTree::for_file(text.len()),
            definitions: Vec::new(),
            work: Work::of(text.len(), LEXICAL_WORK_PER_BYTE),
        }
    }

    /// The scope of `name`, a key, a target or a heading as the file
    /// holds it, under `parent`.
    pub(super) fn scope(
        &mut self,
        parent: Option<ScopeId>,
        name: &str,
    ) -> Result<ScopeId, IndexFault> {
        self.scopes.enter(parent, name_segments(name).collect())
    }

    /// The hash of bytes `range` of the text.
    pub(super) fn hash(&mut self, range: Range<usize>) -> Result<SpanHash, IndexFault> {
        self.work.spend(range.len())?;
        Ok(SpanHash::of_text(self.text.get(range).unwrap_or_default()))
    }

    /// Define `scope` as a `kind` over bytes `range` whose hash is `hash`.
    pub(super) fn define(
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

    pub(super) fn finish(self) -> FileIndex {
        FileIndex {
            scopes: self.scopes.into_scopes(),
            definitions: self.definitions,
            ..FileIndex::default()
        }
    }
}
