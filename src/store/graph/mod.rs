//! What a loaded graph holds beyond its files: the issues validation
//! reports, the cycles of its edges, the reads the tools make of it, and
//! whether git agrees with its placement. Every function here takes a
//! [`ProjectState`](crate::store::ProjectState) or facts handed in, and
//! reads no file and runs no process.

mod cycles;
mod placement;
mod query;
mod validate;

pub(in crate::store) use cycles::cycles;
pub use placement::{LinkedGraph, MainWorktree, PlacementFacts, WorkTreeFacts};
pub(in crate::store) use placement::{listed, partition};
pub use query::{Named, anchors_in, normalize_path};
pub use validate::{Issue, IssueKind};
pub(in crate::store) use validate::{carry_members, introduced};
