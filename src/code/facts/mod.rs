//! Facts over a whole tree of indexes, which no one file's index states:
//! which crate holds each file, which files are test code whole, and what
//! an anchor names in the tree. Each is made from what the shell hands in
//! ([`CodeFacts`]), after every file's own index, so a file's cached index
//! stays a function of its kind and bytes alone.

mod code_facts;
mod crates;
mod definitions;
mod entry;
mod roles;

#[cfg(test)]
pub(crate) use code_facts::Target;
pub(crate) use code_facts::{Code, CodeFacts, Resolution};
pub(crate) use crates::MANIFEST;
pub(crate) use entry::{Entry, FileFault, ReadFault};
