//! The loaded graph and its judgements: [`ProjectState`], the node files
//! one load read, with [`NodeFile`], the name of one of them; the issues
//! validation reports, the cycles of its edges, the reads the tools make
//! of it, and whether git agrees with its placement. Every function here
//! takes a state or facts handed in, and reads no file and runs no
//! process.

mod cycles;
mod placement;
mod query;
mod state;
mod validate;

pub(in crate::store) use cycles::cycles;
pub(crate) use placement::{LinkedGraph, MainWorktree, PlacementFacts, WorkTreeFacts};
pub(in crate::store) use placement::{listed, partition};
pub(crate) use query::{Named, anchors_in, normalize_path};
pub(crate) use state::NodeFile;
pub use state::ProjectState;
pub(crate) use validate::{Issue, IssueKind};
pub(in crate::store) use validate::{carry_members, introduced};
