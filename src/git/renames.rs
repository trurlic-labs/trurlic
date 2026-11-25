//! What the history of the calling branch says of an anchored file: the
//! commits that last changed and last added it (for a file gone from
//! `HEAD`, the last change deleted it), what a commit, or the work tree
//! against `HEAD`, renamed or copied it to, the commits that changed how
//! often it spells a name, the paths a commit changed, and the bytes of a
//! file at a revision. Every path is relative to the directory git runs in
//! (`--relative`, `<rev>:./<path>`), as an anchor is to its code root, so a
//! path outside that directory is never named; a path that is not UTF-8
//! names no anchor and is left out.
//!
//! Renames and copies are git's own detection (`-M -C`) at its default
//! similarity, and no external diff or text conversion runs. Every command
//! runs with the settings of [`PINNED`], so no configuration of the user's
//! changes what it prints.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

use super::{command, run, shown};

/// The settings every history read runs with, each one a user's
/// configuration may set otherwise: a signature check prints its lines
/// before a commit's header; with one path, `log` follows its renames into
/// another path's commits; without the root commit's paths, a file added
/// there has no commit that added it; `log` re-encodes all it prints; past
/// the rename limit git pairs no rename, and 1000 is git's own default; a
/// color code would sit in what is parsed.
const PINNED: [&str; 7] = [
    "log.showSignature=false",
    "log.follow=false",
    "log.showRoot=true",
    "i18n.logOutputEncoding=UTF-8",
    "diff.renameLimit=1000",
    "color.ui=never",
    "color.diff=never",
];

/// `git -C <dir>` with [`PINNED`], for its caller to add to.
fn history(dir: &Path) -> Command {
    let mut command = command(dir);
    for setting in PINNED {
        command.arg("-c").arg(setting);
    }
    command
}

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

/// What `HEAD`'s history says of a path it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Changed {
    /// The newest commit that changed it: for a path gone from `HEAD`, the
    /// one that deleted it.
    pub(crate) last: String,
    /// The newest commit that added it; `None` when the history holds no
    /// such commit.
    pub(crate) added: Option<String>,
}

/// For each of `paths` the calling branch's history changed, its newest
/// change and its newest addition: one walk of `HEAD`'s history, limited to
/// those paths, each read literally. A walk of many paths costs about what
/// one costs, which git's changed-path filters cut only for a single path,
/// so one walk serves them all. Fails on an unborn branch.
pub(crate) fn last_changes(dir: &Path, paths: &[&str]) -> Result<BTreeMap<String, Changed>> {
    let pathspecs = literal(paths);
    let mut args: Vec<&OsStr> = [
        "log",
        "--name-status",
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
    let commits = records(&run(history(dir), dir, &args, None)?, &args)?;
    let mut changed: BTreeMap<String, Changed> = BTreeMap::new();
    for Record { commit, paths } in commits {
        for (status, path) in paths {
            let found = changed.entry(path).or_insert_with(|| Changed {
                // clone: a path's first record, its newest, names its commit.
                last: commit.clone(),
                added: None,
            });
            if status == b'A' && found.added.is_none() {
                // clone: as above, for its newest addition.
                found.added = Some(commit.clone());
            }
        }
    }
    Ok(changed)
}

/// The commits of `HEAD`'s history after `since`, newest first, that
/// changed how many times `path` spells `name`: git's pickaxe (`log -S`),
/// read literally. A commit that gives `path` a definition of `name` is
/// among them.
pub(crate) fn spelling_changes(
    dir: &Path,
    since: &str,
    path: &str,
    name: &str,
) -> Result<Vec<String>> {
    let pickaxe = format!("-S{name}");
    let range = format!("{since}..HEAD");
    let pathspec = format!(":(literal){path}");
    let args = [
        "log",
        "--format=%H",
        "--no-ext-diff",
        "--no-textconv",
        &pickaxe,
        &range,
        "--",
        &pathspec,
    ]
    .map(OsStr::new);
    let stdout = run(history(dir), dir, &args, None)?;
    // Commit ids are hex: a lossy read changes none.
    Ok(String::from_utf8_lossy(&stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The paths each of `commits` changed against its first parent, none for
/// a merge: one `git log --no-walk` for them all.
pub(crate) fn changed_by(
    dir: &Path,
    commits: &BTreeSet<&str>,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    if commits.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut args: Vec<&OsStr> = [
        "log",
        "--no-walk=unsorted",
        "--name-status",
        "--relative",
        "--no-renames",
        "-z",
        "--format=commit %H",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    args.extend(commits.iter().map(OsStr::new));
    args.push(OsStr::new("--"));
    let found = records(&run(history(dir), dir, &args, None)?, &args)?;
    Ok(found
        .into_iter()
        .map(|Record { commit, paths }| (commit, paths.into_iter().map(|(_, path)| path).collect()))
        .collect())
}

/// Each path as a literal pathspec.
fn literal(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect()
}

/// One commit `log -z --name-status` lists: its id, and each path it
/// changed with the letter of its status.
#[derive(Debug, PartialEq, Eq)]
struct Record {
    commit: String,
    paths: Vec<(u8, String)>,
}

/// The records of `log -z --name-status --format='commit %H'`, read from
/// the output of the command `args` ran: or that it printed a path before
/// any commit.
fn records(stdout: &[u8], args: &[&OsStr]) -> Result<Vec<Record>> {
    parse_records(stdout).ok_or_else(|| Error::GitOutput {
        args: shown(args),
        detail: "a changed path before any `commit <id>` record",
    })
}

/// The records of `log -z --name-status --format='commit %H'`, newest
/// first: a header token, then a status and a path for each path the
/// commit changed, the first status after a line break. `None` when a
/// status comes before any header or has no path.
fn parse_records(stdout: &[u8]) -> Option<Vec<Record>> {
    let mut commits: Vec<Record> = Vec::new();
    let mut tokens = stdout.split(|&byte| byte == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let text = std::str::from_utf8(token).ok();
        if let Some(id) = text.and_then(|text| text.strip_prefix("commit ")) {
            commits.push(Record {
                commit: id.to_owned(),
                paths: Vec::new(),
            });
            continue;
        }
        let status = token.strip_prefix(b"\n").unwrap_or(token);
        let (&letter, _) = status.split_first()?;
        let path = tokens.next()?;
        let record = commits.last_mut()?;
        if let Ok(path) = std::str::from_utf8(path) {
            record.paths.push((letter, path.to_owned()));
        }
    }
    Some(commits)
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
    let stdout = run(history(dir), dir, &args, None)?;
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
    let sizes = run(history(dir), dir, &check, Some(&names(&asked)))?;
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
    let stdout = run(history(dir), dir, &batch, Some(&names(&fitting)))?;
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
    fn each_commit_lists_its_paths_with_their_status_newest_first() {
        let stdout = b"commit c2\0\nM\0a.rs\0A\0b.rs\0commit c1\0\nA\0a.rs\0\0commit c0\0";

        let record = |commit: &str, paths: &[(u8, &str)]| Record {
            commit: commit.to_owned(),
            paths: paths.iter().map(|&(s, p)| (s, p.to_owned())).collect(),
        };
        assert_eq!(
            parse_records(stdout).unwrap(),
            [
                record("c2", &[(b'M', "a.rs"), (b'A', "b.rs")]),
                record("c1", &[(b'A', "a.rs")]),
                record("c0", &[]),
            ]
        );
        assert_eq!(parse_records(b"\nM\0a.rs\0"), None);
        assert_eq!(parse_records(b"commit c\0\nM\0"), None);
        assert_eq!(parse_records(b""), Some(Vec::new()));
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
