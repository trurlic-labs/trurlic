//! The `lint` section of `status`: what to read that refuses nothing, of
//! three kinds, each gathered to the most a result can show and counted.
//! Likely duplicate pairs come best first; quality findings, each rule of
//! the bar a stored decision breaks, and the decisions with only file
//! anchors come by decision name. The section lists them in that order of
//! kinds, each kind fitted to the budget on its own. A finding is a
//! [`finding`]; the section is gathered and fitted in [`gather`]. The
//! pairs are compared ([`pairs`]) and the `[quality]` patterns judged
//! ([`patterns`]) only for the section asked for alone, the pairs only
//! without `from`.

mod finding;
mod gather;
mod pairs;
mod patterns;

pub(crate) use finding::LintItem;
pub(super) use finding::LintKind;
pub(super) use gather::{LintCounts, LintItems, fitted, gather};
pub(crate) use pairs::{PAIRS_MORE, PairCounts};
pub(super) use patterns::Patterns;
pub(crate) use patterns::{Cut, PatternCounts};
