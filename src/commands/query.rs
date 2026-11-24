use std::path::Path;

use crate::console::{diag, out};
use crate::store::{self, Status, Store};
use crate::{Error, Result};

use super::open_store;

/// `trurlic status`: a one-screen summary of the graph: its components,
/// decisions (project-wide and proposed), edges, and how many issues and
/// non-canonical files it has.
pub(crate) fn status(cwd: &Path) -> Result<()> {
    let state = open_store(cwd)?;
    let project_wide = state.project_decisions().len();
    let proposed = state
        .decisions
        .values()
        .filter(|d| matches!(d.status, Status::Proposed { .. }))
        .count();
    let edges = state
        .components
        .values()
        .map(|c| c.connects_to.len())
        .chain(state.decisions.values().map(|d| d.depends_on.len()))
        .sum::<usize>();

    out!(
        "project: {} ({})",
        state.project.name,
        state.project.placement.as_str()
    )?;
    out!("components: {}", state.components.len())?;
    out!(
        "decisions: {} ({project_wide} project-wide, {proposed} proposed)",
        state.decisions.len()
    )?;
    out!("edges: {edges}")?;
    let issues = state.validate().len();
    if issues > 0 {
        out!("issues: {issues}")?;
    }
    if !state.non_canonical.is_empty() {
        out!("not in canonical form: {}", state.non_canonical.len())?;
    }
    Ok(())
}

/// `trurlic query file <path>`: every decision anchored on `path`, on a
/// file under it as a directory, or on a directory that holds it, with its
/// status and the anchors that matched. The path is normalized and refused when it could
/// leave the code root, before the graph is read.
pub(crate) fn query_file(cwd: &Path, path: &str) -> Result<()> {
    let normalized = store::normalize_path(path).map_err(|fault| Error::InvalidPath {
        path: path.to_owned(),
        fault,
    })?;
    let state = open_store(cwd)?;
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

/// `trurlic check`: every issue of the graph as an error, every file not in
/// canonical form as a warning. Fails when the graph has an issue; a file
/// that only needs `trurlic fmt` does not fail it, since a hand edit is a
/// legal write.
pub(crate) fn check(cwd: &Path) -> Result<()> {
    let state = Store::discover(cwd)?.load_state()?;
    let issues = state.validate();
    for issue in &issues {
        diag!("  error: {}", issue.message);
    }
    for file in &state.non_canonical {
        diag!("  warning: {file} is not in canonical form; run `trurlic fmt`");
    }
    if !issues.is_empty() {
        return Err(Error::CheckFailed(issues.len()));
    }
    if state.non_canonical.is_empty() {
        out!(".trurlic/ is consistent")?;
    }
    Ok(())
}
