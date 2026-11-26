//! What a tree-sitter resolver builds an index with: the file's tokens
//! and lines, its scope tree, which charges everything an index holds to
//! the file's room, and the definitions whose hashes are made at the end,
//! all in one pass over the tokens (`Leaves::hash_each`).
//!
//! A resolver walks its grammar's tree and adds what it finds here; the
//! bounds of size and hashing are this builder's, whatever the grammar.

use std::ops::Range;

use super::units::Leaves;
use crate::code::index::{
    Definition, FileIndex, IndexFault, Lines, ModuleFile, ScopeId, ScopeTree, Span, SpanHash, What,
};

/// The hash a definition holds until the end of the walk.
const UNHASHED: SpanHash = SpanHash::from_bytes([0; 32]);

pub(super) struct TreeIndex<'t> {
    text: &'t str,
    leaves: Leaves,
    lines: Lines,
    pub(super) scopes: ScopeTree,
    index: FileIndex,
    /// The units of each definition, in the order they were added.
    ranges: Vec<Range<usize>>,
}

impl<'t> TreeIndex<'t> {
    /// The builder of `text`'s index, whose lines are `lines` and whose
    /// tree's units are `leaves`, over `scopes`, the scopes entered so far.
    pub(super) fn new(text: &'t str, lines: Lines, leaves: Leaves, scopes: ScopeTree) -> Self {
        Self {
            text,
            leaves,
            lines,
            scopes,
            index: FileIndex::default(),
            ranges: Vec::new(),
        }
    }

    /// The span of bytes `range`.
    pub(super) fn span(&self, range: &Range<usize>) -> Span {
        self.lines.span(range.start, range.end)
    }

    /// Define `scope` over bytes `range` as `what` says, its hash over
    /// the tokens within them.
    pub(super) fn define(
        &mut self,
        scope: ScopeId,
        range: Range<usize>,
        what: What,
    ) -> Result<(), IndexFault> {
        let units = self.leaves.within(&range);
        self.define_units(scope, &range, units, what)
    }

    /// Define `scope` over bytes `bytes` as `what` says, its hash over
    /// `units`, the units of a marked tree its span node holds.
    pub(super) fn define_units(
        &mut self,
        scope: ScopeId,
        bytes: &Range<usize>,
        units: Range<usize>,
        what: What,
    ) -> Result<(), IndexFault> {
        let definition = Definition {
            scope,
            kind: what.kind,
            span: self.span(bytes),
            hash: UNHASHED,
            role: what.role,
            visibility: what.visibility,
        };
        self.scopes.admit_definition(&definition)?;
        self.index.definitions.push(definition);
        self.ranges.push(units);
        Ok(())
    }

    pub(super) fn add_module_file(&mut self, module: ModuleFile) -> Result<(), IndexFault> {
        self.scopes.admit_module_file(&module)?;
        self.index.rust.module_files.push(module);
        Ok(())
    }

    /// The tokens within bytes `bytes` as their hash holds them, a space
    /// between each two: no comment, and no token of no meaning.
    pub(super) fn spelled(&self, bytes: &Range<usize>) -> String {
        self.leaves.spelled(self.text, bytes)
    }

    /// The definitions added so far.
    pub(super) fn definitions(&self) -> &[Definition] {
        &self.index.definitions
    }

    /// The index, each definition with its hash.
    pub(super) fn finish(self) -> FileIndex {
        let Self {
            text,
            leaves,
            scopes,
            mut index,
            ranges,
            ..
        } = self;
        let hashes = leaves.hash_each(text, &ranges);
        for (definition, hash) in index.definitions.iter_mut().zip(hashes) {
            definition.hash = hash;
        }
        index.scopes = scopes.into_scopes();
        index
    }
}
