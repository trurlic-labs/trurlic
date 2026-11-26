//! The repairs of the stale anchors `check` reports: what the calling
//! branch's history says of their files ([`History`](renames::History))
//! and the blobs a move is judged by, each read in one bounded read and
//! handed to `lint` as data (`moves.rs`). Git is asked nothing when no
//! stale anchor's file is gone or defines nothing its symbol names, and
//! nothing with no `.git` at or above the code root.
//!
//! A call runs at most six git processes for them, whatever the history:
//! the walk, the diff of what is not committed, and two for each of the two
//! reads of blobs, the anchors' files before their newest changes, then
//! the files each loss changed around it and since. The blobs are indexed
//! through the index's cache, so a warm call indexes none of them again.
//!
//! The write holds what the read judged to the tree again under its lock
//! ([`still_holding`]), through the same `Reads` and with no git but
//! `HEAD` (`recheck.rs`).

mod moves;
mod recheck;
mod renames;

pub(super) use moves::with_history;
pub(crate) use recheck::{Seen, still_holding};
