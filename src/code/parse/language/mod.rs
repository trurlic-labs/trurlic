//! The languages the general resolver reads, each a row of data: its
//! grammar, its query files (`<name>.scm` here, compiled into the
//! binary by `include_str!`, the one include the index's purity admits,
//! and read as one query), how a symbol on its files is spelled, the
//! bounds its grammar needs checked before a parse (`<name>.rs`, for a
//! grammar that has any), and the places of a tree that hold its test
//! code. Adding a language is a grammar crate, a query file, a row here
//! and a fixture corpus. C and C++ add the one rewrite before a parse,
//! the tree's attribute macros blanked (`c_family/`).

mod c_family;
mod python;

use std::sync::{LazyLock, OnceLock};

use super::query::{QueryFault, Rules};
use super::syntax::grammar_identity;
use super::tokens::Lexicon;
use crate::code::index::{FileKind, IndexFault, Syntax};

pub(crate) use c_family::{
    BlankMacros, Defines, FileMacros, Lexed, decode_lexed, encode_lexed, is_header, lexed,
    max_lexed_bytes,
};
pub(in crate::code) use c_family::{MAX_BLANK_ARGUMENT_BYTES, MAX_MACRO_BODY_TOKENS, blank};
pub(in crate::code) use python::{MAX_INDENTATIONS, line_ends};

/// A check of a file's bytes before its parse.
type Bound = fn(&str) -> Result<(), IndexFault>;

/// A language the general resolver reads.
pub(in crate::code) struct Language {
    /// Its name in a cache key.
    pub(in crate::code) name: &'static str,
    grammar: fn() -> tree_sitter::Language,
    node_types: &'static str,
    /// The query files, read as one query in this order.
    queries: &'static [&'static str],
    /// How a symbol on its files is spelled.
    syntax: Syntax,
    /// What its tokens are made of beside its tree. Its literals are its
    /// query's `@hash.unit` captures, not kinds; a comment it declares is
    /// one its grammar leaves among tokens, which a grammar that lexes
    /// every comment as an extra has none of.
    pub(super) lexicon: Lexicon,
    /// The bounds the bytes must meet before the parse, each a cost of the
    /// grammar no budget stops during it.
    pub(super) before_parse: &'static [Bound],
    /// The places of a tree whose files of the language are test code
    /// whole: `dir/` a directory of that name anywhere above the file, any
    /// other a file name, where one `*` stands for any run of characters.
    test_places: &'static [&'static str],
    rules: OnceLock<Result<Rules, QueryFault>>,
    identity: OnceLock<[u8; 32]>,
}

pub(in crate::code) static PYTHON: Language = Language {
    name: "python",
    grammar: || tree_sitter_python::LANGUAGE.into(),
    node_types: tree_sitter_python::NODE_TYPES,
    queries: &[include_str!("python.scm")],
    syntax: Syntax::Code,
    // tree-sitter-python lexes every comment as an extra, never as a
    // token or among bytes no node holds; a `#` at a token's start is a
    // string's text (`"#fff"`). A line continuation is an extra, or before
    // a string among bytes no node holds.
    lexicon: Lexicon {
        literals: &[],
        line_comments: &[],
        block_comments: &[],
        nested_blocks: false,
        glued: &[],
        continuations: true,
        splices: false,
        tokens: None,
    },
    before_parse: &[python::check_indentations],
    test_places: &["tests/", "test_*.py", "*_test.py", "conftest.py"],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// TypeScript's rules, beside which TSX reads its own: what a file
/// defines, how it is hashed, its test places.
const TYPESCRIPT_QUERY: &str = include_str!("typescript.scm");

/// tree-sitter-typescript lexes every comment as an extra; a `//` at a
/// token's start is a template string's text.
const TYPESCRIPT_LEXICON: Lexicon = Lexicon {
    literals: &[],
    line_comments: &[],
    block_comments: &[],
    nested_blocks: false,
    glued: &[],
    continuations: false,
    splices: false,
    tokens: None,
};

/// `.ts`, `.mts` and `.cts`, read by the `typescript` grammar, which
/// takes `<T>x` as a cast where `tsx` reads an element.
pub(in crate::code) static TYPESCRIPT: Language = Language {
    name: "typescript",
    grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    node_types: tree_sitter_typescript::TYPESCRIPT_NODE_TYPES,
    queries: &[TYPESCRIPT_QUERY],
    syntax: Syntax::Strings,
    lexicon: TYPESCRIPT_LEXICON,
    before_parse: &[],
    test_places: &[
        "__tests__/",
        "*.test.ts",
        "*.spec.ts",
        "*.test.mts",
        "*.spec.mts",
        "*.test.cts",
        "*.spec.cts",
    ],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// `.d.ts`, `.d.mts` and `.d.cts`, TypeScript's declaration files: the
/// `typescript` grammar and TypeScript's query, with the rules of a
/// declaration file's top beside them, which TypeScript reads as an
/// ambient module's body.
pub(in crate::code) static TYPESCRIPT_DECLARATIONS: Language = Language {
    name: "typescript-declarations",
    grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    node_types: tree_sitter_typescript::TYPESCRIPT_NODE_TYPES,
    queries: &[TYPESCRIPT_QUERY, include_str!("declarations.scm")],
    syntax: Syntax::Strings,
    lexicon: TYPESCRIPT_LEXICON,
    before_parse: &[],
    test_places: &["__tests__/"],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// `.tsx`, and JavaScript (`.js`, `.jsx`, `.mjs`, `.cjs`): the `tsx`
/// grammar reads JavaScript with its JSX, and TypeScript's query finds in
/// it what JavaScript has of TypeScript's definitions.
pub(in crate::code) static TSX: Language = Language {
    name: "tsx",
    grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    node_types: tree_sitter_typescript::TSX_NODE_TYPES,
    queries: &[TYPESCRIPT_QUERY, include_str!("tsx.scm")],
    syntax: Syntax::Strings,
    lexicon: TYPESCRIPT_LEXICON,
    before_parse: &[],
    test_places: &[
        "__tests__/",
        "*.test.tsx",
        "*.spec.tsx",
        "*.test.js",
        "*.spec.js",
        "*.test.jsx",
        "*.spec.jsx",
        "*.test.mjs",
        "*.spec.mjs",
        "*.test.cjs",
        "*.spec.cjs",
    ],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// tree-sitter-c and tree-sitter-cpp lex every comment as an extra; a
/// line continuation is in no node, and blank where it lies but inside
/// a token; a macro's body is hashed by the tokens of the C lexer.
const C_FAMILY_LEXICON: Lexicon = Lexicon {
    literals: &[],
    line_comments: &[],
    block_comments: &[],
    nested_blocks: false,
    glued: &[],
    continuations: true,
    splices: true,
    tokens: Some(c_family::tokens),
};

/// `.c`, read by the `c` grammar.
pub(in crate::code) static C: Language = Language {
    name: "c",
    grammar: || tree_sitter_c::LANGUAGE.into(),
    node_types: tree_sitter_c::NODE_TYPES,
    queries: &[include_str!("c.scm")],
    syntax: Syntax::Code,
    lexicon: C_FAMILY_LEXICON,
    before_parse: &[],
    test_places: &["tests/", "test/", "*_test.c", "*_unittest.c"],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// `.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh`, and `.h`, a header of C or of
/// C++, read by the `cpp` grammar. Its symbols are text, so `operator()`
/// and `operator<` are spelled as written.
pub(in crate::code) static CPP: Language = Language {
    name: "cpp",
    grammar: || tree_sitter_cpp::LANGUAGE.into(),
    node_types: tree_sitter_cpp::NODE_TYPES,
    queries: &[include_str!("cpp.scm")],
    syntax: Syntax::Strings,
    lexicon: C_FAMILY_LEXICON,
    before_parse: &[],
    test_places: &[
        "tests/",
        "test/",
        "*_test.h",
        "*_test.cc",
        "*_test.cpp",
        "*_test.cxx",
        "*_test.hpp",
        "*_test.hh",
        "*_unittest.h",
        "*_unittest.cc",
        "*_unittest.cpp",
        "*_unittest.cxx",
        "*_unittest.hpp",
        "*_unittest.hh",
    ],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

/// The identity of tree-sitter-rust, computed once.
static RUST_GRAMMAR: LazyLock<[u8; 32]> = LazyLock::new(|| {
    grammar_identity(
        &tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::NODE_TYPES,
    )
});

/// What a file of each kind is read as: the language row of a kind the
/// general resolver reads, the spelling of its symbols, its grammar's
/// identity and its name in a cache key.
impl FileKind {
    /// The language the general resolver reads a file of this kind as;
    /// `None` for Rust and the lexical kinds.
    pub(in crate::code) const fn language(self) -> Option<&'static Language> {
        match self {
            Self::Python => Some(&PYTHON),
            Self::TypeScript => Some(&TYPESCRIPT),
            Self::TypeScriptDeclarations => Some(&TYPESCRIPT_DECLARATIONS),
            Self::Tsx => Some(&TSX),
            Self::C => Some(&C),
            Self::Cpp => Some(&CPP),
            Self::Rust | Self::Toml | Self::Json | Self::Make | Self::Markdown => None,
        }
    }

    /// How a symbol on a file of this kind is spelled: a language's by
    /// its row.
    pub(crate) const fn syntax(self) -> Syntax {
        match self {
            Self::Rust => Syntax::Rust,
            Self::Python => PYTHON.syntax,
            Self::TypeScript => TYPESCRIPT.syntax,
            Self::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.syntax,
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
    /// [`INDEX_FORMAT`](crate::code::INDEX_FORMAT)'s. A language the general resolver reads adds
    /// its query file, whose patterns are rules of its index.
    pub(crate) fn grammar(self) -> [u8; 32] {
        match self {
            Self::Rust => *RUST_GRAMMAR,
            Self::Python => PYTHON.identity(),
            Self::TypeScript => TYPESCRIPT.identity(),
            Self::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.identity(),
            Self::Tsx => TSX.identity(),
            Self::C => C.identity(),
            Self::Cpp => CPP.identity(),
            Self::Toml | Self::Json | Self::Make | Self::Markdown => [0; 32],
        }
    }

    /// The kind's name in a cache key.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => PYTHON.name,
            Self::TypeScript => TYPESCRIPT.name,
            Self::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.name,
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

impl Language {
    /// Its query's rules, compiled once per process, on the first file
    /// of the language the cache does not hold.
    pub(super) fn rules(&self) -> Result<&Rules, &QueryFault> {
        self.rules.get_or_init(|| self.compile()).as_ref()
    }

    /// Its query compiled against its grammar and read by the contract:
    /// the cost a process pays once for the language.
    pub(in crate::code) fn compile(&self) -> Result<Rules, QueryFault> {
        Rules::compile((self.grammar)(), &self.queries.concat())
    }

    /// The identity of its grammar and its query, in a cache key: a
    /// pattern of the query is a rule of the index.
    fn identity(&self) -> [u8; 32] {
        *self.identity.get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&grammar_identity(&(self.grammar)(), self.node_types));
            for query in self.queries {
                hasher.update(&(query.len() as u64).to_le_bytes());
                hasher.update(query.as_bytes());
            }
            *hasher.finalize().as_bytes()
        })
    }

    /// Whether the file at `path`, a `/`-separated path of a tree, is in
    /// one of its test places.
    pub(in crate::code) fn is_test_place(&self, path: &str) -> bool {
        let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
        self.test_places
            .iter()
            .any(|place| match place.strip_suffix('/') {
                Some(dir) => dirs.split('/').any(|segment| segment == dir),
                None => matches_name(place, name),
            })
    }
}

/// Whether `name` matches `pattern`, where one `*` stands for any run of
/// characters.
fn matches_name(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((before, after)) => {
            name.len() >= before.len() + after.len()
                && name.starts_with(before)
                && name.ends_with(after)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::FileKind;
    use crate::code::parse::{syntax, tokens};

    #[test]
    fn each_query_compiles_against_its_grammar_within_the_contract() {
        for language in [
            &PYTHON,
            &TYPESCRIPT,
            &TYPESCRIPT_DECLARATIONS,
            &TSX,
            &C,
            &CPP,
        ] {
            if let Err(fault) = language.rules() {
                panic!("{}: {fault}", language.name);
            }
        }
    }

    #[test]
    fn a_test_place_is_a_directory_anywhere_above_or_a_file_name() {
        for (path, expected) in [
            ("tests/helpers.py", true),
            ("pkg/tests/deep/helpers.py", true),
            ("pkg/test_store.py", true),
            ("store_test.py", true),
            ("pkg/conftest.py", true),
            ("pkg/testing.py", false),
            ("pkg/attests/x.py", false),
            ("test_.pyc", false),
            ("pkg/store.py", false),
            ("conftest.pyi", false),
        ] {
            assert_eq!(PYTHON.is_test_place(path), expected, "{path}");
        }
    }

    #[test]
    fn a_typescript_test_place_is_a_tests_directory_or_a_test_or_spec_file() {
        for (language, path, expected) in [
            (&TYPESCRIPT, "src/__tests__/store.ts", true),
            (&TYPESCRIPT, "src/store.test.ts", true),
            (&TYPESCRIPT, "src/store.spec.mts", true),
            (&TYPESCRIPT, "src/store.ts", false),
            (&TYPESCRIPT, "src/test.ts", false),
            (&TYPESCRIPT, "tests/store.ts", false),
            (&TSX, "ui/card.test.tsx", true),
            (&TSX, "lib/store.spec.js", true),
            (&TSX, "lib/__tests__/helpers.cjs", true),
            (&TSX, "lib/store.test.mjs", true),
            (&TSX, "lib/store.js", false),
        ] {
            assert_eq!(language.is_test_place(path), expected, "{path}");
        }
    }

    #[test]
    fn a_c_family_test_place_is_a_tests_or_test_directory_or_a_test_file() {
        for (language, path, expected) in [
            (&C, "tests/unit/lib500.c", true),
            (&C, "deps/x/test/unit.c", true),
            (&C, "db/c_test.c", true),
            (&C, "db/c_unittest.c", true),
            (&C, "src/test.c", false),
            (&C, "src/testing.c", false),
            (&CPP, "db/db_test.cc", true),
            (&CPP, "util/env_unittest.cpp", true),
            (&CPP, "test/format-test.cc", true),
            (&CPP, "include/shelf_test.hpp", true),
            (&CPP, "src/test_shelf.cc", false),
            (&CPP, "src/shelf.h", false),
        ] {
            assert_eq!(language.is_test_place(path), expected, "{path}");
        }
    }

    /// The fixture corpus of each language the general resolver reads, and a
    /// file per language of comments, continued lines and strings that open
    /// like a comment.
    fn general_corpus() -> Vec<(String, FileKind, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code");
        let mut found = vec![
        (
            "lexemes.py".to_owned(),
            FileKind::Python,
            "x = 1 + \\\n    2  # sum\ny = '#not' f'{x: >4} #' \\\n  \"\"\"# doc\"\"\"\n#!z\n"
                .to_owned(),
        ),
        (
            "lexemes.tsx".to_owned(),
            FileKind::Tsx,
            "#!/usr/bin/env node\nconst t = `//a ${b /* c */} /*d*/`; // e\nconst r = /\\/\\*x/g;\n<!-- f\nconst j = <p> a  // b </p>;\n"
                .to_owned(),
        ),
        (
            "lexemes.cc".to_owned(),
            FileKind::Cpp,
            "#define M(a) ((a) + \\\n  1) // m\nint x = 1 + \\\n  2; /* c */\nauto s = R\"x(/* no */)x\" \"//\";\n"
                .to_owned(),
        ),
    ];
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Some(kind) =
                    FileKind::of(path.to_str().unwrap()).filter(|kind| kind.language().is_some())
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    found.push((path.display().to_string(), kind, text));
                }
            }
        }
        found
    }

    #[test]
    fn every_byte_but_comments_and_whitespace_is_in_one_token() {
        for (file, kind, text) in general_corpus() {
            let language = kind.language().unwrap();
            let rules = language.rules().unwrap();
            let tree = syntax::parse(&text, &rules.grammar).unwrap();
            let mut covered = vec![false; text.len()];
            tokens::tokens(tree.root_node(), &text, &language.lexicon, |token| {
                let (from, to) = match token {
                    tokens::Token::Node(node) => (node.start_byte(), node.end_byte()),
                    tokens::Token::Gap { start, end } | tokens::Token::Run { start, end } => {
                        (start, end)
                    }
                    tokens::Token::Hole { .. }
                    | tokens::Token::Enter(_)
                    | tokens::Token::Leave(_) => (0, 0),
                };
                for byte in &mut covered[from..to] {
                    assert!(!*byte, "{file}: byte {from} in two tokens");
                    *byte = true;
                }
                std::ops::ControlFlow::Continue(())
            });
            let mut extras = vec![false; text.len()];
            let mut cursor = tree.walk();
            'walk: loop {
                let node = cursor.node();
                if tokens::is_comment(node) {
                    assert!(
                        matches!(
                            node.kind(),
                            "comment" | "html_comment" | "line_continuation"
                        ),
                        "{file}: an extra {}",
                        node.kind()
                    );
                    extras[node.byte_range()].fill(true);
                } else if cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break 'walk;
                    }
                }
            }
            for (at, c) in text.char_indices() {
                assert!(
                    covered[at]
                        || extras[at]
                        || c.is_whitespace()
                        || language.lexicon.continuation(&text[at..]) > 0,
                    "{file}: byte {at} ({c:?}) is in no token"
                );
            }
        }
    }
}
