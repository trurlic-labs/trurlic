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

    use crate::purity::places_spelling;

    /// No non-test code of the layer spells `mcp`, `commands` or `cli`, so
    /// no path, `use` list or `super` chain of it reaches a surface.
    #[test]
    fn the_tool_layer_names_no_surface() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let found = places_spelling(root, "src/tools", &["mcp", "commands", "cli"]);
        assert_eq!(found, Vec::<String>::new());
    }
}
