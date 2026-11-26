//! The `status` tool: the graph at a glance, as counts and sections, each
//! section fitted to the result budget. A section that leaves items out
//! says how many and where the rest is; `section` gives one section the
//! whole budget.
//!
//! It reports what the graph's files and, under `shared`, the graph's own
//! history hold. Where the graph sits in the project's git is `check`'s:
//! asking it costs several git processes, too many for a call made often.
//!
//! The tool's spec and handler are in [`tool`], the sections a call can
//! ask for in [`section`]. Each section's items, and how they are drawn
//! from the graph, are a file of their own: [`issues`], [`history`],
//! [`components`], [`lint`], [`anchors`]. The report that holds them is
//! [`report`], its counts [`counts`]; a section's items before and after
//! the budget cuts them are [`part`], and the cut itself is [`fit`].

mod anchors;
mod components;
mod counts;
mod fit;
mod history;
mod issues;
mod lint;
mod part;
mod report;
mod section;
mod tool;

pub(crate) use anchors::AnchorCounts;
pub(crate) use counts::QualityCounts;
pub(crate) use lint::{Cut, LintItem, PAIRS_MORE, PairCounts, PatternCounts};
pub(crate) use part::Part;
pub(crate) use report::Report;
pub(crate) use section::section_names;
pub(crate) use tool::STATUS;
