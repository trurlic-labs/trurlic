//! `trurlic init`: a new graph, placed as the owner states and as git
//! allows. A tracked graph needs a work tree whose git does not ignore
//! it, and gets `.trurlic/.state/` in the `.gitignore` beside it; a shared
//! one is a git repository of its own, which holds its history, and in a
//! work tree sits at the top of the main worktree, where linked worktrees
//! find it, kept out of the project's git through the common dir's
//! `info/exclude`, which every worktree reads. No graph is made in a linked
//! worktree the main worktree's shared graph already serves.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::console::out;
use crate::discovery::{same_dir, shared_graph_serving};
use crate::git::{self, Head};
use crate::store::{PROJECT_FILE, Placement, STORE_DIR, Store};
use crate::{Error, Result};

/// The `.gitignore` entry of a tracked graph.
const STATE_ENTRY: &str = ".trurlic/.state/";

/// The `info/exclude` entry of a shared graph, anchored at the top of the
/// work tree.
const SHARED_ENTRY: &str = "/.trurlic/";

/// Create `.trurlic/` in `cwd`, versioned as `placement` says.
pub(crate) fn init(cwd: &Path, placement: Placement) -> Result<()> {
    let dir = cwd.canonicalize().map_err(Error::io(cwd))?;
    if let Some(served) = shared_graph_serving(&dir)? {
        return Err(Error::SharedGraphServes {
            graph: served.root().to_path_buf(),
        });
    }
    let in_work_tree = git::toplevel(&dir)?.is_some();
    let ignore_file = match placement {
        Placement::Tracked => {
            tracked_allowed(&dir, in_work_tree)?;
            Some((dir.join(".gitignore"), STATE_ENTRY))
        }
        Placement::Shared if in_work_tree => Some((shared_exclude(&dir)?, SHARED_ENTRY)),
        Placement::Shared => None,
    };
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project");
    Store::create(&dir, name, placement)?;
    let Some((path, entry)) = ignore_file else {
        return out!("Initialized .trurlic/ (shared), its history in .trurlic/.git");
    };
    append_line(&path, entry)?;
    match placement {
        Placement::Tracked => out!("Initialized .trurlic/ (tracked): commit it with the project"),
        Placement::Shared => out!(
            "Initialized .trurlic/ (shared), its history in .trurlic/.git: kept out of the \
             project's git by {}",
            path.display()
        ),
    }
}

/// Refuse a tracked graph in `dir` that no git can version: no work tree
/// holds `dir`, or an ignore rule matches the graph.
fn tracked_allowed(dir: &Path, in_work_tree: bool) -> Result<()> {
    if !in_work_tree {
        return Err(Error::NoWorkTree {
            dir: dir.to_path_buf(),
        });
    }
    let project = Path::new(STORE_DIR).join(PROJECT_FILE);
    if git::is_ignored(dir, &project)? {
        return Err(Error::GraphIgnored {
            graph: dir.join(STORE_DIR),
        });
    }
    Ok(())
}

/// The `info/exclude` that keeps a shared graph in `dir` out of git,
/// refused unless `dir` is the top of the main worktree.
fn shared_exclude(dir: &Path) -> Result<PathBuf> {
    match git::worktrees(dir)?.into_iter().next() {
        Some(main) if main.head != Head::Bare => {
            if !same_dir(&main.path, dir) {
                return Err(Error::SharedOffMain {
                    dir: dir.to_path_buf(),
                    main: main.path,
                });
            }
        }
        // A bare repository lists itself first, and only a bare one lists
        // nothing.
        Some(_) | None => {
            return Err(Error::SharedInBare {
                dir: dir.to_path_buf(),
            });
        }
    }
    Ok(git::common_dir(dir)?.join("info").join("exclude"))
}

/// Append `entry` as a line of the file at `path`, creating the file and
/// its directory when absent, unless a line already holds it.
fn append_line(path: &Path, entry: &str) -> Result<()> {
    if path.exists() {
        let content = fs::read_to_string(path).map_err(Error::io(path))?;
        if content.lines().any(|line| line.trim() == entry) {
            return Ok(());
        }
        let separator = if content.is_empty() || content.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{separator}{entry}"))
            .map_err(Error::io(path))
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(Error::io(parent))?;
        }
        fs::write(path, format!("{entry}\n")).map_err(Error::io(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn gitignore(dir: &Path) -> PathBuf {
        dir.join(".gitignore")
    }

    #[test]
    fn append_line_creates_the_file_when_absent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("info/exclude");

        append_line(&path, SHARED_ENTRY).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "/.trurlic/\n");
    }

    #[test]
    fn append_line_appends_after_a_line_break_it_adds_when_missing() {
        let tmp = TempDir::new().unwrap();
        let path = gitignore(tmp.path());
        fs::write(&path, "/target/").unwrap();

        append_line(&path, STATE_ENTRY).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "/target/\n.trurlic/.state/\n"
        );
    }

    #[test]
    fn append_line_does_not_duplicate_an_entry() {
        let tmp = TempDir::new().unwrap();
        let path = gitignore(tmp.path());
        fs::write(&path, "/target/\n.trurlic/.state/\n").unwrap();

        append_line(&path, STATE_ENTRY).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "/target/\n.trurlic/.state/\n"
        );
    }
}
