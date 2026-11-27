//! The one-byte codes of an entry's closed sets: a definition's kind, its
//! role and its visibility, and a dependency's kind, each read back by its
//! place in its set, and whether a doc is there and whole.

use crate::code::index::file::Doc;
use crate::code::index::kinds::{DefinitionKind, Role, Visibility};
use crate::code::index::manifest::DependencyKind;

/// A kind's code: its place in [`DefinitionKind::ALL`], by which a read
/// takes it back.
pub(super) fn kind_code(kind: DefinitionKind) -> u8 {
    place_in(&DefinitionKind::ALL, &kind)
}

/// A dependency kind's code: its place in [`DependencyKind::ALL`].
pub(super) fn dependency_kind_code(kind: DependencyKind) -> u8 {
    place_in(&DependencyKind::ALL, &kind)
}
/// A definition's marks hold a visibility's code in three bits: every
/// code [`visibility_code`] gives, the one past the set's end for a value
/// it misses among them, fits in them.
const _: () = assert!(Visibility::ALL.len() + 1 < 1 << 3);

/// A visibility's code: 0 for none, else one past its place in
/// [`Visibility::ALL`].
pub(super) fn visibility_code(visibility: Option<Visibility>) -> u8 {
    visibility.map_or(0, |visibility| {
        place_in(&Visibility::ALL, &visibility).saturating_add(1)
    })
}

/// The place of `value` in `all`; past its end, a code no read takes back,
/// when `all` misses it.
fn place_in<T: PartialEq>(all: &[T], value: &T) -> u8 {
    let place = all.iter().position(|each| each == value);
    u8::try_from(place.unwrap_or(all.len())).unwrap_or(u8::MAX)
}

pub(super) const fn role_code(role: Role) -> u8 {
    match role {
        Role::Code => 0,
        Role::Test => 1,
    }
}

/// A doc's code: 0 for none, 1 for a whole one, 2 for one cut short; a
/// doc's text follows a code of 1 or 2.
pub(super) const fn doc_code(doc: Option<&Doc>) -> u8 {
    match doc {
        None => 0,
        Some(doc) if doc.is_cut() => 2,
        Some(_) => 1,
    }
}
