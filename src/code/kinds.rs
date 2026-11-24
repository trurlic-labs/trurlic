//! What a definition is, beside where: its kind, whether it is test code,
//! and its visibility. A resolver states each from the file's bytes; the
//! second step adds what the tree says of a whole file (`roles.rs`).

/// The kind of a definition. A kind is a different thing in the source,
/// never a guess: a `static` is no constant, a `union` no struct, and a
/// `pub use` leaf a re-export of something defined elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DefinitionKind {
    /// A `mod`, inline or declared.
    Module,
    Struct,
    Union,
    Enum,
    Trait,
    /// A trait impl, `Type::<Trait>`.
    Impl,
    /// A `fn` outside every `impl` and trait: in a module, in a body, in
    /// an `extern` block.
    Function,
    /// A `fn` of an `impl` or a trait, with a receiver or without.
    Method,
    /// A named field of a struct, a union or a struct variant.
    Field,
    Variant,
    /// A `const`, at any level.
    Constant,
    Static,
    /// A `type` outside every `impl` and trait.
    TypeAlias,
    /// A trait's `type`, or the one an `impl` gives it.
    AssociatedType,
    /// A `macro_rules!` macro.
    Macro,
    /// A `pub use` leaf: the name a module gives what another defines.
    Reexport,
    /// A TOML or JSON key.
    Key,
    /// A Makefile target.
    Target,
    /// A Markdown heading.
    Heading,
}

impl DefinitionKind {
    /// Whether it is a function or a method, what a `()` suffix names.
    pub(crate) const fn is_function(self) -> bool {
        match self {
            Self::Function | Self::Method => true,
            Self::Module
            | Self::Struct
            | Self::Union
            | Self::Enum
            | Self::Trait
            | Self::Impl
            | Self::Field
            | Self::Variant
            | Self::Constant
            | Self::Static
            | Self::TypeAlias
            | Self::AssociatedType
            | Self::Macro
            | Self::Reexport
            | Self::Key
            | Self::Target
            | Self::Heading => false,
        }
    }

    /// Every kind, in the order of their codes in a cache entry.
    pub(super) const ALL: [Self; 19] = [
        Self::Module,
        Self::Struct,
        Self::Union,
        Self::Enum,
        Self::Trait,
        Self::Impl,
        Self::Function,
        Self::Method,
        Self::Field,
        Self::Variant,
        Self::Constant,
        Self::Static,
        Self::TypeAlias,
        Self::AssociatedType,
        Self::Macro,
        Self::Reexport,
        Self::Key,
        Self::Target,
        Self::Heading,
    ];

    /// The kind of the item a Rust keyword opens (`fn`, `struct`, ...),
    /// in a container whose `fn`s are methods or not; `None` for a word
    /// that opens no item.
    pub(super) const fn of_keyword(keyword: &str, container: Container) -> Option<Self> {
        let member = matches!(container, Container::Type);
        Some(match keyword.as_bytes() {
            b"fn" if member => Self::Method,
            b"fn" => Self::Function,
            b"type" if member => Self::AssociatedType,
            b"type" => Self::TypeAlias,
            b"struct" => Self::Struct,
            b"union" => Self::Union,
            b"enum" => Self::Enum,
            b"trait" => Self::Trait,
            b"const" => Self::Constant,
            b"static" => Self::Static,
            b"mod" => Self::Module,
            _ => return None,
        })
    }
}

/// Whether an item's keyword opens a member of a type (an `impl`, a
/// trait) or an item of a module or a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    Type,
    Items,
}

/// Whether a definition is test code or the code a project ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Code,
    Test,
}

impl Role {
    /// Test code when either is.
    pub(super) const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Code, Self::Code) => Self::Code,
            (Self::Test, _) | (_, Self::Test) => Self::Test,
        }
    }
}

/// Who may name a Rust definition, as its source states it. A
/// `pub(in path)` keeps no path: no reader asks which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Visibility {
    /// No modifier, or `pub(self)`.
    Private,
    /// `pub(crate)`, or the old `crate`.
    Crate,
    /// `pub(super)`.
    Super,
    /// `pub(in path)`.
    InPath,
    Public,
}

impl Visibility {
    /// Every visibility, in the order of their codes in a cache entry.
    pub(super) const ALL: [Self; 5] = [
        Self::Private,
        Self::Crate,
        Self::Super,
        Self::InPath,
        Self::Public,
    ];

    /// The visibility a modifier's text states (`pub`, `pub ( crate )`);
    /// `None` for a text that is no modifier.
    pub(super) fn of_modifier(modifier: &str) -> Option<Self> {
        let modifier = modifier.trim();
        if modifier == "crate" {
            return Some(Self::Crate);
        }
        let restricted = modifier.strip_prefix("pub")?.trim();
        if restricted.is_empty() {
            return Some(Self::Public);
        }
        let scope = restricted.strip_prefix('(')?.strip_suffix(')')?.trim();
        match scope {
            "crate" => Some(Self::Crate),
            "super" => Some(Self::Super),
            "self" => Some(Self::Private),
            _ => scope
                .strip_prefix("in")
                .filter(|path| path.starts_with(char::is_whitespace))
                .map(|_| Self::InPath),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modifier_states_its_visibility_whatever_its_spaces() {
        for (modifier, visibility) in [
            ("pub", Some(Visibility::Public)),
            ("pub ( crate )", Some(Visibility::Crate)),
            ("crate", Some(Visibility::Crate)),
            ("pub(super)", Some(Visibility::Super)),
            ("pub(self)", Some(Visibility::Private)),
            ("pub(in crate::a)", Some(Visibility::InPath)),
            ("pub(inner)", None),
            ("public", None),
        ] {
            assert_eq!(Visibility::of_modifier(modifier), visibility, "{modifier}");
        }
    }
}
