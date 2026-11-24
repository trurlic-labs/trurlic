//! Findings over the graph that refuse nothing: what a reader should look
//! at. Pure: each part reads a [`ProjectState`](crate::store::ProjectState)
//! and the facts handed to it, and touches no file and no clock.

pub(crate) mod duplicates;
pub(crate) mod quality;
