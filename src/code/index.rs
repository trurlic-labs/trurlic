//! A file's index: the definitions it holds, each with its path, span and
//! span hash, and for Rust the facts a second step expands macros from.
//! An index is a function of the file's kind and bytes alone, so a cache
//! keyed by the two holds it whatever path or tree the bytes are under.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::span::{Lines, Span, SpanHash};
use super::symbol::{Segment, Syntax};
use super::{json, make, markdown, rust, toml};

/// The version of an index's shape and of the rules that build it. A
/// change to either is a new version, so no cached index built under the
/// old rules is read as one of the new (`tests::the_index_format_names_its_rules`).
pub(crate) const INDEX_FORMAT: u32 = 1;

/// The containers a definition may be nested in. A file that nests one
/// deeper has no index: every definition's path is a copy of its
/// container's, so the nesting bounds the index's size.
pub(crate) const MAX_NESTING: usize = 64;

/// A kind of file the index has a resolver for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// One definition: its path from the outermost container, where it is,
/// and the hash of what it holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Definition {
    pub(crate) path: Vec<Segment>,
    pub(crate) span: Span,
    pub(crate) hash: SpanHash,
}

impl Definition {
    /// The definition at `path` of bytes `start..end` of `text`, a file a
    /// lexical resolver reads: its hash is of the whitespace-normalized
    /// text.
    pub(super) fn lexical(
        path: Vec<Segment>,
        text: &str,
        lines: &Lines,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            path,
            span: lines.span(start, end),
            hash: SpanHash::of_text(text.get(start..end).unwrap_or_default()),
        }
    }
}

/// A `macro_rules!` macro whose invocations can define a type: the
/// literal words each such arm opens with before the `$name:ident` its
/// body defines a type of, and the names its body gives items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypeMacro {
    pub(crate) name: String,
    /// Marked `#[macro_export]`: its workspace invokes it, not its crate
    /// alone.
    pub(crate) exported: bool,
    pub(crate) arms: Vec<Vec<String>>,
    pub(crate) members: Vec<String>,
}

/// A macro invocation in item position: what a type-defining macro of
/// the crate, or one of [`FOREIGN_TYPE_MACROS`](super::expand), would
/// make of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Invocation {
    /// The macro's name, its path dropped (`ir::id!` is `id`).
    pub(crate) macro_name: String,
    /// The path of the container the invocation is in.
    pub(crate) container: Vec<Segment>,
    pub(crate) span: Span,
    pub(crate) hash: SpanHash,
    /// Its first tokens, its attributes and comments left out.
    pub(crate) tokens: Vec<String>,
}

/// What a file of a kind with a resolver defines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileIndex {
    /// Sorted by span, then path.
    pub(crate) definitions: Vec<Definition>,
    pub(crate) macros: Vec<TypeMacro>,
    /// Sorted by span.
    pub(crate) invocations: Vec<Invocation>,
}

/// Why a file's bytes give no index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IndexFault {
    /// The bytes are not UTF-8 from byte `at` on.
    NotUtf8 { at: usize },
    /// A TOML or JSON file that does not parse, from about byte `at`.
    Syntax { at: usize },
    /// The parser gave up past its budget of steps for the file's size.
    TooComplex,
    /// A definition nested in more than [`MAX_NESTING`] containers.
    TooDeep,
    /// The grammar is of an ABI the parser does not read.
    Grammar,
}

impl fmt::Display for IndexFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 { at } => write!(f, "is not UTF-8 from byte {at}"),
            Self::Syntax { at } => write!(f, "does not parse, near byte {at}"),
            Self::TooComplex => f.write_str("takes the parser past its budget of steps"),
            Self::TooDeep => write!(f, "nests definitions deeper than {MAX_NESTING} containers"),
            Self::Grammar => f.write_str("has a grammar of an ABI the parser does not read"),
        }
    }
}

/// The index of `bytes`, a file of `kind`.
pub(crate) fn index(kind: FileKind, bytes: &[u8]) -> Result<FileIndex, IndexFault> {
    let text = std::str::from_utf8(bytes).map_err(|e| IndexFault::NotUtf8 {
        at: e.valid_up_to(),
    })?;
    let mut index = match kind {
        FileKind::Rust => rust::index(text)?,
        FileKind::Toml => definitions_only(toml::definitions(text)?),
        FileKind::Json => definitions_only(json::definitions(text)?),
        FileKind::Make => definitions_only(make::definitions(text)),
        FileKind::Markdown => definitions_only(markdown::definitions(text)),
    };
    index
        .definitions
        .sort_unstable_by(|a, b| a.span.cmp(&b.span).then_with(|| a.path.cmp(&b.path)));
    index
        .invocations
        .sort_unstable_by_key(|invocation| invocation.span);
    Ok(index)
}

fn definitions_only(definitions: Vec<Definition>) -> FileIndex {
    FileIndex {
        definitions,
        ..FileIndex::default()
    }
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
    const FINGERPRINTS: [(u32, &str); 1] = [(
        1,
        "1f2e5879839d9fe325832833197cf195d6326671d14d3c2582784605fa310bc6",
    )];

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
            hasher.update(&serde_json::to_vec(&indexed).unwrap());
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
    fn bytes_that_are_not_utf8_give_no_index() {
        assert_eq!(
            index(FileKind::Markdown, b"# ok\n\xff"),
            Err(IndexFault::NotUtf8 { at: 5 })
        );
    }
}
