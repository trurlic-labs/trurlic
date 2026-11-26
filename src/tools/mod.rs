//! The tool layer: each tool's arguments, handler and result, known to no
//! surface. A surface, the MCP server or a CLI command, finds a tool here
//! and calls it with the directory it serves; it gets back the tool's
//! typed result, or a [`ToolResult`] already fitted to the result budget,
//! and carries it in its own envelope. The layer speaks no JSON-RPC and
//! builds neither MCP's result envelope nor its tool listing.

mod apply;
mod reads;
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::purity::Crate;

    /// What the layer's non-test code may name in the crate: itself, the
    /// modules its arrow of the module map reaches, the error and the
    /// console, and the failpoints. No surface (`mcp`, `commands`, `cli`,
    /// the crate root's `main` or `bench`) is on it.
    const NAMED: [&str; 9] = [
        "tools",
        "store",
        "lint",
        "sources",
        "budget",
        "discovery",
        "Error",
        "console",
        "failpoint",
    ];

    /// Every path the layer names into the crate goes into [`NAMED`],
    /// however it is spelled (`crate::purity` states what the check
    /// cannot see).
    #[test]
    fn the_tool_layer_names_no_surface() {
        let source = Crate::read(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert_eq!(source.places_beyond("tools", &NAMED), Vec::<String>::new());
    }
}
