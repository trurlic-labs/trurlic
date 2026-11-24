use std::path::Path;

use crate::Result;

/// `trurlic serve`: the MCP server on stdio. Each tool call finds the store
/// from `cwd` and loads it, so the server starts without one, and a call
/// made with none, or with one in another format, is refused naming
/// `trurlic init` or `trurlic migrate`.
pub(crate) fn serve(cwd: &Path) -> Result<()> {
    crate::mcp::run_server(cwd.to_path_buf())
}
