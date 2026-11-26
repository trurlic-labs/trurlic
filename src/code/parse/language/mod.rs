//! The languages the general resolver reads, each a row of data
//! (`row.rs`): its grammar, its query files (`<name>.scm` here, compiled
//! into the binary by `include_str!`, the one include the index's purity
//! admits, and read as one query), how a symbol on its files is spelled,
//! the bounds its grammar needs checked before a parse (`<name>.rs`, for
//! a grammar that has any), and the places of a tree that hold its test
//! code; and what a file of each kind is read as (`kind.rs`). Adding a
//! language is a grammar crate, a query file, a row and a fixture corpus.
//! C and C++ add the one rewrite before a parse, the tree's attribute
//! macros blanked (`c_family/`), and read a file's bytes as their
//! preprocessor does (`c_lexer.rs`), both for the blank and for the
//! tokens of a macro's body in a hash.

mod c_family;
mod c_lexer;
mod kind;
mod python;
mod row;

pub(crate) use c_family::{
    BlankMacros, Defines, FileMacros, Lexed, decode_lexed, encode_lexed, is_header, lexed,
    lexed_grammars, max_lexed_bytes,
};
pub(in crate::code) use c_family::{MAX_BLANK_ARGUMENT_BYTES, MAX_MACRO_BODY_TOKENS, blank};
pub(crate) use kind::{grammar_identity_of, name_of};
pub(in crate::code) use kind::{language_of, syntax_of};
pub(in crate::code) use python::{MAX_INDENTATIONS, line_ends};
pub(in crate::code) use row::{C, CPP, Language, PYTHON, TSX, TYPESCRIPT, TYPESCRIPT_DECLARATIONS};
