//! A file handed to the resolver of its kind: Rust's, the general
//! resolver over a language's row, or a lexical one, each building into
//! the scope tree of the file's kind and size. The version of the rules
//! ([`INDEX_FORMAT`]) and every bound an index is built within
//! ([`BOUNDS`]) are named here, over all of them.

use super::index::{
    Definition, Dependency, ENTRY_OVERHEAD_BYTES, FileIndex, FileKind, Import, IndexFault,
    LEXICAL_MAX_BYTES, LEXICAL_WORK_PER_BYTE, MAX_DOC_BYTES, MAX_NESTING, MIN_PARSE_OPERATIONS,
    MIN_WORK, Manifest, ModuleFile, PARSE_OPERATIONS_PER_BYTE, PathStep, SCAN_WORK_PER_BYTE,
    SERVED_BYTES_PER_BYTE, Scope, ScopeTree, Segment, TREE_SITTER_MAX_BYTES, WorkspaceDependency,
    room_per_byte,
};
use super::lexical::{json, make, markdown, toml};
use super::parse::general::{self, Source};
use super::parse::{
    C, CHUNK_BYTES, CPP, FileMacros, Language, MAX_BLANK_ARGUMENT_BYTES, MAX_INDENTATIONS,
    MAX_MACRO_BODY_TOKENS, OPERATIONS_PER_PROGRESS, PYTHON, TSX, TYPESCRIPT,
    TYPESCRIPT_DECLARATIONS, blank, line_ends, rust,
};

/// The version of an index's shape and of the rules that build it. A
/// change to either is a new version, so no cached index built under the
/// old rules is read as one of the new (`tests::the_index_format_names_its_rules`).
pub(crate) const INDEX_FORMAT: u32 = 25;

/// Every bound an index is built within, by name and value: in a cache
/// key, so an index built under other bounds is never read as one of
/// these, whatever the fixture corpus shows of them. The sizes of the
/// values an index holds bound it through the room of its file.
pub(crate) const BOUNDS: [(&str, usize); 38] = [
    ("tree_sitter_max_bytes", TREE_SITTER_MAX_BYTES),
    ("lexical_max_bytes", LEXICAL_MAX_BYTES),
    ("served_bytes_per_byte", SERVED_BYTES_PER_BYTE),
    ("parse_operations_per_byte", PARSE_OPERATIONS_PER_BYTE),
    ("min_parse_operations", MIN_PARSE_OPERATIONS),
    ("scan_work_per_byte", SCAN_WORK_PER_BYTE),
    ("lexical_work_per_byte", LEXICAL_WORK_PER_BYTE),
    ("min_work", MIN_WORK),
    ("chunk_bytes", CHUNK_BYTES),
    ("operations_per_progress", OPERATIONS_PER_PROGRESS),
    ("max_nesting", MAX_NESTING),
    ("max_doc_bytes", MAX_DOC_BYTES),
    ("scope_bytes", size_of::<Scope>()),
    ("segment_bytes", size_of::<Segment>()),
    ("definition_bytes", size_of::<Definition>()),
    ("module_file_bytes", size_of::<ModuleFile>()),
    ("path_step_bytes", size_of::<PathStep>()),
    ("import_bytes", size_of::<Import>()),
    ("manifest_bytes", size_of::<Manifest>()),
    ("dependency_bytes", size_of::<Dependency>()),
    (
        "workspace_dependency_bytes",
        size_of::<WorkspaceDependency>(),
    ),
    ("text_bytes", size_of::<String>()),
    ("rust_room_per_byte", room_per_byte(FileKind::Rust)),
    ("python_room_per_byte", room_per_byte(FileKind::Python)),
    (
        "typescript_room_per_byte",
        room_per_byte(FileKind::TypeScript),
    ),
    (
        "typescript_declarations_room_per_byte",
        room_per_byte(FileKind::TypeScriptDeclarations),
    ),
    ("tsx_room_per_byte", room_per_byte(FileKind::Tsx)),
    ("c_room_per_byte", room_per_byte(FileKind::C)),
    ("cpp_room_per_byte", room_per_byte(FileKind::Cpp)),
    ("toml_room_per_byte", room_per_byte(FileKind::Toml)),
    ("json_room_per_byte", room_per_byte(FileKind::Json)),
    ("make_room_per_byte", room_per_byte(FileKind::Make)),
    ("markdown_room_per_byte", room_per_byte(FileKind::Markdown)),
    ("entry_overhead_bytes", ENTRY_OVERHEAD_BYTES),
    ("max_indentations", MAX_INDENTATIONS),
    ("match_limit", general::MATCH_LIMIT as usize),
    ("max_macro_body_tokens", MAX_MACRO_BODY_TOKENS),
    ("max_blank_argument_bytes", MAX_BLANK_ARGUMENT_BYTES),
];

/// The index of `bytes`, a file of `kind`; a C or C++ file blanks
/// `macros`, the part of its tree's set it writes, and a Python file is
/// read with Python's line ends: each rewrite before the general
/// resolver's parse. Each resolver builds it into the scope tree of the
/// file's kind and size, which holds it to the file's room.
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
    let plain = Source::plain(text);
    let mut index = match kind {
        FileKind::Rust => rust::index(text, scopes)?,
        FileKind::Python => general::index(&PYTHON, &Source::plain(&line_ends(text)), scopes)?,
        FileKind::TypeScript => general::index(&TYPESCRIPT, &plain, scopes)?,
        FileKind::TypeScriptDeclarations => {
            general::index(&TYPESCRIPT_DECLARATIONS, &plain, scopes)?
        }
        FileKind::Tsx => general::index(&TSX, &plain, scopes)?,
        FileKind::C => blanked(&C, text, macros, scopes)?,
        FileKind::Cpp => blanked(&CPP, text, macros, scopes)?,
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

/// The index of `text`, a C or C++ file of `language`, read by the
/// general resolver with `macros` blanked, its lines counted as written.
fn blanked(
    language: &Language,
    text: &str,
    macros: &FileMacros,
    scopes: ScopeTree,
) -> Result<FileIndex, IndexFault> {
    let blanked = blank(text, macros);
    let source = Source {
        parsed: &blanked.text,
        written: text,
        private: &blanked.private,
    };
    general::index(language, &source, scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::parse::{BlankMacros, Defines, is_header, lexed};

    /// The fingerprint of the fixture corpus's indexes under each
    /// [`INDEX_FORMAT`], oldest first. A change to how a file is indexed
    /// moves it, and a cached index of the old rules would then be read as
    /// one of the new: such a change raises the format and appends its
    /// fingerprint. A change to the corpus alone pins the last one again.
    const FINGERPRINTS: [(u32, &str); 25] = [
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
        (
            19,
            "1b33aab31dac314239cb8c62360a4d11c14abd3b262e10fa8f919600611df05a",
        ),
        (
            20,
            "5214c21a480d9b11eef3b3f229d4d51335dd2c11daf9f29b93bdb94e5520b060",
        ),
        (
            21,
            "d077d147d557bb5b793c881a97e15288e906f50f92f2d5be7269b78cb0db447e",
        ),
        (
            22,
            "ec54ed666cb9d7730535b183f8bc05b134a3a39aaaf79188750ee89f1f74e1d2",
        ),
        (
            23,
            "d94a5cec24da0b901cc0978961f71e360d73817d2c329c2d713ad812bab0faf9",
        ),
        (
            24,
            "b331939c6df850e2f60124b8fb1b4fa890b8caa66cf6a557e308a653ca12b26f",
        ),
        (
            25,
            "0d5d2b7fa95c07673a84c77403d46e52648eaffc569880f0641d28149e4149c2",
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
        let kinds: Vec<(&std::path::PathBuf, &str, FileKind)> = files
            .iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&root).ok()?.to_str()?;
                let name = relative.trim_end_matches(".fixture");
                Some((path, name, FileKind::of(name)?))
            })
            .collect();
        let headers: Vec<(&str, Defines)> = kinds
            .iter()
            .filter(|(_, name, kind)| kind.blanks_macros() && is_header(name))
            .map(|(path, name, _)| (*name, lexed(&std::fs::read(path).unwrap()).into_defines()))
            .collect();
        let tree = BlankMacros::of(headers.iter().map(|(name, defines)| (*name, defines)));
        let mut hasher = blake3::Hasher::new();
        for (path, name, kind) in kinds {
            let bytes = std::fs::read(path).unwrap();
            let indexed = crate::code::testing::index_in(name, kind, &bytes, &tree);
            hasher.update(&crate::code::index::encode(&indexed));
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

    /// The BLAKE3 of the vendored tree-sitter runtime's C sources and
    /// headers, each path then its bytes, in path order. The runtime
    /// decides which parse is refused and what tree a parse gives, which no
    /// fixture covers whole, so a change to it raises [`INDEX_FORMAT`].
    const RUNTIME: &str = "90d7f42bc050cf09b4ae029083a3904b3a6fbb5734bf538669b9d87f82628345";

    #[test]
    fn the_vendored_runtime_is_the_one_the_index_format_names() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/tree-sitter");
        let mut files = Vec::new();
        let mut pending = vec![root.join("src"), root.join("include")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let source = matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("c" | "h")
                );
                match path.is_dir() {
                    true => pending.push(path),
                    false if source => files.push(path),
                    false => {}
                }
            }
        }
        files.sort();
        let mut hasher = blake3::Hasher::new();
        for path in &files {
            let name = path
                .strip_prefix(&root)
                .unwrap()
                .components()
                .map(|component| component.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            let bytes = std::fs::read(path).unwrap();
            for part in [name.as_bytes(), &bytes] {
                hasher.update(&u64::try_from(part.len()).unwrap().to_le_bytes());
                hasher.update(part);
            }
        }

        assert_eq!(
            hasher.finalize().to_hex().as_str(),
            RUNTIME,
            "the vendored tree-sitter runtime changed: raise INDEX_FORMAT and pin its hash again"
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
