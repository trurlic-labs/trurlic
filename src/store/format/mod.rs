//! The graph as files: where each one lives ([`Store`]), the one canonical
//! text each is written in, how each is read as its type, where the format
//! `project.toml` states stands against the current one, and the registry
//! of steps from an older format. Nothing here takes the lock. Whoever
//! reads or writes through this module holds it, but for
//! [`Store::stated_placement`], which reads `project.toml` without it: a
//! commit replaces the file by a rename, so the read sees it whole.

mod canonical;
mod layout;
mod load;
pub(in crate::store) mod migration;
mod standing;

#[cfg(test)]
pub(in crate::store) use canonical::is_canonical;
pub(in crate::store) use canonical::{Canonical, canonical};
pub use layout::Store;
pub(crate) use layout::StorePathFault;
#[cfg(test)]
pub(in crate::store) use load::list_toml_stems;
pub(in crate::store) use load::{ensure_regular, read_dir_if_present, read_table};
pub(crate) use migration::PlacementDoubt;
pub(crate) use migration::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
pub(in crate::store) use standing::{Standing, standing};
