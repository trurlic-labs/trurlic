//! Integration tests that drive the built `trurlic` binary.
//!
//! One test crate, so the binary and the harness link once. Modules that
//! need the `failpoints` feature compile only when it is enabled.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a harness step that fails is a failed test; clippy exempts only #[test] bodies"
)]

mod budget;
mod determinism;
#[cfg(feature = "failpoints")]
mod failpoints;
mod golden;
mod harness;
mod invalid_graph;
mod malformed_node;
mod output;
mod stdio;
#[cfg(feature = "failpoints")]
mod watchers;
mod writers;
