//! A file's index: the scopes of its paths, the definitions it holds,
//! each with its scope, what it is, its span and span hash, and for Rust
//! the module files a second step follows. An index is a function of the
//! file's kind and bytes alone, so a cache keyed by the two holds it
//! whatever path or tree the bytes are under.

use std::fmt;

use super::kinds::{DefinitionKind, Role, Visibility};
use super::scope::{MAX_NESTING, Scope, ScopeId};
use super::span::{Span, SpanHash};

/// The largest file a tree-sitter grammar indexes. A parse's budget of
/// work and allocation is per byte of its file (`work.rs`), so this bounds
/// the budget of one parse.
pub(in crate::code) const TREE_SITTER_MAX_BYTES: usize = 1024 * 1024;

/// The largest file a lexical resolver indexes.
pub(in crate::code) const LEXICAL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The largest file of any kind the index reads.
pub(crate) const MAX_SOURCE_BYTES: usize = if TREE_SITTER_MAX_BYTES > LEXICAL_MAX_BYTES {
    TREE_SITTER_MAX_BYTES
} else {
    LEXICAL_MAX_BYTES
};

/// A kind of file the index has a resolver for: Rust's own, a language
/// the general resolver reads by its row (`parse/language/`, which also
/// holds what a kind is read as), or a lexical kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FileKind {
    Rust,
    Python,
    TypeScript,
    /// A TypeScript declaration file, `.d.ts`, whose top is an ambient
    /// context.
    TypeScriptDeclarations,
    /// TSX, and JavaScript, which the TSX grammar reads.
    Tsx,
    C,
    /// C++, and every C or C++ header, which the C++ grammar reads.
    Cpp,
    Toml,
    Json,
    Make,
    Markdown,
}

impl FileKind {
    /// The kind of the file at `path`, by its name; `None` for a kind with
    /// no resolver.
    pub(crate) fn of(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let (stem, extension) = name
            .rsplit_once('.')
            .map_or((name, None), |(stem, extension)| (stem, Some(extension)));
        match (name, extension) {
            ("Makefile" | "makefile" | "GNUmakefile", _) | (_, Some("mk")) => Some(Self::Make),
            (_, Some("rs")) => Some(Self::Rust),
            (_, Some("py" | "pyi" | "pyw")) => Some(Self::Python),
            (_, Some("ts" | "mts" | "cts")) if stem.ends_with(".d") => {
                Some(Self::TypeScriptDeclarations)
            }
            (_, Some("ts" | "mts" | "cts")) => Some(Self::TypeScript),
            (_, Some("tsx" | "js" | "jsx" | "mjs" | "cjs")) => Some(Self::Tsx),
            (_, Some("c")) => Some(Self::C),
            (_, Some("h" | "cc" | "cpp" | "cxx" | "hpp" | "hh")) => Some(Self::Cpp),
            (_, Some("toml")) => Some(Self::Toml),
            (_, Some("json")) => Some(Self::Json),
            (_, Some("md")) => Some(Self::Markdown),
            _ => None,
        }
    }

    /// Whether a file of this kind is source code, which the tree's facts
    /// hold whoever anchors it, rather than data or prose, read when an
    /// anchor names it.
    pub(crate) const fn is_code(self) -> bool {
        match self {
            Self::Rust
            | Self::Python
            | Self::TypeScript
            | Self::TypeScriptDeclarations
            | Self::Tsx
            | Self::C
            | Self::Cpp => true,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => false,
        }
    }

    /// Whether its files blank the macros their tree defines as
    /// attributes, which a C or C++ file does.
    pub(crate) const fn blanks_macros(self) -> bool {
        match self {
            Self::C | Self::Cpp => true,
            Self::Rust
            | Self::Python
            | Self::TypeScript
            | Self::TypeScriptDeclarations
            | Self::Tsx
            | Self::Toml
            | Self::Json
            | Self::Make
            | Self::Markdown => false,
        }
    }

    /// The largest file of the kind the index reads.
    pub(crate) const fn max_bytes(self) -> usize {
        match self {
            Self::Rust
            | Self::Python
            | Self::TypeScript
            | Self::TypeScriptDeclarations
            | Self::Tsx
            | Self::C
            | Self::Cpp => TREE_SITTER_MAX_BYTES,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => LEXICAL_MAX_BYTES,
        }
    }
}

/// One definition: its scope, whose path names it, what it is, where it
/// is, and the hash of what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definition {
    pub(crate) scope: ScopeId,
    pub(crate) kind: DefinitionKind,
    pub(crate) span: Span,
    pub(crate) hash: SpanHash,
    /// Test code by what its file says of it; the tree may make a whole
    /// file test code besides (`CodeFacts::definitions`).
    pub(crate) role: Role,
    /// `None` where the source states none: a trait impl and its members,
    /// whose visibility is the trait's, and every lexical definition.
    pub(crate) visibility: Option<Visibility>,
}

/// A `mod name;` whose items are in a file of their own: the module's
/// scope, whether the declaration is test code, and the file a
/// `#[path = ".."]` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleFile {
    pub(crate) scope: ScopeId,
    pub(crate) role: Role,
    pub(crate) path: Option<String>,
}

/// What a file of a kind with a resolver defines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileIndex {
    pub(crate) scopes: Vec<Scope>,
    /// Sorted by span, then scope.
    pub(crate) definitions: Vec<Definition>,
    /// Empty for every kind but Rust.
    pub(crate) rust: RustFacts,
}

/// What a Rust file holds besides its definitions: the modules declared
/// to be in files of their own, which the second step over its crate
/// follows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustFacts {
    /// In the order of the file.
    pub(crate) module_files: Vec<ModuleFile>,
}

/// Why a file's bytes give no index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexFault {
    /// The bytes are not UTF-8 from byte `at` on.
    NotUtf8 { at: usize },
    /// A TOML or JSON file that does not parse, from about byte `at`.
    Syntax { at: usize },
    /// The parse, the work after it, or the entry it gives ran past its
    /// budget for the file's size.
    TooComplex,
    /// The file is `bytes` long, over the `max` its kind indexes.
    TooLarge { bytes: usize, max: usize },
    /// A path of more than [`MAX_NESTING`] segments.
    TooDeep,
    /// The grammar is of an ABI the parser does not read, or its query
    /// does not compile.
    Grammar,
    /// A Python file whose lines start at more than `max` widths, past
    /// which its grammar's scanner aborts the process.
    TooManyIndentations { max: usize },
}

impl fmt::Display for IndexFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 { at } => write!(f, "is not UTF-8 from byte {at}"),
            Self::Syntax { at } => write!(f, "does not parse, near byte {at}"),
            Self::TooComplex => f.write_str("takes the index past its budget of work or size"),
            Self::TooLarge { bytes, max } => {
                write!(
                    f,
                    "is {bytes} bytes, over the {max} its kind is indexed within"
                )
            }
            Self::TooDeep => write!(f, "nests a path deeper than {MAX_NESTING} segments"),
            Self::Grammar => f.write_str("has a grammar or query the parser does not load"),
            Self::TooManyIndentations { max } => {
                write!(f, "starts its lines at more than {max} indentation widths")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_kind_is_read_from_its_name() {
        for (path, kind) in [
            ("src/lib.rs", Some(FileKind::Rust)),
            ("tools/run.py", Some(FileKind::Python)),
            ("pkg/__init__.pyi", Some(FileKind::Python)),
            ("tools/gui.pyw", Some(FileKind::Python)),
            ("src/app.ts", Some(FileKind::TypeScript)),
            ("types/index.d.ts", Some(FileKind::TypeScriptDeclarations)),
            ("types/index.d.mts", Some(FileKind::TypeScriptDeclarations)),
            ("types/d.ts", Some(FileKind::TypeScript)),
            ("lib/a.mts", Some(FileKind::TypeScript)),
            ("lib/a.cts", Some(FileKind::TypeScript)),
            ("ui/App.tsx", Some(FileKind::Tsx)),
            ("lib/a.js", Some(FileKind::Tsx)),
            ("ui/App.jsx", Some(FileKind::Tsx)),
            ("lib/a.mjs", Some(FileKind::Tsx)),
            ("lib/a.cjs", Some(FileKind::Tsx)),
            ("lib/a.json", Some(FileKind::Json)),
            ("Cargo.toml", Some(FileKind::Toml)),
            ("a/b.json", Some(FileKind::Json)),
            ("Makefile", Some(FileKind::Make)),
            ("sub/GNUmakefile", Some(FileKind::Make)),
            ("rules.mk", Some(FileKind::Make)),
            ("README.md", Some(FileKind::Markdown)),
            ("ci.yml", None),
            ("rs", None),
            ("Makefile.am", None),
        ] {
            assert_eq!(FileKind::of(path), kind, "{path}");
        }
    }
}
