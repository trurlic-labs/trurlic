//! What the history of the calling branch says of the code root: each of
//! its newest commits with the changes it made, what the work tree and the
//! index changed against `HEAD`, and the bytes of blobs. A change names the
//! blob its path held before and after it, so a file that moved unchanged
//! is the same blob leaving one path and reaching another, which trurlic
//! reads without git's similarity scoring. Every path is relative to the
//! directory git runs in (`--relative`, `<rev>:./<path>`), as an anchor is
//! to its code root, so a path outside that directory is never named; a
//! path that is not UTF-8 names no anchor and is left out.
//!
//! No rename detection, external diff or text conversion runs, and every
//! command runs with the settings of [`PINNED`], so no configuration of the
//! user's changes what it prints.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

use super::{MAX_OUTPUT_BYTES, command, run, shown};

/// The newest commits of `HEAD`'s history the walk lists, in topological
/// order: what a repair is judged by. A file whose code moved before them
/// gets no repair, and a walk this long stays well within
/// [`MAX_OUTPUT_BYTES`] at fifty files a commit.
pub(crate) const MAX_WALKED_COMMITS: usize = 10_000;

/// The settings every history read runs with, each one a user's
/// configuration may set otherwise: a signature check prints its lines
/// before a commit's header; without the root commit's paths, a file added
/// there has no commit that added it; a color code would sit in what is
/// parsed.
const PINNED: [&str; 4] = [
    "log.showSignature=false",
    "log.showRoot=true",
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

/// One change of a path, as `--raw` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) path: String,
    /// The letter of its status: `A`, `D`, `M` or `T`.
    pub(crate) status: u8,
    /// The blob at the path before the change and after it; `None` where
    /// git names none: nothing was there, or, for the work tree, git did
    /// not hash the file.
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}

/// A commit of the walk, and the changes it made against its first parent:
/// none for a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) id: String,
    pub(crate) changes: Vec<Change>,
}

/// The [`MAX_WALKED_COMMITS`] newest commits of `HEAD`'s history, newest
/// first, each listed after every commit that descends from it, with the
/// changes each made under `dir`. One walk with no pathspec. Fails on an
/// unborn branch.
pub(crate) fn walk(dir: &Path) -> Result<Vec<Commit>> {
    let count = format!("--max-count={MAX_WALKED_COMMITS}");
    let options = [
        "log",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        "--relative",
        "--topo-order",
        "-z",
        "--format=commit %H",
        &count,
        "HEAD",
        "--",
    ];
    let args: Vec<&OsStr> = options.iter().map(OsStr::new).collect();
    let stdout = run(history(dir), dir, &args, None)?;
    let read = || -> Option<Vec<Commit>> {
        let mut commits: Vec<Commit> = Vec::new();
        for record in parse_records(&stdout)? {
            match record {
                Record::Commit(id) => commits.push(Commit {
                    id,
                    changes: Vec::new(),
                }),
                Record::Change(change) => commits.last_mut()?.changes.push(change),
            }
        }
        Some(commits)
    };
    read().ok_or_else(|| Error::GitOutput {
        args: shown(&args),
        detail: "a change before any `commit <id>` record, or one cut short",
    })
}

/// What the work tree and the index changed under `dir` against `HEAD`,
/// each path once: staged or not, untracked files left out.
pub(crate) fn uncommitted(dir: &Path) -> Result<Vec<Change>> {
    // A read: git writes no refreshed index of the project's repository.
    let args: Vec<&OsStr> = [
        "--no-optional-locks",
        "diff",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        "--relative",
        "-z",
        "HEAD",
        "--",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    let stdout = run(history(dir), dir, &args, None)?;
    let changes = parse_records(&stdout).and_then(|records| {
        records
            .into_iter()
            .map(|record| match record {
                Record::Change(change) => Some(change),
                Record::Commit(_) => None,
            })
            .collect::<Option<Vec<Change>>>()
    });
    changes.ok_or_else(|| Error::GitOutput {
        args: shown(&args),
        detail: "a change record cut short",
    })
}

/// One record of `--raw -z` output under `--format='commit %H'`.
#[derive(Debug, PartialEq, Eq)]
enum Record {
    Commit(String),
    Change(Change),
}

/// The records of `--raw -z --no-renames` output, a commit's header first
/// when there is one: `:<mode> <mode> <blob> <blob> <status>`, then the
/// path. `None` when a change is cut short. A change of a path that is not
/// UTF-8 is left out.
fn parse_records(stdout: &[u8]) -> Option<Vec<Record>> {
    let mut records = Vec::new();
    let mut tokens = stdout.split(|&byte| byte == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let token = token.strip_prefix(b"\n").unwrap_or(token);
        if let Some(id) = token.strip_prefix(b"commit ") {
            records.push(Record::Commit(std::str::from_utf8(id).ok()?.to_owned()));
            continue;
        }
        let fields = std::str::from_utf8(token.strip_prefix(b":")?).ok()?;
        let [_, _, before, after, status] = fields.split(' ').collect::<Vec<_>>()[..] else {
            return None;
        };
        let &status = status.as_bytes().first()?;
        if let Ok(path) = std::str::from_utf8(tokens.next()?) {
            records.push(Record::Change(Change {
                path: path.to_owned(),
                status,
                before: blob(before),
                after: blob(after),
            }));
        }
    }
    Some(records)
}

/// The blob id `--raw` printed, `None` for git's all-zero id.
fn blob(id: &str) -> Option<String> {
    (!id.bytes().all(|byte| byte == b'0')).then(|| id.to_owned())
}

/// The name of the file at `path`, relative to the directory git runs in,
/// at `revision`, as `cat-file` reads one.
pub(crate) fn at_revision(revision: &str, path: &str) -> String {
    format!("{revision}:./{path}")
}

/// The blobs one read gave: git's output, and where each name's bytes are
/// in it.
#[derive(Debug, Default)]
pub(crate) struct Blobs {
    stdout: Vec<u8>,
    at: BTreeMap<String, Range<usize>>,
}

impl Blobs {
    /// The bytes of the blob git read for `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&[u8]> {
        let at = self.at.get(name)?;
        self.stdout.get(at.start..at.end)
    }
}

/// The bytes git names a blob of at most `max` bytes for, for each of
/// `names`, a blob id or a name from [`at_revision`]: `git cat-file
/// --batch-check` for the sizes, then `--batch` for the blobs within the
/// bound, two processes for them all. Blobs are taken in the order of
/// `names` while their output fits [`MAX_OUTPUT_BYTES`]; the rest, a name
/// holding a line break, which the batch's lines cannot carry, and one git
/// names no blob for are left out.
pub(crate) fn blobs(dir: &Path, names: &[String], max: u64) -> Result<Blobs> {
    let lines = |names: &[&String]| -> Vec<u8> {
        let mut input = Vec::new();
        for name in names {
            input.extend_from_slice(name.as_bytes());
            input.push(b'\n');
        }
        input
    };
    let asked: Vec<&String> = names.iter().filter(|name| !name.contains('\n')).collect();
    if asked.is_empty() {
        return Ok(Blobs::default());
    }
    let check = [OsStr::new("cat-file"), OsStr::new("--batch-check")];
    let sizes = run(history(dir), dir, &check, Some(&lines(&asked)))?;
    let fitting = fitting(asked, &sizes, max);
    if fitting.is_empty() {
        return Ok(Blobs::default());
    }
    let batch = [OsStr::new("cat-file"), OsStr::new("--batch")];
    let stdout = run(history(dir), dir, &batch, Some(&lines(&fitting)))?;
    let ranges = parse_batch(&stdout).ok_or_else(|| Error::GitOutput {
        args: shown(&batch),
        detail: "a blob record cut short",
    })?;
    // clone: each blob is keyed by the name it was asked by.
    let at = fitting.into_iter().cloned().zip(ranges).collect();
    Ok(Blobs { stdout, at })
}

/// Of `asked`, in order, those `sizes`, the lines `cat-file --batch-check`
/// printed of them, name a blob of at most `max` bytes, while the batch's
/// output of them fits [`MAX_OUTPUT_BYTES`].
fn fitting<'a>(asked: Vec<&'a String>, sizes: &[u8], max: u64) -> Vec<&'a String> {
    let mut room = MAX_OUTPUT_BYTES;
    asked
        .into_iter()
        .zip(sizes.split(|&byte| byte == b'\n'))
        .filter_map(|(name, line)| Some((name, blob_size(line).filter(|&size| size <= max)?)))
        .take_while(
            |&(_, size)| match room.checked_sub(size.saturating_add(RECORD_BYTES)) {
                Some(left) => {
                    room = left;
                    true
                }
                None => false,
            },
        )
        .map(|(name, _)| name)
        .collect()
}

/// The most bytes `cat-file --batch` prints around one blob: its header
/// line, `<id> blob <size>`, a SHA-256 id included, and the line break
/// after the bytes.
const RECORD_BYTES: u64 = 100;

/// The size of a `--batch-check` line naming a blob, `<id> blob <size>`.
fn blob_size(line: &[u8]) -> Option<u64> {
    let line = std::str::from_utf8(line).ok()?;
    let mut fields = line.split(' ');
    let (_, kind, size) = (fields.next()?, fields.next()?, fields.next()?);
    (kind == "blob").then(|| size.parse().ok()).flatten()
}

/// Where the contents of each record of `cat-file --batch` are in
/// `stdout`: `<id> <type> <size>`, a line break, the bytes, a line break.
fn parse_batch(stdout: &[u8]) -> Option<Vec<Range<usize>>> {
    let mut contents = Vec::new();
    let mut at = 0;
    while at < stdout.len() {
        let rest = stdout.get(at..)?;
        let header_end = rest.iter().position(|&byte| byte == b'\n')?;
        let size = usize::try_from(blob_size(rest.get(..header_end)?)?).ok()?;
        let start = at + header_end + 1;
        let end = start.checked_add(size)?;
        stdout.get(start..=end)?;
        contents.push(start..end);
        at = end + 1;
    }
    Some(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_commit_lists_its_changes_with_their_blobs_newest_first() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let zero = "0".repeat(40);
        let stdout = format!(
            "commit c2\0\n:100644 100644 {a} {b} M\0x.rs\0:000000 100644 {zero} {a} A\0y z.rs\0\
             commit c1\0\n:100644 000000 {b} {zero} D\0gone.rs\0commit c0\0"
        );

        let change = |path: &str, status, before: Option<&str>, after: Option<&str>| {
            Record::Change(Change {
                path: path.to_owned(),
                status,
                before: before.map(str::to_owned),
                after: after.map(str::to_owned),
            })
        };
        assert_eq!(
            parse_records(stdout.as_bytes()).unwrap(),
            [
                Record::Commit("c2".into()),
                change("x.rs", b'M', Some(&a), Some(&b)),
                change("y z.rs", b'A', None, Some(&a)),
                Record::Commit("c1".into()),
                change("gone.rs", b'D', Some(&b), None),
                Record::Commit("c0".into()),
            ]
        );
        assert_eq!(
            parse_records(format!(":100644 100644 {a} {b} M\0").as_bytes()),
            None
        );
        assert_eq!(parse_records(b":100644 M\0a.rs\0"), None);
        assert_eq!(parse_records(b""), Some(Vec::new()));
    }

    #[test]
    fn a_batch_of_blobs_is_read_record_by_record() {
        let stdout = b"aa blob 3\nabc\nbb blob 0\n\ncc blob 2\n\n\n\n";

        let ranges = parse_batch(stdout).unwrap();
        let contents: Vec<&[u8]> = ranges.into_iter().map(|at| &stdout[at]).collect();
        assert_eq!(contents, [&b"abc"[..], b"", b"\n\n"]);
        assert_eq!(parse_batch(b"aa blob 5\nab\n"), None);
        assert_eq!(blob_size(b"x.rs missing"), None);
        assert_eq!(blob_size(b"aa tree 12"), None);
    }
}
