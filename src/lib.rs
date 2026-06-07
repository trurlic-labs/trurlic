#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;

pub(crate) mod commands;
pub(crate) mod config;
pub(crate) mod conversation;
pub(crate) mod error;
pub(crate) mod mcp;
pub(crate) mod provider;
pub(crate) mod store;

pub use error::{Error, Result};
