//! Where a path's first name may be bound anew ([`Rebinding`]). The index
//! reads a path led by a child module's name against it, and states the
//! path unresolvable rather than read the child module where the name may
//! mean something else.
//!
//! A name may be bound anew at a byte in a macro's token tree, which the
//! index does not expand. It may be in a block that holds a `use`, an
//! `extern crate` or a macro invocation as an item or a statement,
//! whatever name they bind. It may be where an item that stands in a
//! block, or a generic parameter of an item, binds that name. The check is
//! conservative: it reads no namespace, so a value of the name in such a
//! block makes the path unresolvable too.

use std::collections::BTreeMap;
use std::ops::Range;

use super::reading::{Reading, Spans};

/// Each name an item in a block or a generic parameter binds in a file:
/// the bytes it binds it over.
pub(super) struct Rebinding<'r>(BTreeMap<&'r str, Spans>);

impl<'r> Rebinding<'r> {
    /// The names `names` binds, each with the bytes of an item in a block
    /// or of an item with a generic parameter.
    pub(super) fn of(names: Vec<(&'r str, Range<usize>)>) -> Self {
        let mut named: BTreeMap<&str, Vec<Range<usize>>> = BTreeMap::new();
        for (name, bytes) in names {
            named.entry(name).or_default().push(bytes);
        }
        let named = named.into_iter();
        Self(
            named
                .map(|(name, bytes)| (name, Spans::of(bytes)))
                .collect(),
        )
    }

    /// Whether something may bind `name` anew at the byte `at` of the file
    /// `reading` reads.
    pub(super) fn may_rebind(&self, name: &str, at: usize, reading: &Reading<'_>) -> bool {
        let named = self.0.get(name).is_some_and(|spans| spans.hold(at));
        named || reading.rebinding.hold(at) || reading.macro_trees.hold(at)
    }
}
