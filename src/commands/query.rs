use std::path::Path;

use crate::console::out;
use crate::store::{self, Status};
use crate::{Error, Result};

use super::open_store;

/// `trurlic query file <path>`: every decision anchored on `path`, on a
/// file under it as a directory, or on a directory that holds it, with its
/// status and the anchors that matched. The path is normalized and refused when it could
/// leave the code root, before the graph is read.
pub(crate) fn query_file(cwd: &Path, path: &str) -> Result<()> {
    let normalized = store::normalize_path(path).map_err(|fault| Error::InvalidPath {
        path: path.to_owned(),
        fault,
    })?;
    let (_, state) = open_store(cwd)?;
    let matches = state.decisions_in(&normalized);
    if matches.is_empty() {
        out!("No decision is anchored on `{normalized}`.")?;
        return Ok(());
    }

    out!("{} decision(s) constrain `{normalized}`:\n", matches.len())?;
    for (name, decision) in &matches {
        let proposed = match decision.status {
            Status::Proposed { .. } => " (proposed)",
            Status::Accepted { .. } => "",
        };
        out!("  [{}] {name}{proposed}", decision.component)?;
        out!("    {}", decision.choice)?;
        let anchors: Vec<String> = store::anchors_in(decision, &normalized)
            .into_iter()
            .map(ToString::to_string)
            .collect();
        out!("    anchors: {}", anchors.join(", "))?;
        out!()?;
    }
    Ok(())
}
