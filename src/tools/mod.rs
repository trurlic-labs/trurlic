//! The tool layer: each tool's arguments, handler and result, known to no
//! surface. A surface, the MCP server or a CLI command, finds a tool here
//! and calls it with the directory it serves; it gets back the tool's
//! typed result, or a [`ToolResult`] already fitted to the result budget,
//! and carries it in its own envelope. Nothing here speaks JSON-RPC, reads
//! or writes stdio, or builds MCP's result envelope or tool listing.

mod apply;
mod legacy;
mod registry;
mod result;
mod spec;
mod status;
mod truncate;

pub(crate) use apply::{APPLY, Answer, Duplicates};
pub(crate) use registry::{TOOLS, Tool};
pub(crate) use result::{ResultKind, ToolError, ToolResult};
pub(crate) use spec::{Destructive, Output, to_value};
pub(crate) use status::{
    AnchorCounts, Cut, LintItem, PAIRS_MORE, PairCounts, Part, PatternCounts, QualityCounts,
    Report, STATUS, section_names,
};
