//! What a tree-sitter resolver builds an index with: the file's tokens
//! and lines, its scope tree, which charges everything an index holds to
//! the file's room, the budget of the resolver's lexical scans, and the
//! definitions and invocations whose hashes are made at the end, all in
//! one pass over the tokens (`Leaves::hash_each`).
//!
//! A resolver walks its grammar's tree and adds what it finds here; the
//! bounds of size, work and hashing are this builder's, whatever the
//! grammar.

use std::ops::Range;

use super::index::{Definition, FileIndex, IndexFault, Invocation, ModuleFile, TypeMacro};
use super::kinds::What;
use super::scope::{ScopeId, ScopeTree};
use super::span::{Lines, Span, SpanHash};
use super::syntax::Leaves;
use super::work::{SCAN_WORK_PER_BYTE, Work};

/// The hash a definition or invocation holds until the end of the walk.
pub(super) const UNHASHED: SpanHash = SpanHash::from_bytes([0; 32]);

pub(super) struct TreeIndex<'t> {
    text: &'t str,
    leaves: Leaves,
    lines: Lines,
    pub(super) scopes: ScopeTree,
    /// The budget of the resolver's scans of token trees and texts.
    pub(super) work: Work,
    index: FileIndex,
    /// The units of each definition, then of each invocation, in the
    /// order they were added.
    ranges: Vec<Range<usize>>,
    invocation_ranges: Vec<Range<usize>>,
}

impl<'t> TreeIndex<'t> {
    /// The builder of `text`'s index, whose tree's units are `leaves`,
    /// over `scopes`, the scopes entered so far.
    pub(super) fn new(text: &'t str, leaves: Leaves, scopes: ScopeTree) -> Self {
        Self {
            text,
            leaves,
            lines: Lines::of(text.as_bytes()),
            scopes,
            work: Work::of(text.len(), SCAN_WORK_PER_BYTE),
            index: FileIndex::default(),
            ranges: Vec::new(),
            invocation_ranges: Vec::new(),
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

    /// Keep `invocation`, over bytes `range`, for the expansion.
    pub(super) fn invoke(
        &mut self,
        invocation: Invocation,
        range: Range<usize>,
    ) -> Result<(), IndexFault> {
        self.scopes.admit_invocation(&invocation)?;
        self.index.rust.invocations.push(invocation);
        self.invocation_ranges.push(self.leaves.within(&range));
        Ok(())
    }

    pub(super) fn add_macro(&mut self, found: TypeMacro) -> Result<(), IndexFault> {
        self.scopes.admit_macro(&found)?;
        self.index.rust.macros.push(found);
        Ok(())
    }

    pub(super) fn add_module_file(&mut self, module: ModuleFile) -> Result<(), IndexFault> {
        self.scopes.admit_module_file(&module)?;
        self.index.rust.module_files.push(module);
        Ok(())
    }

    /// The definitions added so far.
    pub(super) fn definitions(&self) -> &[Definition] {
        &self.index.definitions
    }

    /// The index, each definition and invocation with its hash.
    pub(super) fn finish(self) -> FileIndex {
        let Self {
            text,
            leaves,
            scopes,
            mut index,
            mut ranges,
            invocation_ranges,
            ..
        } = self;
        let definitions = ranges.len();
        ranges.extend(invocation_ranges);
        let hashes = leaves.hash_each(text, &ranges);
        let (own, invoked) = hashes.split_at(definitions.min(hashes.len()));
        for (definition, hash) in index.definitions.iter_mut().zip(own) {
            definition.hash = *hash;
        }
        for (invocation, hash) in index.rust.invocations.iter_mut().zip(invoked) {
            invocation.hash = *hash;
        }
        index.scopes = scopes.into_scopes();
        index
    }
}
