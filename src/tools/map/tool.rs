//! The `map` tool: its spec, its arguments, and the handler that reads a
//! level of the code tree and cuts the page asked for.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::sources;
use crate::store::{ProjectState, Store};
use crate::tools::result::ToolError;
use crate::tools::spec::kinds::Text;
use crate::tools::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use crate::tools::spec::{Args, Fields};

use super::page::{Page, page};

pub(crate) static MAP: ToolSpec<MapArgs, Page> = ToolSpec {
    name: "map",
    title: "Map",
    description: "One level of the code tree, paged.",
    destructive: Destructive::No,
    access: Access::Read(map),
};

/// The arguments of `map`, and the argument and flag of `trurlic map`.
#[derive(Debug)]
pub(crate) struct MapArgs {
    node: Option<String>,
    from: Option<String>,
}

/// The most bytes an address argument holds: an address a page lists
/// takes no more raw bytes than its JSON escape, which the page holds
/// within the result budget.
pub(super) const MAX_ADDRESS_BYTES: usize = MAX_TOOL_RESULT_BYTES;

impl Args for MapArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        let address = Text {
            max_bytes: MAX_ADDRESS_BYTES,
        };
        Self {
            node: fields.optional(
                "node",
                "The address of the node whose children to list, as a level gives it; the \
                 crates, and what lies outside every crate, without it.",
                &address,
            ),
            from: fields.optional(
                "from",
                "The address of the first child to list: a previous page's `next`.",
                &address,
            ),
        }
    }
}

/// The page of the level below `arguments.node` that starts at
/// `arguments.from`.
fn map(store: &Store, state: &ProjectState, arguments: MapArgs) -> Outcome<Page> {
    let MapArgs { node, from } = arguments;
    let decisions = state.decisions.values();
    let level = sources::level(store, decisions, node.as_deref()).map_err(|no| {
        let address = no.address;
        let message =
            format!("`{address}` is no node of the code tree: a level gives each node's address");
        ToolError::invalid("node", message)
    })?;
    page(level, from.as_deref())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::tools::spec::SpecTool;
    use crate::tools::{FRAME, SEARCH};

    /// The schemas a reader's types of the `map` tools' JSON are generated
    /// from, by tool, pinned in `tests/integration/golden/map_schemas.json`
    /// by the rule of `tests/integration/golden.rs`. It stands in the crate
    /// because no surface prints the schemas for an integration test to
    /// read.
    #[test]
    fn the_schemas_of_the_map_tools_match_golden() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/integration/golden/map_schemas.json");
        let tools: [&dyn SpecTool; 3] = [&MAP, &FRAME, &SEARCH];
        let schemas: serde_json::Map<String, serde_json::Value> = tools
            .into_iter()
            .map(|tool| {
                let schemas = json!({
                    "inputSchema": tool.input_schema(),
                    "outputSchema": tool.output_schema(),
                });
                (tool.name().to_owned(), schemas)
            })
            .collect();
        let actual = serde_json::to_string_pretty(&schemas).unwrap() + "\n";

        if std::env::var_os("TRURLIC_UPDATE_GOLDEN").is_some_and(|setting| setting == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "cannot read golden file {}: {e}\nrun with TRURLIC_UPDATE_GOLDEN=1 to create it",
                path.display()
            )
        });
        // A Windows checkout may convert line endings; the content is what counts.
        assert_eq!(expected.replace("\r\n", "\n"), actual, "{}", path.display());
    }
}
