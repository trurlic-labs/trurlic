#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
//! Trurl — structured architectural decisions that constrain AI code generation.
//!
//! Captures architectural decisions in a `.trurl/` directory (TOML, git-tracked),
//! serves them to AI coding agents via MCP, and provides an interactive map
//! for visual exploration.

pub mod cli;

pub(crate) mod commands;
pub(crate) mod config;
pub(crate) mod conversation;
pub(crate) mod error;
pub(crate) mod provider;
pub(crate) mod store;

pub use error::{Error, Result};
