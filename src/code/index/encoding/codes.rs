//! The one-byte codes of an entry's closed sets: a definition's kind, its
//! role and its visibility, each read back by its place in its set.

use crate::code::index::kinds::{DefinitionKind, Role, Visibility};

/// A kind's code: its place in [`DefinitionKind::ALL`], by which a read
/// takes it back.
pub(super) fn kind_code(kind: DefinitionKind) -> u8 {
    place_in(&DefinitionKind::ALL, &kind)
}

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
