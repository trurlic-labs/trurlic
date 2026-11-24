//! Which files are the tree's sources, and what is at an anchor's path.
//!
//! The walk goes down from the code root and takes every Rust file and
//! `Cargo.toml`: a Rust file's definitions can come from any crate's
//! macros, so the tree's facts hold all of them. It never leaves the tree
//! the call comes from: it passes over every entry whose name starts with
//! `.` (`.git`, `.trurlic`, editor and tool state), every directory at the
//! top of another work tree (one holding `.git`: a submodule, a nested
//! clone, a linked worktree placed inside this one, whose definitions
//! would count twice), every directory tagged a cache (`CACHEDIR.TAG`,
//! which cargo writes into each target directory), and every symlink to a
//! directory, so a symlink loop cannot hold it. It asks git nothing, so a
//! tree with no git is walked by the same rule and a call spawns no
//! process. An anchor's own path is looked at wherever it is.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::code::{FileKind, MANIFEST};

/// A file the walk takes: its path from the root and its kind.
pub(super) type Source = (String, FileKind);

/// The marker files of a directory the walk passes over.
const SKIPPED_MARKERS: [&str; 2] = [".git", "CACHEDIR.TAG"];

/// The Rust files and manifests under `root`, by the rule above. A
/// directory that cannot be listed is passed over, as one that vanished.
pub(super) fn tree_sources(root: &Path) -> Vec<Source> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, relative)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let kind = tree_wide_kind(&name);
            let path = match relative.is_empty() {
                true => name,
                false => format!("{relative}/{name}"),
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let inner = entry.path();
                if !SKIPPED_MARKERS
                    .iter()
                    .any(|marker| inner.join(marker).exists())
                {
                    pending.push((inner, path));
                }
            } else if let Some(kind) = kind {
                found.push((path, kind));
            }
        }
    }
    found
}

/// The kind of a file the tree's facts hold whoever anchors it.
fn tree_wide_kind(name: &str) -> Option<FileKind> {
    match FileKind::of(name) {
        Some(FileKind::Rust) => Some(FileKind::Rust),
        Some(FileKind::Toml) if name == MANIFEST => Some(FileKind::Toml),
        Some(FileKind::Toml | FileKind::Json | FileKind::Make | FileKind::Markdown) | None => None,
    }
}

/// What is at `path` under `root`, symlinks followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Probe {
    Absent,
    Directory,
    /// A file, or anything else that is there: a FIFO, a symlink loop,
    /// a path that cannot be looked at. The read says which.
    Other,
}

pub(super) fn probe(root: &Path, path: &str) -> Probe {
    match fs::metadata(root.join(path)) {
        Ok(metadata) if metadata.is_dir() => Probe::Directory,
        Ok(_) => Probe::Other,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Probe::Absent
        }
        Err(_) => Probe::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    #[test]
    fn the_walk_takes_the_trees_rust_files_and_manifests_by_its_rule() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        for path in [
            "Cargo.toml",
            "src/lib.rs",
            "src/a/b.rs",
            "crates/x/Cargo.toml",
            "crates/x/pyproject.toml",
            "README.md",
            ".hidden/a.rs",
            "src/.cache.rs",
            "target/CACHEDIR.TAG",
            "target/debug/build/out.rs",
            "nested/.git",
            "nested/src/lib.rs",
        ] {
            write(root, path);
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root, root.join("src/loop")).unwrap();

        let mut found = tree_sources(root);
        found.sort();

        assert_eq!(
            found,
            [
                ("Cargo.toml".to_owned(), FileKind::Toml),
                ("crates/x/Cargo.toml".to_owned(), FileKind::Toml),
                ("src/a/b.rs".to_owned(), FileKind::Rust),
                ("src/lib.rs".to_owned(), FileKind::Rust),
            ]
        );
    }

    #[test]
    fn a_probe_tells_a_directory_from_what_else_is_there() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs");

        assert_eq!(probe(tmp.path(), "src"), Probe::Directory);
        assert_eq!(probe(tmp.path(), "src/lib.rs"), Probe::Other);
        assert_eq!(probe(tmp.path(), "src/gone.rs"), Probe::Absent);
        assert_eq!(probe(tmp.path(), "src/lib.rs/x"), Probe::Absent);
    }
}
