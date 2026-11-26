//! What C and C++ need done to a file's bytes before the parse, since the
//! index runs no preprocessor and no query runs before the parse: the
//! macros the tree itself defines as attributes or specifiers
//! (`LEVELDB_EXPORT`, `FMT_CONSTEXPR`, `CURL_PRINTF(2, 3)`) or as a
//! namespace's opening and closing (`FMT_BEGIN_NAMESPACE`) are blanked,
//! so `class LEVELDB_EXPORT DB {` parses as the class it is; and the
//! comments of a `#define` that a token of it follows are blanked, line
//! breaks and all, since tree-sitter's C grammars read a comment, a break
//! or a blank run there as the directive's end.
//!
//! A blank is spaces in place of the bytes, so every offset holds: the
//! parse and the hash read the blanked text, in which a blanked name is no
//! token, and a span's lines are counted in the bytes as written. A
//! blankable object-like macro whose every body but an empty one holds
//! `static` (`#define local static`) makes what it marks private: the
//! blank states, beside the text, where the code after each such name
//! starts ([`Blanked::private`](blank::Blanked::private)), and the resolver reads a mark there as it
//! reads `static`. A name is blanked only where it stands outside every
//! directive, comment and literal, so the `#define` that made it blankable
//! and an `#ifdef` that tests it keep it; and a `#define` of a word either
//! grammar reads itself (`const`, `inline`, `__attribute__`) names no macro
//! the blank reads, since the parse already reads the word.
//!
//! Which names are blankable (`set.rs`) is a fact of the tree's headers
//! and of the file itself. The lexical pass of a file ([`lexed`]) gives
//! its `#define`s and the names it writes, and is cached by content; the
//! shell takes the tree's set from its headers' `#define`s, and each file
//! is indexed against the part of it that the file writes, its own
//! `#define`s judged with it. Each `#define`'s body is read up to
//! [`MAX_MACRO_BODY_TOKENS`] tokens, and a blanked call's arguments up to
//! [`MAX_BLANK_ARGUMENT_BYTES`], so each pass is linear in its bytes.
//!
//! What no rule sees: the members, enumerators and functions a
//! function-like macro generates (`BIT(flag);` in a struct), a macro
//! defined outside the tree (a system or generated header) or only in a
//! source file another file includes, and the segments of a namespace a
//! macro opens, which its definitions lack. And what no rule keeps out: a
//! header's macro reaches the files of the tree that cannot include it,
//! a vendored library's among them, but in a test place's (`set.rs`).

mod blank;
mod define;
mod encoding;
mod lex;
mod set;

pub(in crate::code) use blank::{MAX_BLANK_ARGUMENT_BYTES, blank};
pub(crate) use define::{Defines, Lexed, is_header, lexed};
pub(in crate::code) use define::{MAX_MACRO_BODY_TOKENS, tokens};
pub(crate) use encoding::{decode_lexed, encode_lexed, max_lexed_bytes};
pub(crate) use set::{BlankMacros, FileMacros};

#[cfg(test)]
mod tests;
