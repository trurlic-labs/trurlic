//! What a lexical resolver (TOML, JSON, Makefiles, Markdown) builds an
//! index with: the scopes of its names, each split at `::` as a symbol
//! is, and its definitions, each hashed from its whitespace-normalized
//! text, every byte hashed spent from the file's budget of work.

use std::ops::Range;

use super::index::{Definition, FileIndex, IndexFault};
use super::kinds::{DefinitionKind, Role};
use super::scope::{ScopeId, ScopeTree};
use super::span::{Lines, SpanHash};
use super::symbol::name_segments;
use super::work::Work;

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
            scopes: ScopeTree::default(),
            definitions: Vec::new(),
            work: Work::of(text.len()),
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
    ) {
        self.definitions.push(Definition {
            scope,
            kind,
            span: self.lines.span(range.start, range.end),
            hash,
            role: Role::Code,
            visibility: None,
        });
    }

    pub(super) fn finish(self) -> FileIndex {
        FileIndex {
            scopes: self.scopes.into_scopes(),
            definitions: self.definitions,
            ..FileIndex::default()
        }
    }
}
