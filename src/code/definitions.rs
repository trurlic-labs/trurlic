//! The definitions of the one path a symbol names: what an anchor that
//! resolves to them carries.

use std::fmt;

use super::index::Definition;
use super::span::{Span, SpanHash};

/// The definitions of one path, one at least. It holds their spans in
/// file order, a span two share listed once, how many they are, and
/// whether they are of more than one kind. Its one hash is over the
/// members' hashes in their own order ([`SpanHash::of_members`]), so twins
/// that swap places move nothing. The cfg twins of an item, a field and
/// its getter, a key an array's elements each hold: each is one thing an
/// anchor names, whose every span a change may touch. Kind plays no part
/// in what is one thing; a set of mixed kinds is what a report warns of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definitions {
    first: Span,
    /// The spans after the first, in file order.
    more: Vec<Span>,
    mixed_kinds: bool,
    hash: SpanHash,
}

impl Definitions {
    /// The set of `first` and `rest`, definitions of one path.
    pub(super) fn of(first: &Definition, rest: &[&Definition]) -> Self {
        let mut spans: Vec<Span> = std::iter::once(first.span)
            .chain(rest.iter().map(|d| d.span))
            .collect();
        spans.sort_unstable();
        spans.dedup();
        let mut hashes: Vec<(Span, SpanHash)> = std::iter::once(first)
            .chain(rest.iter().copied())
            .map(|d| (d.span, d.hash))
            .collect();
        hashes.sort_unstable_by_key(|(span, _)| *span);
        hashes.dedup_by_key(|(span, _)| *span);
        let mut hashes: Vec<SpanHash> = hashes.into_iter().map(|(_, hash)| hash).collect();
        hashes.sort_unstable();
        let mut spans = spans.into_iter();
        Self {
            first: spans.next().unwrap_or(first.span),
            more: spans.collect(),
            mixed_kinds: rest.iter().any(|d| d.kind != first.kind),
            hash: SpanHash::of_members(&hashes),
        }
    }

    /// Each span, in file order.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the anchor findings and the diff review are the first callers"
        )
    )]
    pub(crate) fn spans(&self) -> impl Iterator<Item = &Span> {
        std::iter::once(&self.first).chain(&self.more)
    }

    /// How many definitions, their shared spans counted once.
    pub(crate) fn count(&self) -> usize {
        self.more.len() + 1
    }

    /// Whether the definitions are of more than one kind: a field and its
    /// getter, `mod parse` and `fn parse`.
    pub(crate) const fn mixed_kinds(&self) -> bool {
        self.mixed_kinds
    }

    /// The hash of the set: one definition's own, or that of several.
    pub(crate) const fn hash(&self) -> SpanHash {
        self.hash
    }
}

/// One definition as its span and hash; several as their count, the
/// first span and the hash, so a set of thousands prints one line.
impl fmt::Display for Definitions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.count() {
            1 => write!(f, "{}, {}", self.first, self.hash),
            count => write!(f, "{count} definitions: {}, {}", self.first, self.hash),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::kinds::{DefinitionKind, Role};
    use crate::code::scope::ScopeId;

    fn definition(at: usize, kind: DefinitionKind, hash: &str) -> Definition {
        Definition {
            scope: ScopeId::of(0).unwrap(),
            kind,
            span: Span {
                start: at,
                end: at + 1,
                first_line: at + 1,
                last_line: at + 1,
            },
            hash: SpanHash::of_text(hash),
            role: Role::Code,
            visibility: None,
        }
    }

    #[test]
    fn a_set_prints_its_count_first_span_and_hash_however_many_it_holds() {
        let members: Vec<Definition> = (0..20_000)
            .map(|at| definition(at, DefinitionKind::Key, &at.to_string()))
            .collect();
        let rest: Vec<&Definition> = members.iter().skip(1).collect();
        let set = Definitions::of(&members[0], &rest);
        let printed = set.to_string();

        assert_eq!(set.count(), 20_000);
        assert!(
            printed.starts_with("20000 definitions: lines 1-1 (bytes 0..1), t1:"),
            "{printed}"
        );
        assert!(printed.len() < 200);
        assert!(!set.mixed_kinds());
    }

    #[test]
    fn a_sets_hash_is_of_its_members_whatever_their_order_and_kinds_are_told() {
        let field = definition(10, DefinitionKind::Field, "a");
        let getter = definition(20, DefinitionKind::Method, "b");
        let swapped_field = definition(20, DefinitionKind::Field, "a");
        let swapped_getter = definition(10, DefinitionKind::Method, "b");
        let set = Definitions::of(&field, &[&getter]);
        let swapped = Definitions::of(&swapped_getter, &[&swapped_field]);
        let shared_span = Definitions::of(&field, &[&field]);

        assert_eq!(
            set.hash(),
            swapped.hash(),
            "twins that swap places move nothing"
        );
        assert!(set.mixed_kinds());
        assert_eq!(shared_span.count(), 1);
        assert_eq!(shared_span.hash(), field.hash);
        assert_eq!(set.to_string().lines().count(), 1);
    }
}
