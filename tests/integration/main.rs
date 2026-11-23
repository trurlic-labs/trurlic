//! Integration tests that drive the built `trurlic` binary.
//!
//! One test crate, so the binary and the harness link once. Modules that
//! need the `failpoints` feature compile only when it is enabled.

mod budget;
mod decision_text;
mod determinism;
#[cfg(feature = "failpoints")]
mod failpoints;
mod golden;
mod harness;
#[cfg(unix)]
mod install;
mod invalid_graph;
mod malformed_node;
mod output;
mod stdio;
#[cfg(feature = "failpoints")]
mod watchers;
mod writers;
