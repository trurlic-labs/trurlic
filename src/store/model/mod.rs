//! The graph's values: the types its files are read as, the rules every
//! text and name in them meets, the limits every file and value is held
//! to, and the state one load reads. Nothing here touches disk, runs a
//! process or reads the clock but [`Timestamp::now`], so `code`, `lint` and
//! `error` name the store's types from here alone, and every other part of
//! the store depends on this one, never the other way.

pub(crate) mod limits;
mod name;
mod quality;
mod schema;
mod state;
mod text;
mod time;
mod version;

pub(crate) use name::NameFault;
pub(in crate::store) use name::name_fault;
pub(crate) use quality::{DecisionField, Level, Pattern, Quality, WordRange};
pub(crate) use schema::{
    AcceptedText, AcceptedVia, Anchor, AnchorFault, COMPONENTS_DIR, Component, DECISIONS_DIR,
    Decision, PROJECT_COMPONENT, PROJECT_FILE, Project, STATE_DIR, STORE_DIR, Status, StatusName,
};
pub use schema::{FORMAT_VERSION, Placement};
pub(crate) use state::NodeFile;
pub use state::ProjectState;
pub(in crate::store) use text::fault;
pub(crate) use text::{TextFault, TextField};
pub(crate) use time::{Timestamp, TimestampError};
pub(crate) use version::FormatVersion;
