//! The migration's test fixtures: the committed graph of each format as a
//! store and as a tree, the placement the tests state, and a step that
//! changes nothing.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::placement::PlacementSource;
use super::steps::Step;
use super::tree::Tree;
use crate::store::format::Store;
use crate::store::model::{FormatVersion, Placement, STORE_DIR};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A store holding the committed fixture graph of format `version`.
pub(in crate::store) fn fixture_store(version: &str) -> (TempDir, Store) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formats")
        .join(version);
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(STORE_DIR);
    copy_dir(&fixture, &root);
    (tmp, Store::at(root))
}

/// The placement the tests state, so no test asks git.
pub(in crate::store) fn tracked() -> PlacementSource {
    PlacementSource::Stated(Placement::Tracked)
}

/// The fixture graph of format `version` as a tree.
pub(super) fn fixture_tree(version: &str) -> (TempDir, Tree) {
    let (tmp, store) = fixture_store(version);
    let (tree, _) = store.read_tree(tracked()).unwrap();
    (tmp, tree)
}

/// A step from `from` to `to` that changes nothing.
pub(in crate::store) fn step(from: (u32, u32, u32), to: (u32, u32, u32)) -> Step {
    Step {
        from: FormatVersion::new(from.0, from.1, from.2),
        to: FormatVersion::new(to.0, to.1, to.2),
        change: "a test step",
        apply: |_| Ok(()),
    }
}
