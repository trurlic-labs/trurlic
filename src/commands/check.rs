use std::path::Path;

use crate::console::{diag, out};
use crate::discovery::discover;
use crate::store::HistoryFinding;
use crate::{Error, Result};

use super::{graph_name, placement};

/// `trurlic check`: every issue of the graph and of its placement as an
/// error, every file not in canonical form as a warning. Fails when there
/// is an issue; a file that only needs `trurlic fmt` does not fail it,
/// since a hand edit is a legal write.
///
/// The placement is held to what git says of the directory that holds the
/// graph: a tracked graph is in a work tree whose git tracks it and
/// ignores none of its files; a shared one is tracked by no git, and in a
/// work tree it is the main worktree's graph, beside which no worktree
/// carries another. A shared graph's history is reported too: a leftover
/// index lock, which refuses every write, fails it; graph files not
/// committed, or no repository, are warnings the next write mends.
pub(crate) fn check(cwd: &Path) -> Result<()> {
    let store = discover(cwd)?;
    let state = store.load_state()?;
    let mut issues = state.validate();
    let facts = placement::facts(&store)?;
    issues.extend(state.placement_issues(store.root(), &facts));
    let (failing, mended): (Vec<_>, Vec<_>) = store
        .history_findings(state.project.placement)?
        .into_iter()
        .partition(HistoryFinding::is_error);
    for issue in &issues {
        diag!("  error: {}", issue.message);
    }
    for finding in &failing {
        diag!("  error: {finding}");
    }
    for file in &state.non_canonical {
        diag!("  warning: {file} is not in canonical form; run `trurlic fmt`");
    }
    for finding in &mended {
        diag!("  warning: {finding}");
    }
    let errors = issues.len() + failing.len();
    if errors > 0 {
        return Err(Error::CheckFailed(errors));
    }
    if state.non_canonical.is_empty() && mended.is_empty() {
        out!("{} is consistent", graph_name(&store))?;
    }
    Ok(())
}
