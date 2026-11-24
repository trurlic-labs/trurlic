//! The placement the step to format 0.5 writes, told by the owner or read
//! from the files the project's git tracks.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

use super::super::Store;
use super::super::schema::Placement;
use super::{File, PlacementBasis};

/// How the step to format 0.5 is told the placement it writes.
#[derive(Debug)]
pub(crate) enum PlacementSource {
    /// The owner stated it: `trurlic migrate --placement`.
    Stated(Placement),
    /// The paths under the store directory, relative to it, that the
    /// project's git tracks, or why git could not list them.
    Git(Result<BTreeSet<PathBuf>>),
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
    /// Git could not list what it tracks.
    Git(Box<Error>),
}

/// The names a mixed placement shows of its smaller side.
const SHOWN_FILES: usize = 5;

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
                    "git tracks {} of its {total} files; {side}: ",
                    tracked.len()
                )?;
                let shown: Vec<&str> = files.iter().take(SHOWN_FILES).map(String::as_str).collect();
                f.write_str(&shown.join(", "))?;
                match files.len().saturating_sub(SHOWN_FILES) {
                    0 => Ok(()),
                    more => write!(f, " and {more} more"),
                }
            }
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
            PlacementSource::Git(tracked) => {
                tracked.map_err(|e| self.doubt(PlacementDoubt::Git(Box::new(e))))?
            }
        };
        let (tracked, untracked): (Vec<String>, Vec<String>) = files
            .iter()
            .map(ToString::to_string)
            .partition(|file| tracked.contains(Path::new(file)));
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
