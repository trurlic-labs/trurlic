//! The graph as files: where each one lives ([`Store`]), the one canonical
//! text each is written in, how each is read as its type, the check of the
//! format `project.toml` states, and the registry of steps from an older
//! format. Nothing here takes the lock: whoever reads or writes through
//! this module holds it.

mod canonical;
mod check;
mod layout;
mod load;
pub(in crate::store) mod migration;

#[cfg(test)]
pub(in crate::store) use canonical::is_canonical;
pub(in crate::store) use canonical::{Canonical, canonical};
pub(in crate::store) use check::{Standing, read_table, standing};
pub use layout::Store;
pub(crate) use layout::StorePathFault;
#[cfg(test)]
pub(in crate::store) use load::list_toml_stems;
pub(in crate::store) use load::{ensure_regular, read_dir_if_present};
pub(crate) use migration::PlacementDoubt;
pub(crate) use migration::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
