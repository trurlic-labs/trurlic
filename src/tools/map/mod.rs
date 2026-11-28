//! The `map` tool: one level of the code tree, the nodes one node holds
//! with what lies in and under each and the imports between them, paged
//! within the result budget. No MCP client lists it; `trurlic map --json`
//! runs it.
//!
//! Its spec and handler are in [`tool`], how a level is cut into pages in
//! [`page`], and what a page shows of each child in [`child`].

mod child;
mod page;
mod tool;

pub(crate) use tool::MAP;
