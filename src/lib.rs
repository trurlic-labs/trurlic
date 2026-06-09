#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;

pub(crate) mod commands;
pub(crate) mod config;
pub(crate) mod error;
pub(crate) mod map;
pub(crate) mod mcp;
pub(crate) mod provider;
pub(crate) mod session;
pub(crate) mod store;
pub(crate) mod workflow;

pub use error::{Error, Result};
