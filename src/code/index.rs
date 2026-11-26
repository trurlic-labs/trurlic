//! A file's index: the scopes of its paths, the definitions it holds,
//! each with its scope, what it is, its span and span hash, and for Rust
//! the module files a second step follows. An index is a function of the
//! file's kind and bytes alone, so a cache keyed by the two holds it
//! whatever path or tree the bytes are under.

use std::fmt;
use std::sync::LazyLock;

use super::kinds::{DefinitionKind, Role, Visibility};
use super::language::c_family::{self, FileMacros};
use super::language::python;
use super::language::{C, CPP, Language, PYTHON, TSX, TYPESCRIPT};
use super::scope::{MAX_NESTING, Scope, ScopeId, ScopeTree};
use super::span::{Span, SpanHash};
use super::symbol::{Segment, Syntax};
use super::{encoding, general, json, make, markdown, rust, syntax, toml, work};

/// The version of an index's shape and of the rules that build it. A
/// change to either is a new version, so no cached index built under the
/// old rules is read as one of the new (`tests::the_index_format_names_its_rules`).
pub(crate) const INDEX_FORMAT: u32 = 18;

/// The largest file a tree-sitter grammar indexes. A parse is bounded in
/// work and memory per byte of its file (`work.rs`), so this bounds what
/// one parse may take.
const TREE_SITTER_MAX_BYTES: usize = 1024 * 1024;

/// The largest file a lexical resolver indexes.
const LEXICAL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Every bound an index is built within, by name and value: in a cache
/// key, so an index built under other bounds is never read as one of
/// these, whatever the fixture corpus shows of them. The sizes of the
/// values an index holds bound it through the room of its file.
pub(crate) const BOUNDS: [(&str, usize); 30] = [
    ("tree_sitter_max_bytes", TREE_SITTER_MAX_BYTES),
    ("lexical_max_bytes", LEXICAL_MAX_BYTES),
    ("served_bytes_per_byte", work::SERVED_BYTES_PER_BYTE),
    ("parse_operations_per_byte", work::PARSE_OPERATIONS_PER_BYTE),
    ("min_parse_operations", work::MIN_PARSE_OPERATIONS),
    ("scan_work_per_byte", work::SCAN_WORK_PER_BYTE),
    ("lexical_work_per_byte", work::LEXICAL_WORK_PER_BYTE),
    ("min_work", work::MIN_WORK),
    ("chunk_bytes", syntax::CHUNK_BYTES),
    ("max_nesting", MAX_NESTING),
    ("scope_bytes", size_of::<Scope>()),
    ("segment_bytes", size_of::<Segment>()),
    ("definition_bytes", size_of::<Definition>()),
    ("module_file_bytes", size_of::<ModuleFile>()),
    ("text_bytes", size_of::<String>()),
    (
        "rust_room_per_byte",
        encoding::room_per_byte(FileKind::Rust),
    ),
    (
        "python_room_per_byte",
        encoding::room_per_byte(FileKind::Python),
    ),
    (
        "typescript_room_per_byte",
        encoding::room_per_byte(FileKind::TypeScript),
    ),
    ("tsx_room_per_byte", encoding::room_per_byte(FileKind::Tsx)),
    ("c_room_per_byte", encoding::room_per_byte(FileKind::C)),
    ("cpp_room_per_byte", encoding::room_per_byte(FileKind::Cpp)),
    (
        "toml_room_per_byte",
        encoding::room_per_byte(FileKind::Toml),
    ),
    (
        "json_room_per_byte",
        encoding::room_per_byte(FileKind::Json),
    ),
    (
        "make_room_per_byte",
        encoding::room_per_byte(FileKind::Make),
    ),
    (
        "markdown_room_per_byte",
        encoding::room_per_byte(FileKind::Markdown),
    ),
    ("entry_overhead_bytes", encoding::ENTRY_OVERHEAD_BYTES),
    ("max_indentations", python::MAX_INDENTATIONS),
    ("match_limit", general::MATCH_LIMIT as usize),
    ("max_macro_body_tokens", c_family::MAX_MACRO_BODY_TOKENS),
    (
        "max_blank_argument_bytes",
        c_family::MAX_BLANK_ARGUMENT_BYTES,
    ),
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
        let extension = name.rsplit_once('.').map(|(_, extension)| extension);
        match (name, extension) {
            ("Makefile" | "makefile" | "GNUmakefile", _) | (_, Some("mk")) => Some(Self::Make),
            (_, Some("rs")) => Some(Self::Rust),
            (_, Some("py" | "pyi" | "pyw")) => Some(Self::Python),
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

    /// The language the general resolver reads a file of this kind as;
    /// `None` for Rust and the lexical kinds.
    pub(super) const fn language(self) -> Option<&'static Language> {
        match self {
            Self::Python => Some(&PYTHON),
            Self::TypeScript => Some(&TYPESCRIPT),
            Self::Tsx => Some(&TSX),
            Self::C => Some(&C),
            Self::Cpp => Some(&CPP),
            Self::Rust | Self::Toml | Self::Json | Self::Make | Self::Markdown => None,
        }
    }

    /// Whether a file of this kind is source code, which the tree's facts
    /// hold whoever anchors it, rather than data or prose, read when an
    /// anchor names it.
    pub(crate) const fn is_code(self) -> bool {
        match self {
            Self::Rust | Self::Python | Self::TypeScript | Self::Tsx | Self::C | Self::Cpp => true,
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
            | Self::Tsx
            | Self::Toml
            | Self::Json
            | Self::Make
            | Self::Markdown => false,
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
            Self::C => C.syntax,
            Self::Cpp => CPP.syntax,
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
            Self::C => C.identity(),
            Self::Cpp => CPP.identity(),
            Self::Toml | Self::Json | Self::Make | Self::Markdown => [0; 32],
        }
    }

    /// The largest file of the kind the index reads.
    pub(crate) const fn max_bytes(self) -> usize {
        match self {
            Self::Rust | Self::Python | Self::TypeScript | Self::Tsx | Self::C | Self::Cpp => {
                TREE_SITTER_MAX_BYTES
            }
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
            Self::C => C.name,
            Self::Cpp => CPP.name,
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

/// The index of `bytes`, a file of `kind`; a C or C++ file blanks
/// `macros`, the part of its tree's set it writes. Each resolver builds
/// it into the scope tree of the file's kind and size, which holds it to
/// the file's room.
pub(crate) fn index(
    kind: FileKind,
    bytes: &[u8],
    macros: &FileMacros,
) -> Result<FileIndex, IndexFault> {
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
    let scopes = ScopeTree::for_file(kind, bytes.len());
    let mut index = match kind {
        FileKind::Rust => rust::index(text, scopes)?,
        FileKind::Python => general::index(&PYTHON, text, scopes)?,
        FileKind::TypeScript => general::index(&TYPESCRIPT, text, scopes)?,
        FileKind::Tsx => general::index(&TSX, text, scopes)?,
        FileKind::C => general::index(&C, &c_family::blank(text, macros), scopes)?,
        FileKind::Cpp => general::index(&CPP, &c_family::blank(text, macros), scopes)?,
        FileKind::Toml => toml::index(text, scopes)?,
        FileKind::Json => json::index(text, scopes)?,
        FileKind::Make => make::index(text, scopes)?,
        FileKind::Markdown => markdown::index(text, scopes)?,
    };
    index
        .definitions
        .sort_unstable_by_key(|definition| (definition.span, definition.scope));
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
    const FINGERPRINTS: [(u32, &str); 18] = [
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
        (
            12,
            "bd91583a0889da4fc819252847ce667d09c134e5594183ec3194c0114324036f",
        ),
        (
            13,
            "f66764382239698b97aeb30f3f17159783a21eaf945c1949822a6c49784e13b2",
        ),
        (
            14,
            "c134e6d6d4348dcb68d6ffe2c0f2a09a81950348e3cc07a8c228859d3d483070",
        ),
        (
            15,
            "36bdf51f6a436873ff2b9d5bff71ae04ee7822328b07ede46f67660458faa5c0",
        ),
        (
            16,
            "89b2cb1fdbc929505587e581dfaff355f5aae12ea5f9f6bef9be278cb4319629",
        ),
        (
            17,
            "eb68daf2f1971c228711184bee7978bd9ab0e601c9597e230359854906ac712e",
        ),
        (
            18,
            "1d7cc98fddd899e12c155f6d26dbadba9f0cbaefc3533938af62f45f153f708e",
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
        let kinds: Vec<(&std::path::PathBuf, FileKind)> = files
            .iter()
            .filter_map(|path| {
                let name = path.to_str().unwrap().trim_end_matches(".fixture");
                Some((path, FileKind::of(name)?))
            })
            .collect();
        let headers: Vec<c_family::Defines> = kinds
            .iter()
            .filter(|(path, kind)| {
                kind.blanks_macros() && c_family::is_header(path.to_str().unwrap())
            })
            .map(|(path, _)| c_family::lexed(&std::fs::read(path).unwrap()).into_defines())
            .collect();
        let tree = c_family::BlankMacros::of(&headers);
        let mut hasher = blake3::Hasher::new();
        for (path, kind) in kinds {
            let indexed =
                crate::code::testing::index_in(kind, &std::fs::read(path).unwrap(), &tree);
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
            crate::code::testing::index(FileKind::Rust, &over),
            Err(IndexFault::TooLarge {
                bytes: 1_048_577,
                max: 1_048_576,
            })
        );
        for kind in [FileKind::Python, FileKind::TypeScript, FileKind::Tsx] {
            assert_eq!(
                crate::code::testing::index(kind, &over).err(),
                crate::code::testing::index(FileKind::Rust, &over).err()
            );
            assert!(
                crate::code::testing::index(kind, &over[1..]).is_ok(),
                "{kind:?}"
            );
        }
        assert!(crate::code::testing::index(FileKind::Markdown, &over).is_ok());
        assert!(crate::code::testing::index(FileKind::Rust, &over[1..]).is_ok());
    }

    #[test]
    fn bytes_that_are_not_utf8_give_no_index() {
        assert_eq!(
            crate::code::testing::index(FileKind::Markdown, b"# ok\n\xff"),
            Err(IndexFault::NotUtf8 { at: 5 })
        );
    }
}
