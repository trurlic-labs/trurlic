//! The placement the step to format 0.5 writes, told by the owner or read
//! from the files the project's git tracks.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use crate::{Error, Result};

use super::{File, PlacementBasis};
use crate::store::format::Store;
use crate::store::graph::{listed, partition};
use crate::store::model::Placement;

/// How the step to format 0.5 is told the placement it writes.
#[derive(Debug)]
pub(crate) enum PlacementSource {
    /// The owner stated it: `trurlic migrate --placement`.
    Stated(Placement),
    /// The paths under the store directory, relative to it, that the
    /// project's git tracks, or why git could not list them.
    Git(Result<BTreeSet<PathBuf>>),
    /// No git work tree holds the store directory.
    NoWorkTree,
}

/// Why git does not tell the placement of a store.
#[derive(Debug)]
pub enum PlacementDoubt {
    /// Git tracks some of the store's files and not the others; each list
    /// is sorted.
    Mixed {
        tracked: Vec<String>,
        untracked: Vec<String>,
    },
    /// No git work tree holds the store directory.
    NoWorkTree,
    /// Git could not list what it tracks.
    Git(Box<Error>),
}

impl fmt::Display for PlacementDoubt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mixed { tracked, untracked } => {
                let total = tracked.len() + untracked.len();
                let (side, files) = if tracked.len() <= untracked.len() {
                    ("tracked", tracked)
                } else {
                    ("not tracked", untracked)
                };
                write!(
                    f,
                    "git tracks {} of its {total} files; {side}: {}",
                    tracked.len(),
                    listed(files)
                )
            }
            Self::NoWorkTree => f.write_str("no git work tree holds it"),
            Self::Git(error) => write!(f, "{error}"),
        }
    }
}

impl Store {
    /// The placement `source` gives a store of `files`: git tracks all of
    /// them, or none, or the owner stated it.
    pub(super) fn placement(
        &self,
        source: PlacementSource,
        files: &[File],
    ) -> Result<(Placement, PlacementBasis)> {
        let tracked = match source {
            PlacementSource::Stated(placement) => return Ok((placement, PlacementBasis::Stated)),
            PlacementSource::NoWorkTree => return Err(self.doubt(PlacementDoubt::NoWorkTree)),
            PlacementSource::Git(tracked) => {
                tracked.map_err(|e| self.doubt(PlacementDoubt::Git(Box::new(e))))?
            }
        };
        let files: Vec<String> = files.iter().map(ToString::to_string).collect();
        let (tracked, untracked) = partition(files, &tracked);
        match (tracked.len(), untracked.len()) {
            (files, 0) => Ok((Placement::Tracked, PlacementBasis::AllTracked { files })),
            (0, files) => Ok((Placement::Shared, PlacementBasis::NoneTracked { files })),
            _ => Err(self.doubt(PlacementDoubt::Mixed { tracked, untracked })),
        }
    }

    fn doubt(&self, doubt: PlacementDoubt) -> Error {
        Error::PlacementUnknown {
            // clone: the error owns the path it names.
            store: self.root.clone(),
            doubt,
        }
    }
}
