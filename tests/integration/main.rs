//! Integration tests that drive the built `trurlic` binary.
//!
//! One test crate, so the binary and the harness link once. Modules that
//! need the `failpoints` feature compile only when it is enabled.

mod apply;
mod budget;
mod canonical_form;
mod decision_text;
mod determinism;
#[cfg(feature = "failpoints")]
mod failpoints;
mod git_merge;
mod golden;
mod harness;
mod history;
#[cfg(unix)]
mod install;
mod invalid_graph;
mod lint;
mod malformed_node;
mod migrate;
mod output;
#[cfg(unix)]
mod placement;
#[cfg(feature = "failpoints")]
mod server_failpoints;
mod stdio;
