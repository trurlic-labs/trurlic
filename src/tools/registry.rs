//! Every tool, in the order a surface lists them: the tools built on a
//! spec, then the reads that predate specs. A surface looks a call up here
//! by name and calls the tool it finds; a name not here is no tool.

use std::path::Path;

use serde_json::{Map, Value};

use super::apply::APPLY;
use super::legacy::Read;
use super::result::ToolResult;
use super::spec::SpecTool;
use super::status::STATUS;

/// Bytes of the request one tool call arrives in, on every surface: a
/// message the server reads, a batch document `trurlic apply` reads.
pub(crate) const MAX_REQUEST_BYTES: usize = 10 * 1024 * 1024;

/// A tool a surface lists and calls.
#[derive(Clone, Copy)]
pub(crate) enum Tool {
    /// Built on a spec: its schemas and its parse walk one declaration.
    Spec(&'static dyn SpecTool),
    /// A read that predates specs, its schema written out by hand.
    Legacy(Read),
}

/// Every tool, spec-built first.
pub(crate) static TOOLS: [Tool; 4] = [
    Tool::Spec(&APPLY),
    Tool::Spec(&STATUS),
    Tool::Legacy(Read::Context),
    Tool::Legacy(Read::DecisionsForFile),
];

impl Tool {
    /// The tool called `name`, if there is one.
    pub(crate) fn named(name: &str) -> Option<Self> {
        TOOLS.iter().copied().find(|tool| tool.name() == name)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Spec(tool) => tool.name(),
            Self::Legacy(read) => read.name(),
        }
    }

    /// Call the tool with `arguments` on the store found from `cwd`: its
    /// result, or its refusal, no store there included.
    pub(crate) fn call(self, cwd: &Path, arguments: &Map<String, Value>) -> ToolResult {
        match self {
            Self::Spec(tool) => tool.call(cwd, arguments).map_or_else(
                |refused| ToolResult::refusal(&refused),
                ToolResult::structured,
            ),
            Self::Legacy(read) => read.call(cwd, arguments),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_is_found_by_its_name_alone() {
        for tool in TOOLS {
            let found = Tool::named(tool.name()).map(Tool::name);
            assert_eq!(found, Some(tool.name()));
        }
        for gone in ["record_decision", "check_pattern", "get_architecture", ""] {
            assert!(Tool::named(gone).is_none(), "{gone}");
        }
    }
}
