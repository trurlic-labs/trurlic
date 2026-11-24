//! Finding the graph from the directory a call starts in, and the code
//! root: the tree the call comes from, which anchors resolve against.
//!
//! The walk goes up from the directory and never leaves the work tree it
//! starts in. In each directory it takes the `.trurlic` there, a symlink
//! of that name followed to the graph it leads to, so a clone or a copy
//! outside the repository reaches the graph through one. A directory with
//! no `.trurlic` whose `.git` is a file is the top of a linked worktree,
//! or of a submodule: the walk stops, and the graph is the main worktree's
//! when that one states `shared`, which git names from the common dir. A
//! directory whose `.git` is a directory is the top of a main worktree,
//! or of a repository cloned inside another project's tree: a graph above
//! it is not that repository's, and the walk stops with none. A tree with
//! no git at all is walked to the root of the file system. Git runs only
//! at a linked worktree's top, with no graph found below it: a call from a
//! tree that holds its graph, the per-call path of `serve`, makes two
//! metadata calls per directory and spawns no process. The graph's own
//! repository, `.trurlic/.git`, is no work tree: the walk passes through
//! a graph directory to the tree that holds it, and discovery never runs
//! git inside one (`git::graph` alone does).

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::git::{self, DotGit, Head};
use crate::store::{Placement, STORE_DIR, Store};
use crate::{Error, Result};

/// The graph a call from `cwd` reads, with the tree it is read for. Fails
/// with [`Error::StoreNotFound`] when the work tree holds none, and with a
/// typed error for a `.trurlic` that leads to no directory, which stops
/// the walk: a graph above it is not the one the tree means.
pub(crate) fn discover(cwd: &Path) -> Result<Store> {
    let start = cwd.canonicalize().map_err(Error::io(cwd))?;
    let not_found = || Error::StoreNotFound(cwd.to_path_buf());
    for dir in start.ancestors() {
        if let Some(graph) = graph_in(dir)? {
            return Ok(Store::reached_from(graph, dir.to_path_buf()));
        }
        // A graph directory's own `.git` is its history, no work tree.
        if dir.file_name().is_some_and(|name| name == STORE_DIR) {
            continue;
        }
        match git::dot_git_in(dir)? {
            DotGit::Absent => {}
            DotGit::File => return shared_graph_of(dir)?.ok_or_else(not_found),
            DotGit::Directory => return Err(not_found()),
        }
    }
    Err(not_found())
}

/// The real path of the graph directory `dir/.trurlic` leads to: itself,
/// or a symlink's target. `None` when there is no `.trurlic` in `dir`.
pub(crate) fn graph_in(dir: &Path) -> Result<Option<PathBuf>> {
    let entry = dir.join(STORE_DIR);
    let metadata = match fs::symlink_metadata(&entry) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                path: entry,
                source,
            });
        }
    };
    if metadata.is_dir() {
        // `dir` is canonical when the walk passes it; a caller with another
        // spelling gets the real path, as for a symlink.
        return entry.canonicalize().map(Some).map_err(Error::io(&entry));
    }
    if !metadata.is_symlink() {
        return Err(Error::GraphNotDirectory { path: entry });
    }
    // Resolving the whole chain is bounded by the system's limit on
    // symlinks, so a loop is an error here, never a hang.
    let target = match entry.canonicalize() {
        Ok(target) => target,
        Err(source) => {
            return Err(Error::GraphLinkBroken {
                link: entry,
                source,
            });
        }
    };
    if target.is_dir() {
        Ok(Some(target))
    } else {
        Err(Error::GraphNotDirectory { path: entry })
    }
}

/// The main worktree's graph, when it states `shared`, as it serves the
/// linked worktree that holds `dir`, read for that worktree's top. `None`
/// outside a linked worktree, in a bare repository's, and when the main
/// worktree holds no shared graph.
pub(crate) fn shared_graph_serving(dir: &Path) -> Result<Option<Store>> {
    if git::dot_git(dir)? != DotGit::File {
        return Ok(None);
    }
    let Some(top) = git::toplevel(dir)? else {
        return Ok(None);
    };
    let top = top.canonicalize().map_err(Error::io(&top))?;
    shared_graph_of(&top)
}

/// The main worktree's graph, when it states `shared`, read for `top`,
/// the canonical top of a work tree whose `.git` is a file: a linked
/// worktree's, a submodule's. `None` when the main worktree is `top`
/// itself (a submodule, a repository made with `--separate-git-dir`) or
/// holds no shared graph, and in a bare repository's worktree.
fn shared_graph_of(top: &Path) -> Result<Option<Store>> {
    let worktrees = git::worktrees(top)?;
    let Some(main) = worktrees.first().filter(|main| main.head != Head::Bare) else {
        return Ok(None);
    };
    if same_dir(&main.path, top) {
        return Ok(None);
    }
    let Some(graph) = graph_in(&main.path)? else {
        return Ok(None);
    };
    let store = Store::reached_from(graph, top.to_path_buf());
    match store.stated_placement()? {
        Placement::Shared => Ok(Some(store)),
        Placement::Tracked => Ok(None),
    }
}

/// Whether `path` is the canonical directory `dir`; a path that cannot be
/// resolved is none.
pub(crate) fn same_dir(path: &Path, dir: &Path) -> bool {
    path.canonicalize().is_ok_and(|path| path == dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store_in(dir: &Path) {
        Store::create(dir, "t", Placement::Tracked).unwrap();
    }

    #[test]
    fn the_walk_finds_the_nearest_graph_and_reads_it_for_its_tree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        store_in(&root);
        let nested = root.join("src/deep");
        fs::create_dir_all(&nested).unwrap();

        for start in [root.as_path(), nested.as_path()] {
            let store = discover(start).unwrap();
            assert_eq!(store.root(), root.join(STORE_DIR));
            assert_eq!(store.code_root(), root);
            assert!(store.is_local());
        }
        // Inside the graph directory the walk meets it too.
        let inside = discover(&root.join(STORE_DIR).join("decisions")).unwrap();
        assert_eq!(inside.root(), root.join(STORE_DIR));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_leads_to_the_graph_and_the_tree_that_holds_it_is_the_code_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main = root.join("main");
        let clone = root.join("clone");
        fs::create_dir_all(clone.join("src")).unwrap();
        fs::create_dir_all(&main).unwrap();
        store_in(&main);
        std::os::unix::fs::symlink("../main/.trurlic", clone.join(STORE_DIR)).unwrap();

        let store = discover(&clone.join("src")).unwrap();

        assert_eq!(store.root(), main.join(STORE_DIR));
        assert_eq!(store.code_root(), clone);
        assert!(!store.is_local());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_or_looping_symlink_stops_the_walk_with_its_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        store_in(&root);
        let dangling = root.join("dangling");
        let looping = root.join("looping");
        fs::create_dir_all(&dangling).unwrap();
        fs::create_dir_all(&looping).unwrap();
        std::os::unix::fs::symlink("gone", dangling.join(STORE_DIR)).unwrap();
        std::os::unix::fs::symlink(".trurlic", looping.join(STORE_DIR)).unwrap();

        for dir in [dangling, looping] {
            let err = discover(&dir).unwrap_err();
            assert!(
                matches!(&err, Error::GraphLinkBroken { link, .. } if *link == dir.join(STORE_DIR)),
                "{err:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_trurlic_that_is_no_directory_is_refused() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let file = root.join("file");
        let link = root.join("link");
        fs::create_dir_all(&file).unwrap();
        fs::create_dir_all(&link).unwrap();
        fs::write(file.join(STORE_DIR), "").unwrap();
        fs::write(root.join("plain"), "").unwrap();
        std::os::unix::fs::symlink("../plain", link.join(STORE_DIR)).unwrap();

        for dir in [file, link] {
            let err = discover(&dir).unwrap_err();
            assert!(
                matches!(&err, Error::GraphNotDirectory { path } if *path == dir.join(STORE_DIR)),
                "{err:?}"
            );
        }
    }

    /// The top of a repository inside the project's tree ends the walk:
    /// the project's graph above it is not its graph. Asking takes no git.
    #[test]
    fn the_walk_stops_at_the_top_of_a_nested_repository() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        store_in(&root);
        let nested = root.join("vendor/lib");
        fs::create_dir_all(nested.join(".git")).unwrap();
        fs::create_dir_all(nested.join("src")).unwrap();

        for start in [nested.clone(), nested.join("src")] {
            let err = discover(&start).unwrap_err();
            assert!(matches!(err, Error::StoreNotFound(_)), "{err:?}");
        }
        assert_eq!(discover(&root.join("vendor")).unwrap().code_root(), root);
    }

    #[test]
    fn no_graph_and_no_git_is_store_not_found() {
        let tmp = TempDir::new().unwrap();

        let err = discover(tmp.path()).unwrap_err();

        assert!(matches!(err, Error::StoreNotFound(_)), "{err:?}");
    }
}
