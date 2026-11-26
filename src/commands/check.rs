use std::path::Path;

use crate::console::{diag, out};
use crate::discovery::discover;
use crate::lint::anchors::Severity;
use crate::sources;
use crate::store::{HistoryFinding, ProjectState, Store};
use crate::{Error, Result};

use super::{graph_name, placement};

/// `trurlic check`: every issue of the graph and of its placement as an
/// error, and a `[quality]` pattern that does not compile, which would
/// refuse every write; every file not in canonical form as a warning; and
/// the anchor findings, with the other worktrees asked: a stale, malformed
/// or ambiguous anchor as an error, an unverified or weak one, and another
/// worktree that went unread, as a warning, a pending or planned one as a
/// note. Fails when there is an error; a file that only needs `trurlic
/// fmt` does not fail it, since a hand edit is a legal write. It says the graph is
/// consistent only when it printed nothing.
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
    let mut lines = Lines::default();
    graph_findings(&store, &state, &mut lines)?;
    anchor_findings(&store, &state, &mut lines);
    lines.print();
    if !lines.errors.is_empty() {
        return Err(Error::CheckFailed(lines.errors.len()));
    }
    if lines.is_empty() {
        out!("{} is consistent", graph_name(&store))?;
    }
    Ok(())
}

/// What `check` prints, by severity, each in the order it was found.
#[derive(Debug, Default)]
struct Lines {
    errors: Vec<String>,
    warnings: Vec<String>,
    notes: Vec<String>,
}

impl Lines {
    fn push(&mut self, severity: Severity, line: String) {
        match severity {
            Severity::Error => self.errors.push(line),
            Severity::Warning => self.warnings.push(line),
            Severity::Note => self.notes.push(line),
        }
    }

    fn is_empty(&self) -> bool {
        self.errors.is_empty() && self.warnings.is_empty() && self.notes.is_empty()
    }

    /// Errors first, then warnings, then notes.
    fn print(&self) {
        let labelled = [
            ("error", &self.errors),
            ("warning", &self.warnings),
            ("note", &self.notes),
        ];
        for (label, lines) in labelled {
            for line in lines {
                diag!("  {label}: {line}");
            }
        }
    }
}

/// The graph's issues, its placement's, a pattern that does not compile,
/// the files not in canonical form and what a shared graph's history
/// lacks.
fn graph_findings(store: &Store, state: &ProjectState, lines: &mut Lines) -> Result<()> {
    let facts = placement::facts(store)?;
    let issues = state.validate();
    let placed = state.placement_issues(store.root(), &facts);
    for issue in issues.into_iter().chain(placed) {
        lines.push(Severity::Error, issue.message);
    }
    if let Err(error) = store.bar(&state.project.quality) {
        lines.push(Severity::Error, error.to_string());
    }
    let findings = store.history_findings(state.project.placement)?;
    let (failing, mended): (Vec<_>, Vec<_>) =
        findings.into_iter().partition(HistoryFinding::is_error);
    for finding in failing {
        lines.push(Severity::Error, finding.to_string());
    }
    for file in &state.non_canonical {
        let line = format!("{file} is not in canonical form; run `trurlic fmt`");
        lines.push(Severity::Warning, line);
    }
    for finding in mended {
        lines.push(Severity::Warning, finding.to_string());
    }
    Ok(())
}

/// Every anchor that does not plainly resolve in the calling tree, and,
/// as a warning, each other worktree that went unread, where an anchor
/// stale here may be pending.
fn anchor_findings(store: &Store, state: &ProjectState, lines: &mut Lines) {
    let found = sources::checked_findings(store, &state.decisions);
    for finding in found.findings {
        lines.push(finding.kind.severity(), finding.to_string());
    }
    for unread in found.unread {
        lines.push(Severity::Warning, unread.to_string());
    }
}
