//! The paths of a file's definitions, held as a tree: each scope stores
//! its own name segments and its parent, so a path is walked, never
//! copied, and an index grows with its file, not with its file times its
//! nesting.
//!
//! A scope is a definition's place or a container that is no definition
//! (an inherent `impl`'s type). A parent is always created before its
//! children, so a parent's id is below its child's and a walk up ends.
//! Every segment counts toward [`MAX_NESTING`], those of a key or heading
//! split at `::` included.

use super::symbol::Segment;

/// The segments a path may hold. A file that nests one deeper has no
/// index.
pub(crate) const MAX_NESTING: usize = 64;

/// A scope of one file, by its place in the file's scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopeId(u32);

impl ScopeId {
    pub(crate) const fn at(self) -> usize {
        self.0 as usize
    }

    /// The id of the scope at `at`; `None` past `u32`, which no file
    /// within the size a source may have reaches.
    pub(crate) fn of(at: usize) -> Option<Self> {
        u32::try_from(at).ok().map(Self)
    }
}

/// A scope: its own name segments, under its parent, `None` at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) parent: Option<ScopeId>,
    pub(crate) name: Vec<Segment>,
}

/// The scopes of a file, which a path is walked through.
#[derive(Clone, Copy)]
pub(crate) struct Scopes<'a>(pub(crate) &'a [Scope]);

impl<'a> Scopes<'a> {
    /// The segments of the path of `id`, from its last: each scope's
    /// own, then its parent's. A parent's id is below its child's, so the
    /// walk ends.
    pub(crate) fn path_from_end(self, id: ScopeId) -> impl Iterator<Item = &'a Segment> + Clone {
        let mut next = Some(id);
        std::iter::from_fn(move || {
            let current = next?;
            let scope = self.0.get(current.at())?;
            next = scope.parent.filter(|parent| *parent < current);
            Some(scope.name.iter().rev())
        })
        .flatten()
    }
}
