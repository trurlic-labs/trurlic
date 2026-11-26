//! Tool argument and result specs: one declaration per tool yields both
//! the schemas a surface lists and the parser and writer the tool's calls
//! go through, so the two cannot drift. Its arguments are declared in
//! [`args`], with the kinds a field can hold in [`kinds`] and the refusal
//! of an argument in [`invalid`]; its result in [`output`]; the tool built
//! on both, and the entry point every surface calls it by, in [`tool`].
//!
//! Text limits are bytes, from `store::limits`. The schema states them as
//! `maxLength`, which counts characters: every string within the byte
//! limit meets it, and a non-ASCII string can meet it and still be refused.

mod args;
mod invalid;
pub(super) mod kinds;
pub(super) mod output;
pub(super) mod tool;

pub(super) use args::Kind;
#[cfg(test)]
pub(super) use args::parse;
pub(super) use args::{Args, Fields};
#[cfg(test)]
pub(super) use invalid::At;
pub(super) use output::Shown;
pub(crate) use output::{Output, to_value};
pub(crate) use tool::Destructive;
pub(super) use tool::SpecTool;
