//! What the history of the calling branch says of an anchored file: the
//! commit that last changed it (for a file gone from `HEAD`, the one that
//! deleted it), what that commit, or the work tree against `HEAD`, renamed
//! or copied it to, and the bytes of a file at a revision. Every path is
//! relative to the directory git runs in (`--relative`, `<rev>:./<path>`),
//! as an anchor is to its code root, so a path outside that directory is
//! never named; a path that is not UTF-8 names no anchor and is left out.
//!
//! Renames and copies are git's own detection (`-M -C`) at its default
//! similarity, and no external diff or text conversion runs.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use crate::{Error, Result};

use super::{command, run, shown};

/// What a diff says a file that left its path became. A file split in
/// two shows as two records from its path, a copy beside the rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Moved {
    pub(crate) from: String,
    /// `None` for a deletion.
    pub(crate) to: Option<String>,
    /// git's similarity index of a rename or a copy, 0 to 100; 0 for a
    /// deletion.
    pub(crate) similarity: u8,
}

/// What a diff compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Changes<'c> {
    /// The commit with its first parent.
    Commit(&'c str),
    /// The work tree and the index against `HEAD`: what is not committed.
    Uncommitted,
}

/// For each of `paths` the calling branch's history changed, the most
/// recent commit that changed it, which for a path gone from `HEAD` is the
/// one that deleted it: one walk of `HEAD`'s history, limited to those
/// paths, each read literally. A walk of many paths costs about what one
/// costs, which git's changed-path filters cut only for a single path, so
/// one walk serves them all. Fails on an unborn branch.
pub(crate) fn last_commits(dir: &Path, paths: &[&str]) -> Result<BTreeMap<String, String>> {
    let pathspecs: Vec<String> = paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    let mut args: Vec<&OsStr> = [
        "log",
        "--name-only",
        "--relative",
        "--no-renames",
        "-z",
        "--format=commit %H",
        "HEAD",
        "--",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    args.extend(pathspecs.iter().map(OsStr::new));
    let stdout = run(command(dir), dir, &args, None)?;
    parse_commits(&stdout).ok_or_else(|| Error::GitOutput {
        args: shown(&args),
        detail: "a changed path before any `commit <id>` record",
    })
}

/// The records of `log -z --name-only --format='commit %H'`: a header
/// token, then the commit's paths, the first after a line break. The first
/// commit naming a path, the newest, is kept for it.
fn parse_commits(stdout: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut deleted = BTreeMap::new();
    let mut commit: Option<&str> = None;
    for token in stdout.split(|&byte| byte == 0).filter(|t| !t.is_empty()) {
        let text = std::str::from_utf8(token).ok();
        if let Some(id) = text.and_then(|text| text.strip_prefix("commit ")) {
            commit = Some(id);
            continue;
        }
        let path = token.strip_prefix(b"\n").unwrap_or(token);
        let (Some(id), Ok(path)) = (commit, std::str::from_utf8(path)) else {
            commit?;
            continue;
        };
        deleted
            .entry(path.to_owned())
            .or_insert_with(|| id.to_owned());
    }
    Some(deleted)
}

/// The renames, copies and deletions of `changes`, as git detects them.
pub(crate) fn moved(dir: &Path, changes: Changes<'_>) -> Result<Vec<Moved>> {
    let parent;
    let range: Vec<&str> = match changes {
        Changes::Commit(commit) => {
            parent = format!("{commit}^");
            vec![parent.as_str(), commit]
        }
        Changes::Uncommitted => vec!["HEAD"],
    };
    // A read: git writes no refreshed index of the project's repository.
    let mut args: Vec<&OsStr> = [
        "--no-optional-locks",
        "diff",
        "-z",
        "--name-status",
        "--relative",
        "--no-ext-diff",
        "--no-textconv",
        "-M",
        "-C",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    args.extend(range.iter().map(OsStr::new));
    args.push(OsStr::new("--"));
    let stdout = run(command(dir), dir, &args, None)?;
    parse_moved(&stdout).ok_or_else(|| Error::GitOutput {
        args: shown(&args),
        detail: "a rename or copy record without both of its paths",
    })
}

/// The records of `diff -z --name-status`: a status, then one path, or two
/// for a rename (`R<score>`) or a copy (`C<score>`), which are kept with
/// their score, and with a deletion (`D`).
fn parse_moved(stdout: &[u8]) -> Option<Vec<Moved>> {
    let mut moved = Vec::new();
    let mut fields = stdout.split(|&byte| byte == 0);
    while let Some(status) = fields.next() {
        match status.split_first() {
            None => {}
            Some((b'R' | b'C', score)) => {
                let similarity = std::str::from_utf8(score).ok()?.parse().ok()?;
                let mut path = || fields.next().filter(|path| !path.is_empty());
                let (from, to) = (path()?, path()?);
                if let (Ok(from), Ok(to)) = (std::str::from_utf8(from), std::str::from_utf8(to)) {
                    moved.push(Moved {
                        from: from.to_owned(),
                        to: Some(to.to_owned()),
                        similarity,
                    });
                }
            }
            Some((b'D', _)) => {
                let path = fields.next().filter(|path| !path.is_empty())?;
                if let Ok(from) = std::str::from_utf8(path) {
                    moved.push(Moved {
                        from: from.to_owned(),
                        to: None,
                        similarity: 0,
                    });
                }
            }
            // Every other status names one path.
            Some(_) => {
                fields.next()?;
            }
        }
    }
    Some(moved)
}

/// The bytes of each of `files`, a revision and a path, that exists at
/// that revision as a blob of at most `max` bytes: `git cat-file
/// --batch-check` for the sizes, then `--batch` for the blobs within the
/// bound, two processes for them all. A path holding a line break, which
/// the batch's lines cannot carry, is left out, as is one git names no
/// blob for.
pub(crate) fn blobs(
    dir: &Path,
    files: &[(String, String)],
    max: u64,
) -> Result<BTreeMap<(String, String), Vec<u8>>> {
    let names = |files: &[&(String, String)]| -> Vec<u8> {
        let mut input = Vec::new();
        for (revision, path) in files {
            input.extend_from_slice(format!("{revision}:./{path}\n").as_bytes());
        }
        input
    };
    let asked: Vec<&(String, String)> = files
        .iter()
        .filter(|(revision, path)| !revision.contains('\n') && !path.contains('\n'))
        .collect();
    if asked.is_empty() {
        return Ok(BTreeMap::new());
    }
    let check = [OsStr::new("cat-file"), OsStr::new("--batch-check")];
    let sizes = run(command(dir), dir, &check, Some(&names(&asked)))?;
    let fitting: Vec<&(String, String)> = asked
        .into_iter()
        .zip(sizes.split(|&byte| byte == b'\n'))
        .filter(|(_, line)| blob_size(line).is_some_and(|size| size <= max))
        .map(|(file, _)| file)
        .collect();
    if fitting.is_empty() {
        return Ok(BTreeMap::new());
    }
    let batch = [OsStr::new("cat-file"), OsStr::new("--batch")];
    let stdout = run(command(dir), dir, &batch, Some(&names(&fitting)))?;
    let contents = parse_batch(&stdout).ok_or_else(|| Error::GitOutput {
        args: shown(&batch),
        detail: "a blob record cut short",
    })?;
    // clone: each blob is keyed by the revision and path it was asked as.
    Ok(fitting.into_iter().cloned().zip(contents).collect())
}

/// The size of a `--batch-check` line naming a blob, `<id> blob <size>`.
fn blob_size(line: &[u8]) -> Option<u64> {
    let line = std::str::from_utf8(line).ok()?;
    let mut fields = line.split(' ');
    let (_, kind, size) = (fields.next()?, fields.next()?, fields.next()?);
    (kind == "blob").then(|| size.parse().ok()).flatten()
}

/// The contents of each record of `cat-file --batch`: `<id> <type>
/// <size>`, a line break, the bytes, a line break.
fn parse_batch(mut stdout: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut contents = Vec::new();
    while !stdout.is_empty() {
        let header_end = stdout.iter().position(|&byte| byte == b'\n')?;
        let size = usize::try_from(blob_size(stdout.get(..header_end)?)?).ok()?;
        let body = stdout.get(header_end + 1..)?;
        contents.push(body.get(..size)?.to_vec());
        stdout = body.get(size + 1..)?;
    }
    Some(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_the_newest_commit_that_changed_it() {
        let stdout = b"commit c2\0\na.rs\0b.rs\0commit c1\0\na.rs\0\0";

        let deleted = parse_commits(stdout).unwrap();

        assert_eq!(
            deleted,
            BTreeMap::from([
                ("a.rs".to_owned(), "c2".to_owned()),
                ("b.rs".to_owned(), "c2".to_owned()),
            ])
        );
        assert_eq!(parse_commits(b"\na.rs\0"), None);
        assert_eq!(parse_commits(b""), Some(BTreeMap::new()));
    }

    #[test]
    fn renames_copies_and_deletions_are_read_and_the_rest_skipped() {
        let stdout = b"M\0x.rs\0C100\0s.rs\0s1.rs\0R087\0a b.rs\0a\nc.rs\0D\0gone.rs\0";

        assert_eq!(
            parse_moved(stdout).unwrap(),
            [
                Moved {
                    from: "s.rs".into(),
                    to: Some("s1.rs".into()),
                    similarity: 100,
                },
                Moved {
                    from: "a b.rs".into(),
                    to: Some("a\nc.rs".into()),
                    similarity: 87,
                },
                Moved {
                    from: "gone.rs".into(),
                    to: None,
                    similarity: 0,
                },
            ]
        );
        assert_eq!(parse_moved(b"R100\0only.rs\0"), None);
        assert_eq!(parse_moved(b"R\0a.rs\0b.rs\0"), None);
    }

    #[test]
    fn a_batch_of_blobs_is_read_record_by_record() {
        let stdout = b"aa blob 3\nabc\nbb blob 0\n\ncc blob 2\n\n\n\n";

        assert_eq!(
            parse_batch(stdout).unwrap(),
            [b"abc".to_vec(), Vec::new(), b"\n\n".to_vec()]
        );
        assert_eq!(parse_batch(b"aa blob 5\nab\n"), None);
        assert_eq!(blob_size(b"x.rs missing"), None);
        assert_eq!(blob_size(b"aa tree 12"), None);
    }
}
