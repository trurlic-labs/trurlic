//! What a Rust item is, from its node and the container it stands in:
//! its kind, and whose visibility it and its members have.

use crate::code::kinds::{Container, DefinitionKind, Role, Visibility};

/// The container a node stands in, as far as what it defines is
/// concerned: whether a `fn` is a method, and whose visibility a member
/// has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Within {
    /// A file, a module, a body, a struct's or union's fields, an
    /// `extern` block: each item states its own visibility.
    Items,
    /// An inherent `impl`: its members state their own.
    Impl,
    /// A trait impl: its members have the trait's, which it does not
    /// state.
    TraitImpl,
    /// A trait: its members have its visibility.
    Trait(Visibility),
    /// An enum: its variants and their fields have its visibility.
    Enum(Visibility),
}

impl Within {
    pub(super) const fn container(self) -> Container {
        match self {
            Self::Impl | Self::TraitImpl | Self::Trait(_) => Container::Type,
            Self::Items | Self::Enum(_) => Container::Items,
        }
    }

    /// The visibility of a member whose own modifier states `own`.
    pub(super) const fn visibility(self, own: Visibility) -> Option<Visibility> {
        match self {
            Self::Items | Self::Impl => Some(own),
            Self::TraitImpl => None,
            Self::Trait(inherited) | Self::Enum(inherited) => Some(inherited),
        }
    }
}

/// What a definition is, beside where.
#[derive(Clone, Copy)]
pub(super) struct What {
    pub(super) kind: DefinitionKind,
    pub(super) role: Role,
    pub(super) visibility: Option<Visibility>,
}

/// The kind of the item a node of `node_kind` is, standing in `within`
/// with the modifier `own`, and the container its children stand in.
pub(super) fn item_kind(
    node_kind: &str,
    within: Within,
    own: Visibility,
) -> (DefinitionKind, Within) {
    let keyword = match node_kind {
        "function_item" | "function_signature_item" => "fn",
        "struct_item" => "struct",
        "union_item" => "union",
        "trait_item" => return (DefinitionKind::Trait, Within::Trait(own)),
        "enum_item" => return (DefinitionKind::Enum, Within::Enum(own)),
        "mod_item" => "mod",
        "const_item" => "const",
        "static_item" => "static",
        "type_item" => "type",
        "associated_type" => return (DefinitionKind::AssociatedType, Within::Items),
        // A variant's fields have the enum's visibility, as it has.
        "enum_variant" => return (DefinitionKind::Variant, within),
        // A `field_declaration`, the one other node an item is defined of.
        _ => return (DefinitionKind::Field, Within::Items),
    };
    let kind = DefinitionKind::of_keyword(keyword, within.container());
    (kind.unwrap_or(DefinitionKind::Function), Within::Items)
}
