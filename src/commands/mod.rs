mod apply;
mod check;
mod fmt;
mod init;
mod install;
mod map;
mod migrate;
mod placement;
mod query;
mod serve;
mod status;

pub(crate) use apply::apply;
pub(crate) use check::check;
pub(crate) use fmt::fmt;
pub(crate) use init::init;
pub(crate) use install::install;
pub(crate) use map::{MapUnit, map};
pub(crate) use migrate::migrate;
pub(crate) use query::query_file;
pub(crate) use serve::serve;
pub(crate) use status::{StatusFlags, status};

use std::path::Path;

use crate::Result;
use crate::console::{diag, out};
use crate::discovery::discover;

use crate::store::{ProjectState, STORE_DIR, Store};
use crate::tools::{Output, ToolError, ToolResult, to_value};

/// How a tool's CLI twin prints its result: for a terminal, or as the
/// JSON the tool's call returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Json,
}

/// What a tool's CLI twin prints of `outcome`, the tool's result or its
/// refusal: `print` for a terminal, the text the call returns under
/// `--json`, its refusal included. A refusal fails the command either way,
/// and `cli` prints its message on stderr.
fn answer<O: Output>(
    outcome: std::result::Result<O, ToolError>,
    format: Format,
    print: impl FnOnce(&O) -> Result<()>,
) -> std::result::Result<(), ToolError> {
    match format {
        Format::Json => json(outcome),
        Format::Text => Ok(print(&outcome?)?),
    }
}

/// The text a tool's call returns of `outcome`, its result or its
/// refusal, printed: what a command prints under `--json`. A refusal fails
/// the command, and `cli` prints its message on stderr.
fn json<O: Output>(
    outcome: std::result::Result<O, ToolError>,
) -> std::result::Result<(), ToolError> {
    match outcome {
        Ok(result) => Ok(out!(
            "{}",
            ToolResult::structured(to_value(&result)).text()
        )?),
        Err(refusal) => {
            out!("{}", ToolResult::refusal(&refusal).text())?;
            Err(refusal)
        }
    }
}

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DryRun {
    Yes,
    No,
}

/// Load the graph for a read-only command and warn when it has issues. The
/// load waits only while a writer holds the lock.
fn open_store(cwd: &Path) -> Result<(Store, ProjectState)> {
    let store = discover(cwd)?;
    let state = store.load_state()?;
    let issues = state.validate().len();
    if issues > 0 {
        diag!(
            "warning: {} has {issues} issue(s); run `trurlic check` for details",
            graph_name(&store)
        );
    }
    Ok((store, state))
}

/// The graph directory as a message names it: `.trurlic/` when it is the
/// code root's own, else its path, since a tree that reaches the graph
/// through a symlink or a linked worktree holds no such directory.
fn graph_name(store: &Store) -> String {
    if store.is_local() {
        format!("{STORE_DIR}/")
    } else {
        format!("{}/", store.root().display())
    }
}
