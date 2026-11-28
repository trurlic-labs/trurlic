//! What a Rust file's index holds of the paths it names into its crate:
//! each path as its segments are spelled, and what kind of path it is.
//! The Rust resolver reads them (`parse/rust/imports.rs`); the facts over
//! a tree resolve them to files (`facts/imports.rs`). What a path resolves
//! to depends on the other files of the tree, so no index holds it, and an
//! index stays a function of its file's kind and bytes.
//!
//! A path is a chain of steps, one a segment, each after the one before
//! it. The entries of a `use` list share the steps of its prefix, so what
//! a file's imports cost is linear in the file however deep its lists
//! nest.

use super::kinds::Role;
use super::scope::ScopeId;

/// One segment of a path, as spelled, a raw identifier as its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathStep {
    pub(in crate::code) before: Before,
    pub(in crate::code) segment: String,
}

/// What a step comes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Before {
    /// Nothing: the step is its path's first segment, standing in the
    /// inline module of this scope, `None` for the file's own module.
    Start(Option<ScopeId>),
    /// The step at this place of the index's steps, which is below its
    /// own, so a path's chain of steps ends.
    Step(usize),
}

/// A path a Rust file names into its crate, ending at the step at `last`
/// of its index's steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Import {
    pub(in crate::code) last: usize,
    /// Test code by what its file says of the bytes it stands in; the
    /// tree may make a whole file test code besides.
    pub(in crate::code) role: Role,
    pub(in crate::code) kind: ImportKind,
}

/// Where an import stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportKind {
    /// In code, a macro's tokens or an attribute's string.
    Code,
    /// A leaf of a module's `use` with no visibility, of a `use` in a
    /// block whatever its visibility, or of one among a macro's tokens
    /// with none: the last two bind nothing in the module.
    Use(Binds),
    /// A leaf of a `use` a visibility opens, a module's or one among a
    /// macro's tokens (which binds nothing): a re-export, no dependency
    /// of its file.
    Reexport(Binds),
    /// A path led by a child module's name where that name may be bound
    /// anew, by the rule of `parse/rust/rebinding.rs`: a dependency of its
    /// file that resolves to no file.
    Unresolvable,
}

/// The name a `use` leaf binds in the module it stands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binds {
    /// Its path's last segment: `b` of `use a::b` and of `use a::b::{self}`.
    Last,
    /// The name after `as`.
    Alias(String),
    /// No name it can tell: an alias `_`, or a `use` in a block or a
    /// macro's tokens.
    Nothing,
    /// Whatever names the module it globs holds: no one name.
    Glob,
}
