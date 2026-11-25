//! What a definition is, beside where: its kind, whether it is test code,
//! and its visibility. A resolver states each from the file's bytes; the
//! second step adds what the tree says of a whole file (`roles.rs`). The
//! kinds and visibilities are every language's: a query file names them
//! by [`DefinitionKind::name`] and [`Visibility::name`], and a language
//! uses those its source can state.

/// The kind of a definition. A kind is a different thing in the source,
/// never a guess: a `static` is no constant, a `union` no struct, a
/// `pub use` leaf a re-export of something defined elsewhere, a Python or
/// TypeScript `class` no struct, and a TypeScript `interface` no trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DefinitionKind {
    /// A `mod`, inline or declared; a TypeScript `namespace` or `module`.
    Module,
    Struct,
    Union,
    Enum,
    Trait,
    /// A trait impl, `Type::<Trait>`.
    Impl,
    /// A `fn` outside every `impl` and trait: in a module, in a body, in
    /// an `extern` block; a function a language declares, or a function
    /// expression bound to a name.
    Function,
    /// A `fn` of an `impl` or a trait, with a receiver or without; a
    /// function a class, an interface or an object literal holds, its
    /// accessors and constructor included.
    Method,
    /// A named field of a struct, a union or a struct variant; a
    /// property a class, an interface or a type literal declares.
    Field,
    /// A variant of a Rust enum, a member of a TypeScript one.
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
    /// A class: Python's `class`.
    Class,
    /// A name a module or a class binds by assignment, not a function's
    /// local: Python's `x = 1`, `x: int`, TypeScript's `const x = 1`.
    Variable,
    /// A TypeScript `interface`.
    Interface,
    /// A test a call defines (`it("opens")` under `describe("Store")`),
    /// named by the strings it is called with.
    Test,
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
            | Self::Heading
            | Self::Class
            | Self::Variable
            | Self::Interface
            | Self::Test => false,
        }
    }

    /// The kind's name in a query file.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Struct => "struct",
            Self::Union => "union",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Function => "function",
            Self::Method => "method",
            Self::Field => "field",
            Self::Variant => "variant",
            Self::Constant => "constant",
            Self::Static => "static",
            Self::TypeAlias => "type_alias",
            Self::AssociatedType => "associated_type",
            Self::Macro => "macro",
            Self::Reexport => "reexport",
            Self::Key => "key",
            Self::Target => "target",
            Self::Heading => "heading",
            Self::Class => "class",
            Self::Variable => "variable",
            Self::Interface => "interface",
            Self::Test => "test",
        }
    }

    /// The kind a query file names `name`.
    pub(super) fn of_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Every kind, in the order of their codes in a cache entry.
    pub(super) const ALL: [Self; 23] = [
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
        Self::Class,
        Self::Variable,
        Self::Interface,
        Self::Test,
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
            b"const" => Self::Constant,
            // A type's members are its functions, types and constants alone.
            _ if member => return None,
            b"struct" => Self::Struct,
            b"union" => Self::Union,
            b"enum" => Self::Enum,
            b"trait" => Self::Trait,
            b"static" => Self::Static,
            b"mod" => Self::Module,
            _ => return None,
        })
    }
}

/// What a definition is, beside where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct What {
    pub(super) kind: DefinitionKind,
    pub(super) role: Role,
    pub(super) visibility: Option<Visibility>,
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
    pub(crate) const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Code, Self::Code) => Self::Code,
            (Self::Test, _) | (_, Self::Test) => Self::Test,
        }
    }
}

/// Who may name a definition, as its source states it: Rust's modifiers,
/// a language's rule (a Python name with a leading underscore is
/// private), or TypeScript's `export` and member modifiers. A
/// `pub(in path)` keeps no path: no reader needs which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Visibility {
    /// No modifier, or `pub(self)`; a TypeScript declaration its module
    /// does not export, a `private` or `#` member.
    Private,
    /// `pub(crate)`, or the old `crate`.
    Crate,
    /// `pub(super)`.
    Super,
    /// `pub(in path)`.
    InPath,
    Public,
    /// A TypeScript `protected` member: its class and those that extend
    /// it.
    Protected,
}

impl Visibility {
    /// Every visibility, in the order of their codes in a cache entry.
    pub(super) const ALL: [Self; 6] = [
        Self::Private,
        Self::Crate,
        Self::Super,
        Self::InPath,
        Self::Public,
        Self::Protected,
    ];

    /// The visibility's name in a query file.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Crate => "crate",
            Self::Super => "super",
            Self::InPath => "in_path",
            Self::Public => "public",
            Self::Protected => "protected",
        }
    }

    /// The visibility a query file names `name`.
    pub(super) fn of_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|visibility| visibility.name() == name)
    }

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
