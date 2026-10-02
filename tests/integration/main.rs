//! Integration tests that drive the built `trurlic` binary.
//!
//! One test crate, so the binary and the harness link once. Modules that
//! need the `failpoints` feature compile only when it is enabled.

#[cfg(feature = "failpoints")]
mod failpoints;
mod golden;
mod harness;
mod stdio;
