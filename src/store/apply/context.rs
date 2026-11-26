//! The state the ops of one batch share while they run ([`Context`]).

use crate::Result;

use super::applied::{LeftRepair, Repaired};
use super::warning::Warning;
use crate::store::model::Timestamp;
use crate::store::quality::{Bar, Hit, Scan};

/// What the ops of one batch share: what every record carries, the bar
/// each written decision meets and what its patterns may still read, the
/// warnings, and what the repairs did.
pub(super) struct Context<'b> {
    pub(super) origin: Option<&'b str>,
    pub(super) created: Timestamp,
    pub(super) bar: &'b Bar,
    pub(super) scan: Scan,
    /// The `warn` matches of the op running now, each with the argument
    /// it is in.
    pub(super) hits: Vec<(String, Hit)>,
    /// The batch's warnings so far.
    pub(super) warnings: Vec<Warning>,
    /// The anchors the batch's repairs replaced so far.
    pub(super) repaired: Vec<Repaired>,
    /// The repairs of the map the bar did not admit, so far.
    pub(super) left: Vec<LeftRepair>,
}

impl<'b> Context<'b> {
    /// A batch's context before its first op: its `origin`, the instant
    /// now, the `bar` and its `scan`, and the origin's `hits`.
    pub(super) fn new(
        origin: Option<&'b str>,
        bar: &'b Bar,
        scan: Scan,
        hits: Vec<(String, Hit)>,
    ) -> Result<Self> {
        Ok(Self {
            origin,
            created: Timestamp::now()?,
            bar,
            scan,
            hits,
            warnings: Vec::new(),
            repaired: Vec::new(),
            left: Vec::new(),
        })
    }
}
