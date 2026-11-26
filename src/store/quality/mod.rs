//! The quality bar a write holds a decision to: the built-in rules and the
//! project's `[quality]` table.
//!
//! Two kinds of rule govern a decision's text and shape. What a file must
//! hold to be read as written (`store::model::text`: a non-blank choice on one
//! line within 200 bytes, a reason of real text, no invisible, reordering
//! or control character, no tool-call markup) is a property of every graph:
//! validation reports each, so `check` fails on a hand edit that breaks
//! one. The bar is a property of a write: at least one alternative and one
//! anchor, and the project's limits and patterns. A graph migrated or edited
//! by hand below it loads, passes `check` and stays revisable, since an op
//! is held to the bar only in the fields it writes. A write holds each
//! decision an op writes to the text rules, the limits and the patterns as
//! the op runs, in one order ([`Bar::judge`]).
//!
//! Words are counted one way everywhere: the runs of non-whitespace
//! between Unicode whitespace (`bar.rs`, `words`). Bytes are bytes of UTF-8.

mod audit;
mod bar;
mod folding;
mod pattern;
mod verdict;

pub(crate) use audit::Shortfall;
pub(crate) use bar::{Bar, PreparedBar};
pub(in crate::store) use bar::{element, texts};
pub(crate) use pattern::Scan;
#[cfg(test)]
pub(crate) use verdict::PatternMatch;
pub(crate) use verdict::QualityFault;
pub(in crate::store) use verdict::{Breach, Broken, Hit, Place};
