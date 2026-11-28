//! Facts over a whole tree of indexes, which no one file's index states:
//! which crate holds each file, which files are test code whole, what an
//! anchor names in the tree, and the code tree of crates, modules and
//! files with the crates' edges and the imports between its nodes, read a
//! level at a time and each node by its address, a file's definitions and
//! where anchors lie in it, and a search of its paths. Each is
//! made from what the shell hands in ([`CodeFacts`]), after every file's
//! own index, so a file's cached index stays a function of its kind and
//! bytes alone.

mod address;
mod code_facts;
mod coverage;
mod crate_graph;
mod crates;
mod definitions;
mod entry;
mod frame;
mod imports;
mod level;
mod path;
mod places;
mod roles;
mod search;
mod tree;
mod walk;

#[cfg(test)]
pub(crate) use code_facts::Target;
pub(crate) use code_facts::{Code, CodeFacts, Resolution};
pub(crate) use coverage::SymbolAnchor;
pub(crate) use crate_graph::{Crate, Edge, Endpoint};
pub(crate) use crates::MANIFEST;
pub(crate) use entry::{Entry, FileFault, ReadFault};
pub(crate) use frame::{FileDefinition, Impls};
pub(crate) use level::{Covered, Level};
pub(crate) use places::{Module, Unresolved};
pub(crate) use search::{Found, Hit, Match};
pub(crate) use tree::{CodeTree, Node};
