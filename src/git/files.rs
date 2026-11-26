//! The questions about the files of a work tree: which the index tracks and
//! which an ignore rule matches.

use std::path::{Path, PathBuf};

use crate::Result;

use super::encoding::paths;
use super::process::{command, failed, finish, run};

/// The paths under `pathspec` that the index of the repository holding
/// `dir` tracks, relative to `dir` as git prints them. Fails when `dir` is
/// in no work tree.
pub(crate) fn tracked(dir: &Path, pathspec: &Path) -> Result<Vec<PathBuf>> {
    let args = [
        "ls-files".as_ref(),
        "-z".as_ref(),
        "--".as_ref(),
        pathspec.as_os_str(),
    ];
    Ok(paths(&run(command(dir), dir, &args, None)?))
}

/// The files under `pathspec` that the index does not hold and an ignore
/// rule matches (`.gitignore`, `info/exclude`, `core.excludesFile`),
/// relative to `dir` as git prints them. A file the index holds is never
/// ignored, whatever rule matches it.
pub(crate) fn ignored(dir: &Path, pathspec: &Path) -> Result<Vec<PathBuf>> {
    let args = [
        "ls-files".as_ref(),
        "-z".as_ref(),
        "--others".as_ref(),
        "--ignored".as_ref(),
        "--exclude-standard".as_ref(),
        "--".as_ref(),
        pathspec.as_os_str(),
    ];
    Ok(paths(&run(command(dir), dir, &args, None)?))
}

/// Whether an ignore rule matches `path`, relative to `dir`, whether or not
/// the file exists: `git check-ignore`, whose exit status is the answer.
pub(crate) fn is_ignored(dir: &Path, path: &Path) -> Result<bool> {
    let args = [
        "check-ignore".as_ref(),
        "--quiet".as_ref(),
        "--".as_ref(),
        path.as_os_str(),
    ];
    let output = finish(command(dir), &args, None)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        // A fatal error, or a signal.
        _ => Err(failed(dir, &args, &output)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::testing::git;
    use super::*;

    #[test]
    fn tracked_lists_the_indexed_paths_under_the_pathspec() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet"]);
        fs::create_dir_all(dir.join("store/sub")).unwrap();
        for file in [
            "store/a b.toml",
            "store/sub/c\nd.toml",
            "store/new.toml",
            "outside.toml",
        ] {
            fs::write(dir.join(file), "").unwrap();
        }
        git(dir, &["add", "store/a b.toml", "store/sub", "outside.toml"]);

        let mut found = tracked(dir, Path::new("store")).unwrap();
        found.sort();

        assert_eq!(
            found,
            [
                PathBuf::from("store/a b.toml"),
                PathBuf::from("store/sub/c\nd.toml")
            ]
        );
    }

    #[test]
    fn ignored_lists_untracked_files_a_rule_matches_and_is_ignored_needs_no_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet"]);
        fs::create_dir_all(dir.join("store/sub")).unwrap();
        for file in ["store/kept.toml", "store/sub/a b.toml", "store/forced.toml"] {
            fs::write(dir.join(file), "").unwrap();
        }
        fs::write(dir.join(".gitignore"), "sub/\nforced.toml\n").unwrap();
        git(dir, &["add", "--force", "store/forced.toml"]);

        assert_eq!(
            ignored(dir, Path::new("store")).unwrap(),
            [PathBuf::from("store/sub/a b.toml")]
        );
        assert!(is_ignored(dir, Path::new("store/sub/new.toml")).unwrap());
        assert!(!is_ignored(dir, Path::new("store/new.toml")).unwrap());
    }
}
