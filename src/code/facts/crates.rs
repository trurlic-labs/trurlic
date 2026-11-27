//! The crates of a tree: which directory each file's crate is.
//!
//! A file's crate is the nearest directory above it with a `Cargo.toml`,
//! or the file alone when there is none. A walked file's crate is read
//! from the walked manifests alone. A file an anchor brought from outside
//! the walk is read from those and the manifests the anchors brought. A
//! manifest an anchor brings thus governs no file the walk took, so what
//! one anchor resolves to never depends on the others.

use std::collections::{BTreeMap, BTreeSet};

use super::path::{ancestors, file_name, parent};
use crate::code::index::{FileIndex, FileKind};

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

    /// The crate directory of `path`; `None` outside every crate.
    fn crate_of<'p>(&self, path: &'p str) -> Option<&'p str> {
        ancestors(path).find(|dir| self.crate_roots.binary_search(dir).is_ok())
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
    /// The crates of `walked`, the paths the walk took, and of `outside`,
    /// those an anchor brought. A manifest makes its directory a crate
    /// whether it has an index or not, so a syntax error mid-edit moves no
    /// file out of its crate.
    pub(super) fn of(
        walked: impl IntoIterator<Item = &'i str>,
        outside: impl IntoIterator<Item = &'i str>,
    ) -> Self {
        let is_manifest = |path: &&str| file_name(path) == MANIFEST;
        let outside: BTreeSet<&str> = outside.into_iter().collect();
        let walked: Vec<&str> = walked.into_iter().filter(is_manifest).collect();
        let brought = outside.iter().copied().filter(is_manifest);
        let all = Layout::of(walked.iter().copied().chain(brought));
        Self {
            walked: Layout::of(walked.into_iter()),
            all,
            outside,
        }
    }

    /// The crate directory of `path`; `None` outside every crate.
    pub(super) fn crate_of<'p>(&self, path: &'p str) -> Option<&'p str> {
        match self.outside.contains(path) {
            true => self.all.crate_of(path),
            false => self.walked.crate_of(path),
        }
    }
}
