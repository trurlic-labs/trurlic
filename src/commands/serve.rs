use std::path::Path;

use crate::Result;
use crate::store::Store;

/// `trurlic serve`: the MCP server on stdio, for the store above `cwd`.
/// Each call loads the graph from disk, so the server keeps none between
/// calls; a store in another format is refused before it starts.
pub fn serve(cwd: &Path) -> Result<()> {
    let store = Store::discover(cwd)?;
    store.check_version()?;
    crate::mcp::run_server(store)
}
