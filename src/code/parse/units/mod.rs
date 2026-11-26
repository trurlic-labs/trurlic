//! The units of a tree's span hashes (`leaves.rs`), which `hash.rs`
//! hashes. A tree's tokens (`parse/tokens.rs`) are its units, each glued
//! kind of its grammar's lexicon two, a node its query marks by its
//! tokens one per token its lexicon's lexer finds in it (a C macro's
//! body), and one it marks by its characters as JSX renders it (JSX text,
//! `jsx.rs`), and a string's delimiter or a number it marks one spelled by
//! what it means (`spelling.rs`), and a string's text one with the text
//! that continues it, its escapes so spelled; a comment inside a token,
//! and a line continuation the lexicon splices there, is no part of it. A
//! grammar whose layout carries syntax that no byte holds (the statements
//! and blocks of Python, whose NEWLINE, INDENT and DEDENT are tokens of no
//! width in no node) marks the nodes whose extent is part of the hash:
//! each such group opens and closes with a mark no token's bytes give
//! ([`Leaves::of_marked`]).

mod hash;
mod jsx;
mod leaves;
mod spelling;
mod unit;

pub(in crate::code::parse) use leaves::{Leaves, NodeMarks};
