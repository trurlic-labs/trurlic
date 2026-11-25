//! A file's index: the scopes of its paths, the definitions it holds,
//! each with its scope, what it is, its span and span hash, and for Rust
//! the facts a second step expands macros from and follows modules by. An index is a function
//! of the file's kind and bytes alone, so a cache keyed by the two holds
//! it whatever path or tree the bytes are under.

use std::fmt;
use std::sync::{Arc, LazyLock};

use super::kinds::{DefinitionKind, Role, Visibility};
use super::language::python;
use super::language::{Language, PYTHON, TSX, TYPESCRIPT};
use super::scope::{MAX_NESTING, Scope, ScopeId};
use super::span::{Span, SpanHash};
use super::symbol::{Segment, Syntax};
use super::{encoding, expand, general, json, make, markdown, rust, syntax, toml, work};

/// The version of an index's shape and of the rules that build it. A
/// change to either is a new version, so no cached index built under the
/// old rules is read as one of the new (`tests::the_index_format_names_its_rules`).
pub(crate) const INDEX_FORMAT: u32 = 11;

/// The largest file a tree-sitter grammar indexes. tree-sitter's error
/// recovery takes a few microseconds a byte on hostile input, past what
/// its own count of steps shows, so the bound on its time is the bound
/// on the file's size.
const TREE_SITTER_MAX_BYTES: usize = 1024 * 1024;

/// The largest file a lexical resolver indexes.
const LEXICAL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The stack a parse may take per byte of its file. tree-sitter releases
/// the stack of an ambiguous parse recursively, as deep as the ambiguity
/// runs: a chain of `a?(b):c=>` takes about 11 bytes a byte, which this
/// covers about three times.
const STACK_BYTES_PER_SOURCE_BYTE: usize = 32;

/// The stack of a thread that indexes a file of any size: a parse of a
/// file at the tree-sitter cap fits in it.
pub(crate) const PARSE_STACK_BYTES: usize = TREE_SITTER_MAX_BYTES * STACK_BYTES_PER_SOURCE_BYTE;

/// The largest file a worker that indexes many files indexes itself; a
/// larger one is indexed on a thread of its own. A thread keeps the memory
/// its parses freed until it ends, up to about 900 bytes per byte of its
/// largest file, so this bounds what each worker keeps to about 30 MB.
pub(crate) const MAX_BYTES_INDEXED_BY_A_WORKER: usize = 32 * 1024;

/// The stack of a worker: a parse of a file of
/// [`MAX_BYTES_INDEXED_BY_A_WORKER`] takes a quarter of it, and the index's
/// own frames the rest.
pub(crate) const WORKER_STACK_BYTES: usize =
    4 * MAX_BYTES_INDEXED_BY_A_WORKER * STACK_BYTES_PER_SOURCE_BYTE;

/// The bytes of the files indexed at once, between them. A parse holds
/// memory in proportion to its file, up to about 900 bytes a byte on
/// error-dense input, so this bounds what the parses under way hold to
/// about 2 GB; a file larger than it is indexed alone.
pub(crate) const MAX_BYTES_INDEXED_AT_ONCE: usize = 2 * TREE_SITTER_MAX_BYTES;

/// Every bound an index and its expansion are built within, by name and
/// value: in a cache key, so an index built under other bounds is never
/// read as one of these, whatever the fixture corpus shows of them.
pub(crate) const BOUNDS: [(&str, usize); 31] = [
    ("tree_sitter_max_bytes", TREE_SITTER_MAX_BYTES),
    ("lexical_max_bytes", LEXICAL_MAX_BYTES),
    ("parse_stack_bytes", PARSE_STACK_BYTES),
    (
        "max_bytes_indexed_by_a_worker",
        MAX_BYTES_INDEXED_BY_A_WORKER,
    ),
    ("worker_stack_bytes", WORKER_STACK_BYTES),
    ("max_bytes_indexed_at_once", MAX_BYTES_INDEXED_AT_ONCE),
    ("parse_work_per_byte", work::PARSE_WORK_PER_BYTE),
    ("scan_work_per_byte", work::SCAN_WORK_PER_BYTE),
    ("lexical_work_per_byte", work::LEXICAL_WORK_PER_BYTE),
    ("resolution_work_per_byte", work::RESOLUTION_WORK_PER_BYTE),
    ("expansion_bytes_per_byte", work::EXPANSION_BYTES_PER_BYTE),
    ("min_work", work::MIN_WORK),
    ("min_expansion_bytes", work::MIN_EXPANSION_BYTES),
    ("chunk_bytes", syntax::CHUNK_BYTES),
    ("max_nesting", MAX_NESTING),
    (
        "entry_bytes_per_source_byte",
        encoding::MAX_ENTRY_BYTES_PER_SOURCE_BYTE,
    ),
    ("entry_overhead_bytes", encoding::ENTRY_OVERHEAD_BYTES),
    ("max_entry_bytes", encoding::MAX_ENTRY_BYTES),
    ("scope_weight", encoding::weight::SCOPE),
    ("segment_weight", encoding::weight::SEGMENT),
    ("definition_weight", encoding::weight::DEFINITION),
    ("invocation_weight", encoding::weight::INVOCATION),
    ("module_file_weight", encoding::weight::MODULE_FILE),
    ("element_weight", encoding::weight::ELEMENT),
    ("macro_part_weight", encoding::weight::MACRO_PART),
    ("max_opening", rust::MAX_OPENING),
    ("max_group_tokens", rust::MAX_GROUP_TOKENS),
    ("max_hand_offs", expand::MAX_HAND_OFFS),
    ("max_indentations", python::MAX_INDENTATIONS),
    ("max_continuations", python::MAX_CONTINUATIONS),
    ("match_limit", general::MATCH_LIMIT as usize),
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

/// A kind of file the index has a resolver for: Rust's own, a language
/// the general resolver reads by its row in `language.rs`, or a lexical
/// kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FileKind {
    Rust,
    Python,
    TypeScript,
    /// TSX, and JavaScript, which the TSX grammar reads.
    Tsx,
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
            (_, Some("py" | "pyi" | "pyw")) => Some(Self::Python),
            (_, Some("ts" | "mts" | "cts")) => Some(Self::TypeScript),
            (_, Some("tsx" | "js" | "jsx" | "mjs" | "cjs")) => Some(Self::Tsx),
            (_, Some("toml")) => Some(Self::Toml),
            (_, Some("json")) => Some(Self::Json),
            (_, Some("md")) => Some(Self::Markdown),
            _ => None,
        }
    }

    /// The language the general resolver reads a file of this kind as;
    /// `None` for Rust and the lexical kinds.
    pub(super) const fn language(self) -> Option<&'static Language> {
        match self {
            Self::Python => Some(&PYTHON),
            Self::TypeScript => Some(&TYPESCRIPT),
            Self::Tsx => Some(&TSX),
            Self::Rust | Self::Toml | Self::Json | Self::Make | Self::Markdown => None,
        }
    }

    /// Whether a file of this kind is source code, which the tree's facts
    /// hold whoever anchors it, rather than data or prose, read when an
    /// anchor names it.
    pub(crate) const fn is_code(self) -> bool {
        match self {
            Self::Rust | Self::Python | Self::TypeScript | Self::Tsx => true,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => false,
        }
    }

    /// How a symbol on a file of this kind is spelled: a language's by
    /// its row.
    pub(crate) const fn syntax(self) -> Syntax {
        match self {
            Self::Rust => Syntax::Rust,
            Self::Python => PYTHON.syntax,
            Self::TypeScript => TYPESCRIPT.syntax,
            Self::Tsx => TSX.syntax,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => Syntax::Plain,
        }
    }

    /// The identity of the grammar the kind is parsed with, in a cache
    /// key: a grammar's new version may parse the same bytes into
    /// another tree, which no fixture of the index need show. A lexical
    /// kind's rules are the index's own, so its identity is
    /// [`INDEX_FORMAT`]'s. A language the general resolver reads adds
    /// its query file, whose patterns are rules of its index.
    pub(crate) fn grammar(self) -> [u8; 32] {
        match self {
            Self::Rust => *RUST_GRAMMAR,
            Self::Python => PYTHON.identity(),
            Self::TypeScript => TYPESCRIPT.identity(),
            Self::Tsx => TSX.identity(),
            Self::Toml | Self::Json | Self::Make | Self::Markdown => [0; 32],
        }
    }

    /// The largest file of the kind the index reads.
    pub(crate) const fn max_bytes(self) -> usize {
        match self {
            Self::Rust | Self::Python | Self::TypeScript | Self::Tsx => TREE_SITTER_MAX_BYTES,
            Self::Toml | Self::Json | Self::Make | Self::Markdown => LEXICAL_MAX_BYTES,
        }
    }

    /// The kind's name in a cache key.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => PYTHON.name,
            Self::TypeScript => TYPESCRIPT.name,
            Self::Tsx => TSX.name,
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

/// A `macro_rules!` macro whose invocations can define a type: every
/// rule, as an invocation or a hand-off from another macro meets it, with
/// what its body gives a name it receives. The names it gives are shared,
/// so each invocation's expansion holds them by reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeMacro {
    pub(crate) name: String,
    /// Marked `#[macro_export]`: its workspace invokes it, not its crate
    /// alone.
    pub(crate) exported: bool,
    /// Every rule, in order: Rust takes the first one an invocation's
    /// tokens match.
    pub(crate) rules: Vec<Rule>,
}

/// A rule as an invocation or a hand-off meets it: the elements its
/// pattern opens with, at most [`MAX_OPENING`](super::rust::MAX_OPENING),
/// what each of its bindings of one name receives, and the macros its
/// body hands a binding to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) opening: Vec<Element>,
    /// Whether the opening is the whole pattern.
    pub(crate) whole: bool,
    /// Each place of an `ident` or `tt` binding whose body spells a type
    /// of it or holds an `impl` of it, and what it does with the name.
    pub(crate) receives: Vec<(usize, Receives)>,
    /// Each distinct invocation at the body's top level that hands a
    /// binding of the opening on.
    pub(crate) hand_offs: Vec<HandOff>,
    /// The items its body names at its top level but types (`pub fn
    /// make()`), items of the invocation's container.
    pub(crate) items: Vec<(Arc<Segment>, DefinitionKind)>,
}

/// One element of a pattern's opening, of a hand-off's arguments, or of
/// an invocation's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Element {
    Word(String),
    /// A punctuation token or a literal, which matches itself alone.
    Token(String),
    /// A delimited group, by its opening delimiter, and its tokens when
    /// none is a group, a binding or a metavariable (`[summarized]`,
    /// `(u32)`), at most [`MAX_GROUP_TOKENS`](super::rust::MAX_GROUP_TOKENS);
    /// `None` when no literal states them.
    Group(char, Option<Vec<String>>),
    /// A pattern's binding of one fragment (`$x:ident`).
    Binding(Fragment),
    /// A hand-off's metavariable, which the rule's opening binds at this
    /// place when it does, holding the fragment it binds (a `tt` when it
    /// is not bound there).
    Forwarded(Option<usize>, Fragment),
    /// A repetition: what follows it cannot be lined up.
    Many,
}

/// What a binding takes: one identifier, one token tree, one literal,
/// one lifetime, or anything else (`expr`, `ty`, `vis` and the like),
/// which a pattern may take as many tokens and a hand-off forwards
/// opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fragment {
    Ident,
    Tree,
    Literal,
    Lifetime,
    Other,
}

impl Fragment {
    /// Every fragment, in the order of its code in an entry.
    pub(crate) const ALL: [Self; 5] = [
        Self::Ident,
        Self::Tree,
        Self::Literal,
        Self::Lifetime,
        Self::Other,
    ];
}

/// What a rule's body does with a name it receives: the type it spells of
/// it (`struct $name`), and each `impl` of it (`impl $name`, `impl<T>
/// $name<T>`), whose items are the type's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Receives {
    pub(crate) kind: Option<DefinitionKind>,
    pub(crate) impls: Vec<TypeImpl>,
}

/// An invocation in a body that hands a binding on. It holds the macro,
/// whether it is named through `$crate::` (the defining crate's macro),
/// and its arguments, at most [`MAX_OPENING`](super::rust::MAX_OPENING);
/// `whole` says whether they are all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandOff {
    pub(crate) macro_name: String,
    pub(crate) from_crate: bool,
    pub(crate) arguments: Vec<Element>,
    pub(crate) whole: bool,
}

/// An `impl` of the type a name defines, `impl $name`: its trait's tokens
/// for a trait impl, and the items it holds, the type's members.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TypeImpl {
    /// `None` for an inherent impl.
    pub(crate) trait_tokens: Option<Vec<TraitToken>>,
    pub(crate) members: Vec<(Arc<Segment>, DefinitionKind)>,
}

/// A token of the trait a macro's `impl` names: its text, or the name the
/// invocation defines.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TraitToken {
    Text(String),
    Name,
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
    /// Its first elements, its attributes and comments left out, at most
    /// [`MAX_OPENING`](super::rust::MAX_OPENING).
    pub(crate) arguments: Vec<Element>,
    /// Whether the arguments are all of them.
    pub(crate) whole: bool,
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
    /// Empty for every kind but Rust.
    pub(crate) rust: RustFacts,
}

/// What a Rust file holds for the second step over its crate: the
/// macros that can define a type, the invocations they expand, and the
/// modules declared to be in files of their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustFacts {
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
    /// The grammar is of an ABI the parser does not read, or its query
    /// does not compile.
    Grammar,
    /// A Python file whose lines start at more than `max` widths, past
    /// which its grammar's scanner aborts the process.
    TooManyIndentations { max: usize },
    /// A Python file with a run of more than `max` line continuations in
    /// one logical line, past which its grammar's error recovery runs
    /// quadratic.
    TooManyContinuations { max: usize },
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
            Self::TooManyContinuations { max } => {
                write!(f, "continues one line more than {max} times")
            }
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
        FileKind::Python => general::index(&PYTHON, text)?,
        FileKind::TypeScript => general::index(&TYPESCRIPT, text)?,
        FileKind::Tsx => general::index(&TSX, text)?,
        FileKind::Toml => toml::index(text)?,
        FileKind::Json => json::index(text)?,
        FileKind::Make => make::index(text)?,
        FileKind::Markdown => markdown::index(text)?,
    };
    index
        .definitions
        .sort_unstable_by_key(|definition| (definition.span, definition.scope));
    index
        .rust
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
            ("tools/run.py", Some(FileKind::Python)),
            ("pkg/__init__.pyi", Some(FileKind::Python)),
            ("tools/gui.pyw", Some(FileKind::Python)),
            ("src/app.ts", Some(FileKind::TypeScript)),
            ("types/index.d.ts", Some(FileKind::TypeScript)),
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

    /// The fingerprint of the fixture corpus's indexes under each
    /// [`INDEX_FORMAT`], oldest first. A change to how a file is indexed
    /// moves it, and a cached index of the old rules would then be read as
    /// one of the new: such a change raises the format and appends its
    /// fingerprint. A change to the corpus alone pins the last one again.
    const FINGERPRINTS: [(u32, &str); 11] = [
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
        (
            7,
            "18663a405b363c0ff6b43db6ec7cdb3db0d421b0ff7ebeaa82af7a241e13b183",
        ),
        (
            8,
            "102cfbc72d9beba458820443d3193fe8074dc29b42390c0c5b9c04ba1683c179",
        ),
        (
            9,
            "348f0b3b9c2a45d73863d6dd1a36c53951e2df349de51e3bc5448d5d717eb2e2",
        ),
        (
            10,
            "023fc1d422d4a842d67f26e6dda6a703ca0fd97b41d400ecf0f62c7dea1bca1b",
        ),
        (
            11,
            "bc0087ec4045b9e204a4c2ce352ca9833f40e3b7806b08d32b7b574a8194c238",
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
        let over = vec![b'\n'; 1_048_577];

        assert_eq!(
            index(FileKind::Rust, &over),
            Err(IndexFault::TooLarge {
                bytes: 1_048_577,
                max: 1_048_576,
            })
        );
        for kind in [FileKind::Python, FileKind::TypeScript, FileKind::Tsx] {
            assert_eq!(index(kind, &over).err(), index(FileKind::Rust, &over).err());
            assert!(index(kind, &over[1..]).is_ok(), "{kind:?}");
        }
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
