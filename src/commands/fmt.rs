use std::path::Path;

use crate::Result;
use crate::console::out;
use crate::discovery::discover;

use super::graph_name;

/// `trurlic fmt`: rewrite every file of the graph that is not in canonical
/// form, in one commit, and name each one. A file already canonical is not
/// touched, so a second run writes nothing.
pub(crate) fn fmt(cwd: &Path) -> Result<()> {
    let store = discover(cwd)?;
    let formatted = store.begin_write()?.format()?;
    if formatted.is_empty() {
        out!("{} is in canonical form", graph_name(&store))?;
        return Ok(());
    }
    for file in &formatted {
        out!("formatted {file}")?;
    }
    Ok(())
}
