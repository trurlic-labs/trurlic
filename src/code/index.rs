//! A file's index: the scopes of its paths, the definitions it holds,
//! each with its scope, what it is, its span and span hash, and for Rust
//! the facts a second step expands macros from. An index is a function
//! of the file's kind and bytes alone, so a cache keyed by the two holds
//! it whatever path or tree the bytes are under.

use std::fmt;
use std::sync::{Arc, LazyLock};

use super::kinds::{DefinitionKind, Role, Visibility};
use super::scope::{MAX_NESTING, Scope, ScopeId};
use super::span::{Span, SpanHash};
use super::symbol::{Segment, Syntax};
use super::{encoding, json, make, markdown, rust, syntax, toml, work};

/// The version of an index's shape and of the rules that build it. A
/// change to either is a new version, so no cached index built under the
/// old rules is read as one of the new (`tests::the_index_format_names_its_rules`).
pub(crate) const INDEX_FORMAT: u32 = 6;

/// The largest file a tree-sitter grammar indexes. tree-sitter's error
/// recovery takes a few microseconds a byte on hostile input, past what
/// its own count of steps shows, so the bound on its time is the bound
/// on the file's size.
const TREE_SITTER_MAX_BYTES: usize = 1024 * 1024;

/// The largest file a lexical resolver indexes.
const LEXICAL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Every bound an index is built within, by name and value: in a cache
/// key, so an index built under other bounds is never read as one of
/// these, whatever the fixture corpus shows of them. The expansion's
/// budget is no part of it: an expansion is never cached.
pub(crate) const BOUNDS: [(&str, usize); 13] = [
    ("tree_sitter_max_bytes", TREE_SITTER_MAX_BYTES),
    ("lexical_max_bytes", LEXICAL_MAX_BYTES),
    ("parse_work_per_byte", work::PARSE_WORK_PER_BYTE),
    ("scan_work_per_byte", work::SCAN_WORK_PER_BYTE),
    ("lexical_work_per_byte", work::LEXICAL_WORK_PER_BYTE),
    ("min_work", work::MIN_WORK),
    ("steps_per_report", syntax::STEPS_PER_REPORT),
    ("chunk_bytes", syntax::CHUNK_BYTES),
    ("max_nesting", MAX_NESTING),
    (
        "entry_bytes_per_source_byte",
        encoding::MAX_ENTRY_BYTES_PER_SOURCE_BYTE,
    ),
    ("entry_overhead_bytes", encoding::ENTRY_OVERHEAD_BYTES),
    ("max_arm_words", rust::MAX_ARM_WORDS),
    ("max_invocation_tokens", rust::MAX_INVOCATION_TOKENS),
];

/// The largest file of any kind the index reads.
pub(crate) const MAX_SOURCE_BYTES: usize = if TREE_SITTER_MAX_BYTES > LEXICAL_MAX_BYTES {
    TREE_SITTER_MAX_BYTES
} else {
    LEXICAL_MAX_BYTES
};

/// The identity of tree-sitter-rust, computed once.
static RUST_GRAMMAR: LazyLock<[u8; 32]> = LazyLock::new(|| {
    syntax::grammar_identity(
        &tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::NODE_TYPES,
    )
});

/// A kind of file the index has a resolver for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FileKind {
    Rust,
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
        let extension = name.rsplit_once('.').map(|(_, extension)| extension);
        match (name, extension) {
            ("Makefile" | "makefile" | "GNUmakefile", _) | (_, Some("mk")) => Some(Self::Make),
            (_, Some("rs")) => Some(Self::Rust),
            (_, Some("toml")) => Some(Self::Toml),
            (_, Some("json")) => Some(Self::Json),
            (_, Some("md")) => Some(Self::Markdown),
            _ => None,
        }
    }

    /// How a symbol on a file of this kind is spelled.
    pub(crate) const fn syntax(self) -> Syntax {
        match self {
            Self::Rust => Syntax::Rust,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => Syntax::Plain,
        }
    }

    /// The identity of the grammar the kind is parsed with, in a cache
    /// key: a grammar's new version may parse the same bytes into
    /// another tree, which no fixture of the index need show. A lexical
    /// kind's rules are the index's own, so its identity is
    /// [`INDEX_FORMAT`]'s.
    pub(crate) fn grammar(self) -> [u8; 32] {
        match self {
            Self::Rust => *RUST_GRAMMAR,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => [0; 32],
        }
    }

    /// The largest file of the kind the index reads.
    pub(crate) const fn max_bytes(self) -> usize {
        match self {
            Self::Rust => TREE_SITTER_MAX_BYTES,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => LEXICAL_MAX_BYTES,
        }
    }

    /// The kind's name in a cache key.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Make => "make",
            Self::Markdown => "markdown",
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
    /// whose visibility is the trait's, a type a macro invocation
    /// defines, and every lexical definition.
    pub(crate) visibility: Option<Visibility>,
}

/// A `macro_rules!` macro whose invocations can define a type: the arms
/// an invocation opens by its words, and every rule as a hand-off from
/// another macro meets it, with what its body gives a name it receives.
/// The names it gives are shared, so each invocation's expansion holds
/// them by reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeMacro {
    pub(crate) name: String,
    /// Marked `#[macro_export]`: its workspace invokes it, not its crate
    /// alone.
    pub(crate) exported: bool,
    pub(crate) arms: Vec<TypeArm>,
    /// Every rule, in order: the first a hand-off's arguments match is
    /// the one that receives the name, as in Rust.
    pub(crate) rules: Vec<Rule>,
}

/// An arm an invocation opens by literal words before the `$name:ident`
/// that receives the name: the words, what the pattern holds right after
/// the name, and the rule it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeArm {
    pub(crate) words: Vec<String>,
    pub(crate) after: After,
    /// The index of its rule in [`TypeMacro::rules`]; the name is the
    /// element after the words of its opening.
    pub(crate) rule: usize,
}

/// What an arm's pattern holds right after its `$name`, which the
/// invocation's token after the name must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum After {
    /// Nothing: the name is the invocation's last token.
    End,
    /// This token, or the opening delimiter of a group.
    Token(String),
    /// A binding or a repetition, which no single token states.
    Unchecked,
}

/// A rule as a hand-off meets it: the elements its pattern opens with,
/// at most [`MAX_OPENING`](super::rust::MAX_OPENING), and what each of its
/// `ident` bindings among them receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) opening: Vec<Element>,
    /// Whether the opening is the whole pattern.
    pub(crate) whole: bool,
    /// Each `ident` binding's place in the opening, and what its body
    /// does with the name it receives there.
    pub(crate) receives: Vec<(usize, Receives)>,
    /// The items its body names at its top level but types (`pub fn
    /// make()`), items of the invocation's container.
    pub(crate) items: Vec<(Arc<Segment>, DefinitionKind)>,
}

/// One element of a pattern's opening, or of a hand-off's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Element {
    Word(String),
    /// A punctuation token or a literal, which matches itself alone.
    Token(String),
    /// A delimited group, by its opening delimiter, and its tokens when
    /// they are all literal (`[summarized]`, `[]`); `None` when it holds a
    /// metavariable, a binding, a repetition or a group, which no literal
    /// states.
    Group(char, Option<Vec<String>>),
    /// A pattern's `$x:ident`.
    Ident,
    /// A pattern's binding of one token tree (`tt`, `literal`, `lifetime`).
    Tree,
    /// A hand-off's metavariable, whatever it holds.
    Variable,
    /// A binding of many tokens, or a repetition: what follows it cannot
    /// be lined up.
    Many,
}

/// What a rule's body does with a name it receives: the type it spells of
/// it (`struct $name`), each macro it hands the name to, and each `impl`
/// of it (`impl $name`, `impl<T> $name<T>`), whose items are the type's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Receives {
    pub(crate) kind: Option<DefinitionKind>,
    pub(crate) hand_offs: Vec<HandOff>,
    pub(crate) impls: Vec<TypeImpl>,
}

/// An invocation in a body that hands a name on. It holds the macro,
/// whether it is named through `$crate::` (the defining crate's macro),
/// and the place of the name among its arguments. The arguments are at
/// most [`MAX_OPENING`](super::rust::MAX_OPENING), and `whole` says
/// whether they are all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandOff {
    pub(crate) macro_name: String,
    pub(crate) from_crate: bool,
    pub(crate) arguments: Vec<Element>,
    pub(crate) whole: bool,
    pub(crate) place: usize,
}

/// An `impl` of the type a name defines, `impl $name`: its
/// trait's tokens for a trait impl, and the items it holds, the type's
/// members.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TypeImpl {
    /// The trait's tokens, the metavariable that names the type written
    /// `$` (`From < $ >`); `None` for an inherent impl.
    pub(crate) trait_tokens: Option<Vec<String>>,
    pub(crate) members: Vec<(Arc<Segment>, DefinitionKind)>,
}

/// A macro invocation in item position: what a type-defining macro of
/// the crate, or one of [`FOREIGN_TYPE_MACROS`](super::expand), would
/// make of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// The macro's name, its path dropped (`ir::id!` is `id`).
    pub(crate) macro_name: String,
    /// The scope of the container the invocation is in; `None` at the top.
    pub(crate) container: Option<ScopeId>,
    pub(crate) span: Span,
    pub(crate) hash: SpanHash,
    /// Its first tokens, its attributes and comments left out.
    pub(crate) tokens: Vec<String>,
    /// Test code when it stands in test code, or its attributes make it.
    pub(crate) role: Role,
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
    pub(crate) macros: Vec<TypeMacro>,
    /// Sorted by span.
    pub(crate) invocations: Vec<Invocation>,
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
    /// The grammar is of an ABI the parser does not read.
    Grammar,
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
            Self::Grammar => f.write_str("has a grammar of an ABI the parser does not read"),
        }
    }
}

/// The index of `bytes`, a file of `kind`.
pub(crate) fn index(kind: FileKind, bytes: &[u8]) -> Result<FileIndex, IndexFault> {
    let max = kind.max_bytes();
    if bytes.len() > max {
        return Err(IndexFault::TooLarge {
            bytes: bytes.len(),
            max,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|e| IndexFault::NotUtf8 {
        at: e.valid_up_to(),
    })?;
    let mut index = match kind {
        FileKind::Rust => rust::index(text)?,
        FileKind::Toml => toml::index(text)?,
        FileKind::Json => json::index(text)?,
        FileKind::Make => make::index(text)?,
        FileKind::Markdown => markdown::index(text)?,
    };
    index
        .definitions
        .sort_unstable_by_key(|definition| (definition.span, definition.scope));
    index
        .invocations
        .sort_unstable_by_key(|invocation| invocation.span);
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_kind_is_read_from_its_name() {
        for (path, kind) in [
            ("src/lib.rs", Some(FileKind::Rust)),
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

    /// The fingerprint of the fixture corpus's indexes under each
    /// [`INDEX_FORMAT`], oldest first. A change to how a file is indexed
    /// moves it, and a cached index of the old rules would then be read as
    /// one of the new: such a change raises the format and appends its
    /// fingerprint. A change to the corpus alone pins the last one again.
    const FINGERPRINTS: [(u32, &str); 6] = [
        (
            1,
            "1f2e5879839d9fe325832833197cf195d6326671d14d3c2582784605fa310bc6",
        ),
        (
            2,
            "01389c905b0ad5576b8b5dd4d082fc317d4995ca273fd3e29e1caa33e8374e72",
        ),
        (
            3,
            "6c4d79be6239ca763d78d5599795a7168c467dbaa482c60f38d8b4dd6e9dbd55",
        ),
        (
            4,
            "271d34c72dc4f8397e78ebeeffbb9a1b34c70b2cc03aff2a9d28ab4a553546b2",
        ),
        (
            5,
            "e90e33a198c4c8499706082d7e8ae1410f8e9929f8c3f22c309803611c6144f9",
        ),
        (
            6,
            "2311aee02372c269a380b0faaef22e11a092ac4a92922d9baca6d4433da4af92",
        ),
    ];

    #[test]
    fn the_index_format_names_its_rules() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code");
        let mut files = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                match path.is_dir() {
                    true => pending.push(path),
                    false => files.push(path),
                }
            }
        }
        files.sort();
        let mut hasher = blake3::Hasher::new();
        for path in files {
            let name = path.to_str().unwrap().trim_end_matches(".fixture");
            let Some(kind) = FileKind::of(name) else {
                continue;
            };
            let indexed = index(kind, &std::fs::read(&path).unwrap());
            hasher.update(&crate::code::encoding::encode(&indexed));
        }
        let fingerprint = hasher.finalize().to_hex().to_string();

        let formats: Vec<u32> = FINGERPRINTS.iter().map(|(format, _)| *format).collect();
        assert_eq!(formats, (1..=INDEX_FORMAT).collect::<Vec<_>>());
        assert_eq!(
            FINGERPRINTS.last(),
            Some(&(INDEX_FORMAT, fingerprint.as_str())),
            "the index of the fixture corpus changed: raise INDEX_FORMAT and append its fingerprint"
        );
    }

    #[test]
    fn a_file_past_its_kinds_size_is_refused_naming_the_bound() {
        let over = vec![b'\n'; TREE_SITTER_MAX_BYTES + 1];

        assert_eq!(
            index(FileKind::Rust, &over),
            Err(IndexFault::TooLarge {
                bytes: TREE_SITTER_MAX_BYTES + 1,
                max: TREE_SITTER_MAX_BYTES,
            })
        );
        assert!(index(FileKind::Markdown, &over).is_ok());
        assert!(index(FileKind::Rust, &over[1..]).is_ok());
    }

    #[test]
    fn bytes_that_are_not_utf8_give_no_index() {
        assert_eq!(
            index(FileKind::Markdown, b"# ok\n\xff"),
            Err(IndexFault::NotUtf8 { at: 5 })
        );
    }
}
