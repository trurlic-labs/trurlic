//! The `map` tools: one level of the code tree, the nodes one node holds
//! with what lies in and under each and the imports between them; one
//! file's frame; and a search of the tree's paths, each paged within the
//! result budget. No MCP client lists them; `trurlic map --json` runs
//! them.
//!
//! The level's spec and handler are in [`tool`], how a level is cut into
//! pages, and the fill every page shares, in [`page`], and what a page
//! shows of each child in [`child`]; a frame is in [`frame`] and a search
//! in [`search`].

mod child;
mod frame;
mod page;
mod search;
mod tool;

pub(crate) use frame::FRAME;
pub(crate) use search::SEARCH;
pub(crate) use tool::MAP;
