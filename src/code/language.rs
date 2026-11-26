//! The languages the general resolver reads, each a row of data: its
//! grammar, its query files (`language/<name>.scm`, compiled into the
//! binary by `include_str!`, the one include the index's purity admits,
//! and read as one query), how a symbol on its files is spelled, the
//! bounds its grammar needs checked before a parse (`language/<name>.rs`,
//! for a grammar that has any), and the places of a tree that hold its
//! test code. Adding a language is a grammar crate, a query file, a row
//! here and a fixture corpus. C and C++ add the one rewrite before a
//! parse, the tree's attribute macros blanked (`language/c_family.rs`).

use std::sync::OnceLock;

use super::index::IndexFault;

pub(super) mod c_family;
pub(super) mod python;
use super::query::{QueryFault, Rules};
use super::symbol::Syntax;
use super::syntax::{Lexicon, grammar_identity};

/// A check of a file's bytes before its parse.
type Bound = fn(&str) -> Result<(), IndexFault>;

/// A language the general resolver reads.
pub(super) struct Language {
    /// Its name in a cache key.
    pub(super) name: &'static str,
    grammar: fn() -> tree_sitter::Language,
    node_types: &'static str,
    /// The query files, read as one query in this order.
    queries: &'static [&'static str],
    /// How a symbol on its files is spelled.
    pub(super) syntax: Syntax,
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
    pub(super) test_places: &'static [&'static str],
    rules: OnceLock<Result<Rules, QueryFault>>,
    identity: OnceLock<[u8; 32]>,
}

pub(super) static PYTHON: Language = Language {
    name: "python",
    grammar: || tree_sitter_python::LANGUAGE.into(),
    node_types: tree_sitter_python::NODE_TYPES,
    queries: &[include_str!("language/python.scm")],
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
const TYPESCRIPT_QUERY: &str = include_str!("language/typescript.scm");

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
pub(super) static TYPESCRIPT: Language = Language {
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

/// `.tsx`, and JavaScript (`.js`, `.jsx`, `.mjs`, `.cjs`): the `tsx`
/// grammar reads JavaScript with its JSX, and TypeScript's query finds in
/// it what JavaScript has of TypeScript's definitions.
pub(super) static TSX: Language = Language {
    name: "tsx",
    grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    node_types: tree_sitter_typescript::TSX_NODE_TYPES,
    queries: &[TYPESCRIPT_QUERY, include_str!("language/tsx.scm")],
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
pub(super) static C: Language = Language {
    name: "c",
    grammar: || tree_sitter_c::LANGUAGE.into(),
    node_types: tree_sitter_c::NODE_TYPES,
    queries: &[include_str!("language/c.scm")],
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
pub(super) static CPP: Language = Language {
    name: "cpp",
    grammar: || tree_sitter_cpp::LANGUAGE.into(),
    node_types: tree_sitter_cpp::NODE_TYPES,
    queries: &[include_str!("language/cpp.scm")],
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

impl Language {
    /// Its query's rules, compiled once per process, on the first file
    /// of the language the cache does not hold.
    pub(super) fn rules(&self) -> Result<&Rules, &QueryFault> {
        self.rules.get_or_init(|| self.compile()).as_ref()
    }

    /// Its query compiled against its grammar and read by the contract:
    /// the cost a process pays once for the language.
    pub(super) fn compile(&self) -> Result<Rules, QueryFault> {
        Rules::compile((self.grammar)(), &self.queries.concat())
    }

    /// The identity of its grammar and its query, in a cache key: a
    /// pattern of the query is a rule of the index.
    pub(super) fn identity(&self) -> [u8; 32] {
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
    pub(super) fn is_test_place(&self, path: &str) -> bool {
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

    #[test]
    fn each_query_compiles_against_its_grammar_within_the_contract() {
        for language in [&PYTHON, &TYPESCRIPT, &TSX, &C, &CPP] {
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
}
