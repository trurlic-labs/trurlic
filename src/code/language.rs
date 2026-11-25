//! The languages the general resolver reads, each a row of data: its
//! grammar, its query file (`language/<name>.scm`, compiled into the
//! binary by `include_str!`, the one include the index's purity admits),
//! the bounds its grammar needs checked before a parse
//! (`language/<name>.rs`, for a grammar that has any), and the places of
//! a tree that hold its test code. Adding a language is a grammar crate,
//! a query file, a row here and a fixture corpus.

use std::sync::OnceLock;

use super::index::IndexFault;

pub(super) mod python;
use super::query::{QueryFault, Rules};
use super::syntax::{Lexicon, grammar_identity};

/// A check of a file's bytes before its parse.
type Bound = fn(&str) -> Result<(), IndexFault>;

/// A language the general resolver reads.
pub(super) struct Language {
    /// Its name in a cache key.
    pub(super) name: &'static str,
    grammar: fn() -> tree_sitter::Language,
    node_types: &'static str,
    query: &'static str,
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
    query: include_str!("language/python.scm"),
    // tree-sitter-python lexes every comment and line continuation as
    // an extra, never as a token or among bytes no node holds; a `#` at
    // a token's start is a string's text (`"#fff"`).
    lexicon: Lexicon {
        literals: &[],
        line_comments: &[],
        block_comments: &[],
        nested_blocks: false,
        glued: &[],
    },
    before_parse: &[python::check_indentations, python::check_continuations],
    test_places: &["tests/", "test_*.py", "*_test.py", "conftest.py"],
    rules: OnceLock::new(),
    identity: OnceLock::new(),
};

impl Language {
    /// Its query's rules, compiled once per process, on the first file
    /// of the language the cache does not hold.
    pub(super) fn rules(&self) -> Result<&Rules, &QueryFault> {
        self.rules
            .get_or_init(|| Rules::compile((self.grammar)(), self.query))
            .as_ref()
    }

    /// The identity of its grammar and its query, in a cache key: a
    /// pattern of the query, or its order, is a rule of the index.
    pub(super) fn identity(&self) -> [u8; 32] {
        *self.identity.get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&grammar_identity(&(self.grammar)(), self.node_types));
            hasher.update(self.query.as_bytes());
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
    fn the_python_query_compiles_against_its_grammar_within_the_contract() {
        if let Err(fault) = PYTHON.rules() {
            panic!("{fault}");
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
}
