//! How a file of a language is read with tree-sitter, the one place for
//! it: the parse within its budgets (`parser.rs`), the tokens of its tree
//! (`tokens.rs`), the units its span hashes are over (`units/`), the
//! builder every tree-sitter resolver adds its definitions to
//! (`tree_index.rs`), Rust's resolver (`rust/`), and the general resolver
//! (`general/`), which reads every other language by its row
//! (`language/`) under the contract of its query file (`query/`).

pub(in crate::code) mod general;
mod language;
mod parser;
mod query;
pub(in crate::code) mod rust;
mod tokens;
mod tree_index;
mod units;

pub(crate) use language::{
    BlankMacros, Defines, FileMacros, Lexed, decode_lexed, encode_lexed, grammar_identity_of,
    is_header, lexed, lexed_grammars, max_lexed_bytes, name_of,
};
pub(in crate::code) use language::{
    C, CPP, Language, MAX_BLANK_ARGUMENT_BYTES, MAX_INDENTATIONS, MAX_MACRO_BODY_TOKENS, PYTHON,
    TSX, TYPESCRIPT, TYPESCRIPT_DECLARATIONS, blank, language_of, line_ends, syntax_of,
};
pub(in crate::code) use parser::{CHUNK_BYTES, OPERATIONS_PER_PROGRESS};
#[cfg(test)]
pub(crate) use parser::{parse, text_of};
#[cfg(test)]
pub(in crate::code) use query::Rules;
#[cfg(test)]
pub(crate) use rust::LEXICON as RUST_LEXICON;
#[cfg(test)]
pub(crate) use tokens::{Token, is_comment, tokens};
