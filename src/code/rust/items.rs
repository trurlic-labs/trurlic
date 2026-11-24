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

/// A node that defines its `name`: an item, a variant, a named field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Item {
    Function,
    Struct,
    Union,
    Enum,
    Trait,
    Module,
    Constant,
    Static,
    Type,
    AssociatedType,
    Variant,
    Field,
}

impl Item {
    /// The item a node of `kind` is; `None` for a node that defines no
    /// name of its own.
    pub(super) fn of_node(kind: &str) -> Option<Self> {
        Some(match kind {
            "function_item" | "function_signature_item" => Self::Function,
            "struct_item" => Self::Struct,
            "union_item" => Self::Union,
            "enum_item" => Self::Enum,
            "trait_item" => Self::Trait,
            "mod_item" => Self::Module,
            "const_item" => Self::Constant,
            "static_item" => Self::Static,
            "type_item" => Self::Type,
            "associated_type" => Self::AssociatedType,
            "enum_variant" => Self::Variant,
            "field_declaration" => Self::Field,
            _ => return None,
        })
    }

    /// Whether what it holds may define more: a function's body, a type's
    /// fields, a `const`'s initializer; a field's type holds nothing.
    pub(super) const fn holds_items(self) -> bool {
        !matches!(self, Self::Field)
    }

    /// Its kind, standing in `within` with the modifier `own`, and the
    /// container its children stand in.
    pub(super) const fn kind(self, within: Within, own: Visibility) -> (DefinitionKind, Within) {
        let member = matches!(within.container(), Container::Type);
        match self {
            Self::Function if member => (DefinitionKind::Method, Within::Items),
            Self::Function => (DefinitionKind::Function, Within::Items),
            Self::Struct => (DefinitionKind::Struct, Within::Items),
            Self::Union => (DefinitionKind::Union, Within::Items),
            Self::Enum => (DefinitionKind::Enum, Within::Enum(own)),
            Self::Trait => (DefinitionKind::Trait, Within::Trait(own)),
            Self::Module => (DefinitionKind::Module, Within::Items),
            Self::Constant => (DefinitionKind::Constant, Within::Items),
            Self::Static => (DefinitionKind::Static, Within::Items),
            Self::Type if member => (DefinitionKind::AssociatedType, Within::Items),
            Self::Type => (DefinitionKind::TypeAlias, Within::Items),
            Self::AssociatedType => (DefinitionKind::AssociatedType, Within::Items),
            // A variant's fields have the enum's visibility, as it has; its
            // value is walked as items are (`push_children`).
            Self::Variant => (DefinitionKind::Variant, within),
            Self::Field => (DefinitionKind::Field, Within::Items),
        }
    }
}
