//! The graph's own repository: the git repository at `.trurlic/.git` that
//! holds a shared graph's history, which only trurlic writes. This is the
//! one module that runs git inside a graph directory; discovery and the
//! placement facts ask git only about the directory that holds a graph.
//!
//! Every call but `init` names the repository and its work tree
//! (`--git-dir`, `--work-tree`), so git discovers nothing: a missing `.git` is git's
//! error, never the project's repository answering in its place. No
//! configuration of the system's or the user's is read, and the settings
//! of the repository's own that run code or change bytes are overridden
//! ([`OVERRIDES`]), so neither a caller nor a configuration can break a
//! commit or point it elsewhere. Commits are made as [`NAME`] with the
//! dates git reads from the clock.
//!
//! What reaches the index, and every blob of the object store, is `index`'s: files staged
//! as their bytes on disk, blobs written from a write's staged files, index
//! entries set from blob ids, every one through no filter.

mod index;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::{command, failed, finish, path, run, shown};

/// The author and committer of every commit: a program, at an address no
/// mailbox can have (`.invalid` is reserved for that).
const NAME: &str = "trurlic";
const EMAIL: &str = "trurlic@trurlic.invalid";

/// The branch a new repository starts on, whatever `init.defaultBranch`
/// says.
const BRANCH: &str = "main";

/// The empty file, for a setting or a variable that names a file or the
/// hooks directory.
#[cfg(unix)]
const NULL: &str = "/dev/null";
#[cfg(not(unix))]
const NULL: &str = "NUL";

/// Settings passed with `-c`, which beat the repository's own
/// configuration, the one the environment does not shut out. `{null}` is
/// [`NULL`].
const OVERRIDES: [&str; 9] = [
    // No hook runs, `.git/hooks/` included: `--no-verify` skips only
    // pre-commit and commit-msg, not prepare-commit-msg or post-commit.
    "core.hooksPath={null}",
    // A command `git status` would run.
    "core.fsmonitor=false",
    // Read from `$XDG_CONFIG_HOME/git/` when no configuration names them.
    "core.excludesFile={null}",
    "core.attributesFile={null}",
    // `git status` compares the bytes on disk with the index unconverted,
    // and an exec bit is no change: every graph file is committed 100644.
    "core.autocrlf=false",
    "core.fileMode=false",
    "commit.gpgSign=false",
    // Objects and refs reach the disk before the journal that names them
    // is written, and before the journal that would make the commit again
    // is deleted.
    "core.fsync=committed",
    // The loose objects of one `update-index`, and the trees of one
    // commit, reach the device with one hardware flush before any of them
    // gets its name (git 2.37: `bulk-checkin.c`, `flush_batch_fsync`); a
    // command with no object transaction, or a git before 2.37, which
    // warns and ignores the value, flushes each object on its own.
    "core.fsyncMethod=batch",
];

/// Set for every call. The system's and the user's configuration are read
/// from no file, and the identity is fixed: these variables beat every
/// configuration key that names an author or a committer.
const ENVIRONMENT: [(&str, &str); 6] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", NULL),
    ("GIT_AUTHOR_NAME", NAME),
    ("GIT_AUTHOR_EMAIL", EMAIL),
    ("GIT_COMMITTER_NAME", NAME),
    ("GIT_COMMITTER_EMAIL", EMAIL),
];

/// Removed for every call: a date the caller set would stand in for the
/// clock git reads.
const DATES: [&str; 2] = ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"];

/// The repository's `info/attributes`, which beats every `.gitattributes`
/// and `core.attributesFile`: no attribute that converts bytes applies, so
/// `update-index` writes, and `git status` and `git commit` read, the
/// bytes on disk as they are. With `text` unset, `eol` converts nothing
/// either; it is unset all the same.
const ATTRIBUTES: &str = "* -text -eol -filter -ident -working-tree-encoding\n";

/// The repository at `<work tree>/.git`, whose work tree is a graph
/// directory.
#[derive(Debug)]
pub(crate) struct Repository {
    work_tree: PathBuf,
    git_dir: PathBuf,
}

/// What `git status` says of the graph's files.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Status {
    /// The branch's commit; `None` in a new repository, which has none.
    pub(crate) head: Option<Oid>,
    /// The paths the pathspecs matched that differ between the work tree,
    /// the index and the last commit, or that no commit holds: modified,
    /// deleted, untracked and ignored ones alike, in git's order.
    pub(crate) changed: Vec<PathBuf>,
}

/// A git object name as git prints it: 40 lowercase hex digits, or 64 in
/// a SHA-256 repository. Checked when read, from git or from a journal,
/// so a name passed to git is never an option or a revision expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct Oid(String);

impl Oid {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let hex = std::str::from_utf8(bytes).ok()?;
        Self::try_from(hex.to_owned()).ok()
    }

    /// The null name of this one's length, which `--index-info` reads as
    /// "no such path".
    pub(crate) fn null(&self) -> Self {
        Self("0".repeat(self.0.len()))
    }

    fn is_null(&self) -> bool {
        self.0.bytes().all(|byte| byte == b'0')
    }

    fn as_os_str(&self) -> &OsStr {
        OsStr::new(&self.0)
    }
}

impl TryFrom<String> for Oid {
    type Error = &'static str;

    fn try_from(hex: String) -> std::result::Result<Self, &'static str> {
        let digits = hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if digits && matches!(hex.len(), 40 | 64) {
            Ok(Self(hex))
        } else {
            Err("an object name of 40 or 64 lowercase hex digits")
        }
    }
}

impl From<Oid> for String {
    fn from(oid: Oid) -> Self {
        oid.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What [`Repository::commit`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Committed {
    Made,
    /// The index matched the last commit, so there was nothing to commit.
    Nothing,
}

impl Repository {
    /// The repository of the graph directory `work_tree`, which may not
    /// exist yet.
    pub(crate) fn of(work_tree: &Path) -> Self {
        Self {
            work_tree: work_tree.to_path_buf(),
            git_dir: work_tree.join(".git"),
        }
    }

    pub(crate) fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The lock git holds while it writes the index. One that outlives its
    /// git process makes every later write of the index fail.
    pub(crate) fn index_lock(&self) -> PathBuf {
        self.git_dir.join("index.lock")
    }

    pub(crate) fn exists(&self) -> Result<bool> {
        self.git_dir.try_exists().map_err(Error::io(&self.git_dir))
    }

    /// Make the repository, empty, on [`BRANCH`], from no template, so no
    /// hook or exclude of a template comes with it, with [`ATTRIBUTES`];
    /// `excluded` are the lines of its `info/exclude`.
    pub(crate) fn create(&self, excluded: &[&str]) -> Result<()> {
        // In the work tree, with no `--git-dir`, which init would record
        // as the repository's work tree setting.
        let branch = format!("--initial-branch={BRANCH}");
        let args = [
            "init".as_ref(),
            "--quiet".as_ref(),
            "--template=".as_ref(),
            OsStr::new(&branch),
        ];
        run(isolated(&self.work_tree), &self.work_tree, &args, None)?;
        let info = self.git_dir.join("info");
        fs::create_dir_all(&info).map_err(Error::io(&info))?;
        let attributes = info.join("attributes");
        fs::write(&attributes, ATTRIBUTES).map_err(Error::io(&attributes))?;
        let exclude = info.join("exclude");
        let lines: String = excluded.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&exclude, lines).map_err(Error::io(&exclude))
    }

    /// The state of the files `pathspecs` match. It writes nothing: no
    /// index refresh is saved (`--no-optional-locks`), so a read can ask it
    /// and a leftover `index.lock` does not stop it.
    pub(crate) fn status(&self, pathspecs: &[&str]) -> Result<Status> {
        let mut args: Vec<&OsStr> = [
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignored",
            "--no-renames",
            "--",
        ]
        .iter()
        .map(OsStr::new)
        .collect();
        args.extend(pathspecs.iter().map(OsStr::new));
        let stdout = self.run(&args, None)?;
        parse_status(&stdout).ok_or_else(|| Error::GitOutput {
            args: shown(&args),
            detail: "a status record it cannot read",
        })
    }

    /// Commit the index with `message`, kept verbatim. An index that
    /// matches the last commit is [`Committed::Nothing`], not a failure.
    pub(crate) fn commit(&self, message: &str) -> Result<Committed> {
        let args = [
            "commit".as_ref(),
            "--quiet".as_ref(),
            "--no-verify".as_ref(),
            "--cleanup=verbatim".as_ref(),
            "-m".as_ref(),
            OsStr::new(message),
        ];
        let output = finish(self.command(), &args, None)?;
        if output.status.success() {
            return Ok(Committed::Made);
        }
        // Git refuses a commit that changes nothing with a status it shares
        // with other failures; the index, compared with the last commit,
        // tells which this was.
        let compare = [
            "diff".as_ref(),
            "--cached".as_ref(),
            "--quiet".as_ref(),
            "--no-ext-diff".as_ref(),
        ];
        let same = finish(self.command(), &compare, None)?;
        match same.status.code() {
            Some(0) => Ok(Committed::Nothing),
            // The index differs, or git cannot compare: the commit failed.
            _ => Err(failed(&self.work_tree, &args, &output)),
        }
    }

    /// The branch's commit, which must exist.
    pub(crate) fn head(&self) -> Result<Oid> {
        let args = ["rev-parse".as_ref(), "--verify".as_ref(), "HEAD".as_ref()];
        let stdout = self.run(&args, None)?;
        Oid::parse(stdout.strip_suffix(b"\n").unwrap_or(&stdout)).ok_or_else(|| Error::GitOutput {
            args: shown(&args),
            detail: "no object name",
        })
    }

    /// The first commit after `parent` on the first-parent line of the
    /// branch, oldest first; the line's root when `parent` is `None`.
    /// `None` when the branch is at `parent`.
    pub(crate) fn first_after(&self, parent: Option<&Oid>) -> Result<Option<Oid>> {
        let excluded = parent.map(|parent| format!("^{parent}"));
        let mut args: Vec<&OsStr> = ["rev-list", "--first-parent", "--reverse", "HEAD"]
            .iter()
            .map(OsStr::new)
            .collect();
        args.extend(excluded.as_deref().map(OsStr::new));
        let stdout = self.run(&args, None)?;
        match stdout.split(|&byte| byte == b'\n').next() {
            None | Some([]) => Ok(None),
            Some(line) => Oid::parse(line).map(Some).ok_or_else(|| Error::GitOutput {
                args: shown(&args),
                detail: "a line that is no object name",
            }),
        }
    }

    /// Whether the tree of `commit` holds each of `entries` as it names it:
    /// a path at its blob, and a path at the null name absent.
    pub(crate) fn holds(&self, commit: &Oid, entries: &[(&Path, &Oid)]) -> Result<bool> {
        let mut args: Vec<&OsStr> = ["--literal-pathspecs", "ls-tree", "-z", "--full-tree"]
            .iter()
            .map(OsStr::new)
            .collect();
        args.extend([commit.as_os_str(), OsStr::new("--")]);
        args.extend(entries.iter().map(|(path, _)| path.as_os_str()));
        let stdout = self.run(&args, None)?;
        let held = stdout
            .split(|&byte| byte == 0)
            .filter(|record| !record.is_empty())
            .map(tree_entry)
            .collect::<Option<Vec<(PathBuf, Oid)>>>()
            .ok_or_else(|| Error::GitOutput {
                args: shown(&args),
                detail: "a tree entry it cannot read",
            })?;
        Ok(entries.iter().all(|&(path, blob)| {
            let at = held.iter().find(|(held, _)| held == path).map(|(_, id)| id);
            if blob.is_null() {
                at.is_none()
            } else {
                at == Some(blob)
            }
        }))
    }

    /// Those of `oids` the repository holds no object of.
    pub(crate) fn missing<'o>(&self, oids: &[&'o Oid]) -> Result<BTreeSet<&'o Oid>> {
        let input: Vec<u8> = oids
            .iter()
            .flat_map(|oid| oid.0.bytes().chain(*b"\n"))
            .collect();
        let args = ["cat-file".as_ref(), "--batch-check".as_ref()];
        let stdout = self.run(&args, Some(&input))?;
        let answers: Vec<&[u8]> = stdout
            .split(|&byte| byte == b'\n')
            .filter(|answer| !answer.is_empty())
            .collect();
        if answers.len() != oids.len() {
            return Err(Error::GitOutput {
                args: shown(&args),
                detail: "a different number of answers than object names",
            });
        }
        Ok(oids
            .iter()
            .zip(answers)
            .filter(|(_, answer)| answer.ends_with(b" missing"))
            .map(|(&oid, _)| oid)
            .collect())
    }

    /// The stdout of `git <args>` in this repository, which must exit with
    /// status 0.
    fn run(&self, args: &[&OsStr], input: Option<&[u8]>) -> Result<Vec<u8>> {
        run(self.command(), &self.work_tree, args, input)
    }

    /// [`isolated`] git, pointed at this repository and its work tree.
    fn command(&self) -> Command {
        self.command_in(&self.work_tree)
    }

    /// [`isolated`] git in `dir`, pointed at this repository with `dir` as
    /// its work tree.
    fn command_in(&self, dir: &Path) -> Command {
        let mut command = isolated(dir);
        command
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(dir);
        command
    }
}

/// `git -C <dir>` reading no system or user configuration, with
/// [`OVERRIDES`], [`ENVIRONMENT`], and no [`DATES`].
fn isolated(dir: &Path) -> Command {
    let mut command = command(dir);
    for setting in OVERRIDES {
        command.arg("-c").arg(setting.replace("{null}", NULL));
    }
    command.envs(ENVIRONMENT);
    for variable in DATES {
        command.env_remove(variable);
    }
    command
}

/// One record of `ls-tree -z`: `<mode> <type> <object>\t<path>`.
fn tree_entry(record: &[u8]) -> Option<(PathBuf, Oid)> {
    let tab = record.iter().position(|&byte| byte == b'\t')?;
    let (fields, name) = (record.get(..tab)?, record.get(tab + 1..)?);
    let object = fields.rsplit(|&byte| byte == b' ').next()?;
    Some((path(name), Oid::parse(object)?))
}

/// The records of `status --porcelain=v2 --branch -z`; `None` when one is
/// of a kind or shape this reader does not know.
fn parse_status(stdout: &[u8]) -> Option<Status> {
    let mut status = Status {
        head: None,
        changed: Vec::new(),
    };
    let mut records = stdout.split(|&byte| byte == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let (kind, rest) = record.split_first()?;
        let changed = match kind {
            b'#' => {
                if let Some(oid) = rest.strip_prefix(b" branch.oid ") {
                    status.head = match oid {
                        b"(initial)" => None,
                        oid => Some(Oid::parse(oid)?),
                    };
                }
                continue;
            }
            b'?' | b'!' => rest.strip_prefix(b" ")?,
            // XY, submodule state, three modes and two object names.
            b'1' => after_fields(rest, 7)?,
            // A rename, which `--no-renames` rules out: a score follows the
            // names, and the source path is a record of its own.
            b'2' => {
                records.next()?;
                after_fields(rest, 8)?
            }
            // An unmerged path: XY, submodule state, four modes, three names.
            b'u' => after_fields(rest, 9)?,
            _ => return None,
        };
        status.changed.push(path(changed));
    }
    Some(status)
}

/// What follows the first `n` fields of `rest`, which starts with a space:
/// the path, spaces and all.
fn after_fields(rest: &[u8], n: usize) -> Option<&[u8]> {
    let mut fields = rest.strip_prefix(b" ")?.splitn(n + 1, |&byte| byte == b' ');
    fields.nth(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A repository made by [`Repository::create`] in a new directory.
    fn created() -> (TempDir, Repository) {
        let tmp = TempDir::new().unwrap();
        let repository = Repository::of(tmp.path());
        repository.create(&["/.state/"]).unwrap();
        (tmp, repository)
    }

    /// `git <args>` in the repository, with no configuration of the user's.
    fn git(repository: &Repository, args: &[&str]) -> String {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        String::from_utf8(repository.run(&args, None).unwrap()).unwrap()
    }

    const GRAPH: [&str; 2] = [":(glob)*.toml", ":(glob)decisions/*.toml"];

    #[test]
    fn a_new_repository_is_empty_on_main_and_ignores_what_it_is_told() {
        let (tmp, repository) = created();
        fs::create_dir_all(tmp.path().join(".state")).unwrap();
        fs::write(tmp.path().join(".state/lock"), "1").unwrap();

        assert!(repository.exists().unwrap());
        assert!(!repository.git_dir().join("hooks").exists());
        assert_eq!(
            git(&repository, &["symbolic-ref", "HEAD"]),
            "refs/heads/main\n"
        );
        assert_eq!(
            git(
                &repository,
                &["status", "--porcelain", "--untracked-files=all"]
            ),
            ""
        );
        assert_eq!(
            repository.status(&GRAPH).unwrap(),
            Status {
                head: None,
                changed: vec![]
            }
        );
    }

    #[test]
    fn status_lists_every_kind_of_change_the_pathspecs_match_only() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("decisions/sub")).unwrap();
        for file in [
            "kept.toml",
            "edited.toml",
            "gone.toml",
            "decisions/a b.toml",
        ] {
            fs::write(dir.join(file), file).unwrap();
        }
        repository
            .stage(&["kept.toml".into(), "edited.toml".into(), "gone.toml".into()])
            .unwrap();
        repository.commit("first").unwrap();
        fs::write(dir.join("edited.toml"), "edited").unwrap();
        fs::remove_file(dir.join("gone.toml")).unwrap();
        fs::write(dir.join(".gitignore"), "ignored.toml\n").unwrap();
        fs::write(dir.join("ignored.toml"), "").unwrap();
        fs::write(dir.join("decisions/sub/deep.toml"), "").unwrap();
        fs::write(dir.join("notes.md"), "").unwrap();

        let status = repository.status(&GRAPH).unwrap();

        assert!(status.head.is_some());
        let mut changed = status.changed;
        changed.sort();
        assert_eq!(
            changed,
            [
                PathBuf::from("decisions/a b.toml"),
                PathBuf::from("edited.toml"),
                PathBuf::from("gone.toml"),
                PathBuf::from("ignored.toml"),
            ]
        );
    }

    /// `dir` with a `.gitattributes` that converts every file, through a
    /// filter whose failure fails the command that runs it.
    fn converting(repository: &Repository, dir: &Path) {
        fs::write(
            dir.join(".gitattributes"),
            "* text eol=lf ident filter=broken\n",
        )
        .unwrap();
        git(repository, &["config", "filter.broken.clean", "false"]);
        git(repository, &["config", "filter.broken.required", "true"]);
    }

    /// Without the repository's own attributes, staging writes them again
    /// and takes the bytes as they are: the filter a `.gitattributes`
    /// names, which fails, never runs.
    #[test]
    fn stage_takes_the_bytes_on_disk_through_no_filter() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        converting(&repository, dir);
        fs::remove_file(repository.git_dir().join("info/attributes")).unwrap();
        let bytes = "line one\r\nline two $Id$\n";
        fs::write(dir.join("a.toml"), bytes).unwrap();

        repository.stage(&["a.toml".into()]).unwrap();

        assert_eq!(git(&repository, &["cat-file", "blob", ":a.toml"]), bytes);
    }

    /// The id and the bytes of the blob `write_blobs` writes for one staged
    /// file of `bytes` in `dir/.state/tmp/`, whose own `.gitattributes`
    /// holds `attributes`.
    fn blob_of_staged(
        repository: &Repository,
        dir: &Path,
        attributes: &str,
        bytes: &[u8],
    ) -> (Oid, Vec<u8>) {
        let tmp = dir.join(".state/tmp");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join(".gitattributes"), attributes).unwrap();
        fs::write(tmp.join("4242-0"), bytes).unwrap();
        let index = tmp.join("4242-1");

        let ids = repository
            .write_blobs(&tmp, &[OsStr::new("4242-0")], &index)
            .unwrap();

        let [id] = ids.as_slice() else {
            panic!("{ids:?}");
        };
        let args = ["cat-file".as_ref(), "blob".as_ref(), id.as_os_str()];
        let blob = repository.run(&args, None).unwrap();
        (id.clone(), blob)
    }

    /// Each attribute that converts what is staged or written as a blob,
    /// asked for by a `.gitattributes` in the work tree and in the staging
    /// directory, and the repository's own attributes edited away first:
    /// staging and writing a blob restore them and take the bytes on disk.
    /// Each case fails when its attribute is not unset.
    #[test]
    fn stage_and_blobs_unset_every_converting_attribute_whatever_info_attributes_held() {
        let utf16 = b"\xff\xfea\x00\n\x00".as_slice();
        let cases: [(&str, &[u8]); 5] = [
            ("* text\n", b"crlf\r\nline\r\n"),
            ("* eol=lf\n", b"crlf\r\n"),
            ("* ident\n", b"$Id: 0123456789abcdef $\n"),
            ("* filter=upper\n", b"lower case\n"),
            ("* working-tree-encoding=UTF-16\n", utf16),
        ];
        for (attributes, bytes) in cases {
            let (tmp, repository) = created();
            let dir = tmp.path();
            fs::write(dir.join(".gitattributes"), attributes).unwrap();
            git(&repository, &["config", "filter.upper.clean", "tr a-z A-Z"]);
            let info = repository.git_dir().join("info/attributes");
            fs::write(&info, "* text ident\n").unwrap();
            fs::write(dir.join("a.toml"), bytes).unwrap();

            repository.stage(&["a.toml".into()]).unwrap();

            let blob = repository
                .run(
                    &["cat-file".as_ref(), "blob".as_ref(), ":a.toml".as_ref()],
                    None,
                )
                .unwrap();
            assert_eq!(blob, bytes, "{attributes}");
            assert_eq!(fs::read_to_string(&info).unwrap(), ATTRIBUTES);

            fs::write(&info, "* text ident\n").unwrap();
            let (_, blob) = blob_of_staged(&repository, dir, attributes, bytes);
            assert_eq!(blob, bytes, "a blob, {attributes}");
            assert_eq!(fs::read_to_string(&info).unwrap(), ATTRIBUTES);
        }
    }

    /// Blobs are written from staged files through no filter, whatever
    /// filter a `.gitattributes` names, and read back by their ids; the
    /// repository's index stays as it was.
    #[test]
    fn blobs_are_written_through_no_filter_and_leave_the_index_alone() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        converting(&repository, dir);
        fs::remove_file(repository.git_dir().join("info/attributes")).unwrap();
        let bytes = b"line one\r\nline two $Id$\n";

        let attributes = "* text eol=lf ident filter=broken\n";
        let (_, blob) = blob_of_staged(&repository, dir, attributes, bytes);

        assert_eq!(blob, bytes);
        assert_eq!(git(&repository, &["ls-files"]), "");
        assert!(!repository.git_dir().join("index").exists());
    }

    /// An index set from blob ids holds them whatever the work tree holds,
    /// and the null name takes a path out.
    #[test]
    fn set_index_puts_each_path_at_its_blob_and_takes_a_null_one_out() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        fs::write(dir.join("a.toml"), "a").unwrap();
        fs::write(dir.join("gone.toml"), "gone").unwrap();
        repository
            .stage(&["a.toml".into(), "gone.toml".into()])
            .unwrap();
        repository.commit("first").unwrap();
        let parent = repository.head().unwrap();
        let (id, _) = blob_of_staged(&repository, dir, "", b"the write's bytes");
        fs::write(dir.join("a.toml"), "a hand edit").unwrap();

        let null = parent.null();
        let entries = [(Path::new("a.toml"), &id), (Path::new("gone.toml"), &null)];
        repository.set_index(&entries).unwrap();
        repository.commit("second").unwrap();

        assert_eq!(
            git(&repository, &["show", "HEAD:a.toml"]),
            "the write's bytes"
        );
        assert_eq!(
            git(&repository, &["ls-tree", "--name-only", "HEAD"]),
            "a.toml\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("a.toml")).unwrap(),
            "a hand edit"
        );
        let second = repository.head().unwrap();
        assert!(repository.holds(&second, &entries).unwrap());
        assert!(!repository.holds(&parent, &entries).unwrap());
        assert_eq!(
            repository.first_after(Some(&parent)).unwrap(),
            Some(second.clone())
        );
        assert_eq!(repository.first_after(Some(&second)).unwrap(), None);
        assert_eq!(repository.first_after(None).unwrap(), Some(parent.clone()));
        let absent = Oid::try_from("1".repeat(40)).unwrap();
        assert_eq!(
            repository.missing(&[&id, &absent, &parent]).unwrap(),
            BTreeSet::from([&absent])
        );
    }

    #[test]
    fn an_object_name_is_40_or_64_lowercase_hex_digits() {
        for good in ["a".repeat(40), "0123456789abcdef".repeat(4)] {
            assert!(Oid::try_from(good.clone()).is_ok(), "{good}");
        }
        for bad in [
            "a".repeat(39),
            "A".repeat(40),
            "g".repeat(40),
            format!("-{}", "a".repeat(39)),
            String::new(),
        ] {
            assert!(Oid::try_from(bad.clone()).is_err(), "{bad}");
        }
        let oid = Oid::try_from("ab".repeat(20)).unwrap();
        assert_eq!(oid.null().to_string(), "0".repeat(40));
        assert!(oid.null().is_null() && !oid.is_null());
    }

    /// A symlinked node file is staged as the bytes a load reads through
    /// it, not as the link.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_staged_as_the_bytes_it_leads_to() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        fs::write(dir.join("target.md"), "the bytes\n").unwrap();
        std::os::unix::fs::symlink("target.md", dir.join("a.toml")).unwrap();
        fs::write(dir.join("b.toml"), "plain\n").unwrap();

        repository
            .stage(&["a.toml".into(), "b.toml".into()])
            .unwrap();

        assert_eq!(
            git(&repository, &["cat-file", "blob", ":a.toml"]),
            "the bytes\n"
        );
        assert_eq!(
            git(&repository, &["cat-file", "blob", ":b.toml"]),
            "plain\n"
        );
        assert_eq!(
            git(&repository, &["ls-files", "--stage", "a.toml"])
                .split(' ')
                .next(),
            Some("100644")
        );
    }

    /// The repository's own attributes beat a `.gitattributes`, so status
    /// compares the bytes on disk unconverted, and neither status nor the
    /// commit, which checks the files it stages again, runs a filter.
    #[test]
    fn status_and_commit_run_no_filter_a_gitattributes_names() {
        let (tmp, repository) = created();
        let dir = tmp.path();
        fs::write(dir.join("a.toml"), "a\n").unwrap();
        repository.stage(&["a.toml".into()]).unwrap();
        repository.commit("first").unwrap();
        converting(&repository, dir);
        fs::write(dir.join("a.toml"), "a\r\n").unwrap();

        assert_eq!(
            repository.status(&GRAPH).unwrap().changed,
            [PathBuf::from("a.toml")]
        );
        repository.stage(&["a.toml".into()]).unwrap();
        assert_eq!(repository.commit("crlf").unwrap(), Committed::Made);
        assert_eq!(
            git(&repository, &["cat-file", "blob", "HEAD:a.toml"]),
            "a\r\n"
        );
    }

    #[test]
    fn stage_removes_a_deleted_path_and_ignores_one_never_held() {
        let (tmp, repository) = created();
        fs::write(tmp.path().join("a.toml"), "a").unwrap();
        repository.stage(&["a.toml".into()]).unwrap();
        repository.commit("first").unwrap();
        fs::remove_file(tmp.path().join("a.toml")).unwrap();

        repository
            .stage(&["a.toml".into(), "never.toml".into()])
            .unwrap();
        repository.commit("second").unwrap();

        assert_eq!(git(&repository, &["ls-tree", "--name-only", "HEAD"]), "");
    }

    #[test]
    fn a_commit_of_nothing_new_is_no_failure_and_makes_no_commit() {
        let (tmp, repository) = created();
        fs::write(tmp.path().join("a.toml"), "a").unwrap();
        repository.stage(&["a.toml".into()]).unwrap();
        repository.commit("first").unwrap();

        repository.stage(&["a.toml".into()]).unwrap();

        assert_eq!(repository.commit("again").unwrap(), Committed::Nothing);
        assert_eq!(git(&repository, &["rev-list", "--count", "HEAD"]), "1\n");
    }

    #[test]
    fn a_commit_is_the_fixed_identity_with_the_message_verbatim() {
        let (tmp, repository) = created();
        fs::write(tmp.path().join("a.toml"), "a").unwrap();
        repository.stage(&["a.toml".into()]).unwrap();

        repository.commit("fmt\n\n# kept\nOrigin: x\n").unwrap();

        assert_eq!(
            git(&repository, &["log", "--format=%an <%ae>%n%cn <%ce>%n%B"]),
            "trurlic <trurlic@trurlic.invalid>\ntrurlic <trurlic@trurlic.invalid>\n\
             fmt\n\n# kept\nOrigin: x\n\n"
        );
    }

    #[test]
    fn a_missing_repository_is_a_failure_not_another_repository() {
        let tmp = TempDir::new().unwrap();
        let outer = Repository::of(tmp.path());
        outer.create(&[]).unwrap();
        let graph = tmp.path().join(".trurlic");
        fs::create_dir_all(&graph).unwrap();

        let err = Repository::of(&graph).status(&["."]).unwrap_err();

        assert!(matches!(err, Error::GitFailed { .. }), "{err:?}");
    }

    #[test]
    fn status_records_of_every_kind_are_read_and_unknown_ones_refused() {
        let stdout = b"# branch.oid (initial)\0# branch.head main\0\
            1 .M N... 100644 100644 100644 aa bb sp ace.toml\0\
            2 R. N... 100644 100644 100644 aa bb R100 new.toml\0old.toml\0\
            u UU N... 100644 100644 100644 100644 aa bb cc both.toml\0\
            ? new\nline.toml\0! ignored.toml\0";

        assert_eq!(
            parse_status(stdout).unwrap(),
            Status {
                head: None,
                changed: [
                    "sp ace.toml",
                    "new.toml",
                    "both.toml",
                    "new\nline.toml",
                    "ignored.toml"
                ]
                .map(PathBuf::from)
                .to_vec(),
            }
        );
        let oid = "01ab".repeat(10);
        let born = format!("# branch.oid {oid}\0");
        assert_eq!(
            parse_status(born.as_bytes()).unwrap().head,
            Some(Oid::try_from(oid).unwrap())
        );
        assert_eq!(parse_status(b"# branch.oid 01ab\0"), None);
        assert_eq!(parse_status(b"x what\0"), None);
        assert_eq!(parse_status(b"1 .M short\0"), None);
    }
}
