//! The crates of a tree: which directory each file's crate is.
//!
//! A file's crate is the nearest directory above it with a `Cargo.toml`,
//! or the file alone when there is none. A walked file's crate is read
//! from the walked manifests alone. A file an anchor brought from outside
//! the walk is read from those and the manifests the anchors brought. A
//! manifest an anchor brings thus governs no file the walk took, so what
//! one anchor resolves to never depends on the others.

use std::collections::{BTreeMap, BTreeSet};

use super::index::{FileIndex, FileKind};

/// The manifest that makes a directory a crate.
pub(crate) const MANIFEST: &str = "Cargo.toml";

/// The files of a tree: a path to its kind and index.
pub(super) type Files<'i> = BTreeMap<&'i str, (FileKind, &'i FileIndex)>;

/// The crate directories of a set of manifests, sorted.
struct Layout<'i> {
    crate_roots: Vec<&'i str>,
}

impl<'i> Layout<'i> {
    fn of(manifests: impl Iterator<Item = &'i str>) -> Self {
        // A path sorts before its siblings that extend its name (`a` before
        // `a-b`), but `a/b` after `a-b`: the parents need their own sort.
        let mut crate_roots: Vec<&str> = manifests.map(parent).collect();
        crate_roots.sort_unstable();
        crate_roots.dedup();
        Self { crate_roots }
    }

    /// The crate directory of `path`, or `path` itself outside every crate.
    fn crate_of<'p>(&self, path: &'p str) -> &'p str {
        ancestors(path)
            .find(|dir| self.crate_roots.binary_search(dir).is_ok())
            .unwrap_or(path)
    }
}

/// The crates of the tree.
pub(super) struct Crates<'i> {
    /// What the walked manifests say: what governs a walked file.
    walked: Layout<'i>,
    /// What those and the manifests an anchor brought say: what governs a
    /// file the walk did not take.
    all: Layout<'i>,
    /// The files the walk did not take.
    outside: BTreeSet<&'i str>,
}

impl<'i> Crates<'i> {
    /// The crates of `files`, those the walk took, and of `outside`, the
    /// files an anchor brought.
    pub(super) fn of(files: &Files<'i>, outside: &Files<'i>) -> Self {
        let manifests = |set: &Files<'i>| -> Vec<&'i str> {
            set.keys()
                .copied()
                .filter(|path| file_name(path) == MANIFEST)
                .collect()
        };
        let walked = manifests(files);
        let brought = manifests(outside);
        Self {
            walked: Layout::of(walked.iter().copied()),
            all: Layout::of(walked.iter().chain(&brought).copied()),
            outside: outside.keys().copied().collect(),
        }
    }

    /// The crate directory of `path`, or `path` itself outside every crate.
    pub(super) fn crate_of<'p>(&self, path: &'p str) -> &'p str {
        match self.outside.contains(path) {
            true => self.all.crate_of(path),
            false => self.walked.crate_of(path),
        }
    }
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The directory of `path`; `""` for the code root.
pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The directories above `path`, nearest first, the code root (`""`)
/// last.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(path);
    std::iter::from_fn(move || {
        let current = rest?;
        rest = (!current.is_empty()).then(|| parent(current));
        rest
    })
}
