//! What tree-sitter-rust's tokens are made of beside its tree, which the
//! hash of a Rust definition reads.

use crate::code::parse::tokens::Lexicon;

/// tree-sitter-rust's lexical literals, each one token whole: its
/// prefix, delimiters and suffix hashed with its content. A
/// `negative_literal` is no lexical token: `-1` and `- 1` are one
/// expression.
const LITERALS: [&str; 5] = [
    "string_literal",
    "raw_string_literal",
    "char_literal",
    "integer_literal",
    "float_literal",
];

/// What tree-sitter-rust's tokens are made of beside its tree: its
/// literal kinds, and Rust's comments, block comments nesting.
pub(crate) const LEXICON: Lexicon = Lexicon {
    literals: &LITERALS,
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    nested_blocks: true,
    glued: &[("metavariable", 1)],
    continuations: false,
    splices: false,
    tokens: None,
};
