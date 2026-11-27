//! Facts over a whole tree of indexes, which no one file's index states:
//! which crate holds each file, which files are test code whole, what an
//! anchor names in the tree, and the code tree of crates, modules and
//! files with the crates' edges and the imports between its nodes. Each is
//! made from what the shell hands in ([`CodeFacts`]), after every file's
//! own index, so a file's cached index stays a function of its kind and
//! bytes alone.

mod code_facts;
mod crate_graph;
mod crates;
mod definitions;
mod entry;
mod imports;
mod path;
mod places;
mod roles;
mod tree;
mod walk;

#[cfg(test)]
pub(crate) use code_facts::Target;
pub(crate) use code_facts::{Code, CodeFacts, Resolution};
pub(crate) use crates::MANIFEST;
pub(crate) use entry::{Entry, FileFault, ReadFault};
#[cfg(test)]
pub(crate) use tree::{CodeTree, Node};
