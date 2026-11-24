//! The graph's own repository: the git repository at `.trurlic/.git` that
//! holds a shared graph's history, which only trurlic writes. This is the
//! one module that runs git inside a graph directory; discovery and the
//! placement facts ask git only about the directory that holds a graph.
//!
//! Every call names the repository and its work tree (`--git-dir`,
//! `--work-tree`), so git discovers nothing: a missing `.git` is git's
//! error, never the project's repository answering in its place. No
//! configuration of the system's or the user's is read, and the settings
//! of the repository's own that run code or change bytes are overridden
//! ([`OVERRIDES`]), so neither a caller nor a configuration can break a
//! commit or point it elsewhere. Files are staged as the bytes on disk,
//! through no filter, and committed as [`NAME`] with the dates git reads
//! from the clock.
//!
//! Staging is one `update-index` over every path, which writes its objects
//! in one batched flush ([`OVERRIDES`]); it runs the conversions the
//! attributes ask for, and the repository's own [`ATTRIBUTES`], checked
//! before every staging, unset every one of them.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{Error, Result};

use super::{command, failed, finish, path, path_bytes, shown};

/// The author and committer of every commit: a program, at an address no
/// mailbox can have (`.invalid` is reserved for that).
const NAME: &str = "trurlic";
const EMAIL: &str = "trurlic@trurlic.invalid";

/// The branch a new repository starts on, whatever `init.defaultBranch`
/// says.
const BRANCH: &str = "main";

/// The empty file, for a setting that names a file to read.
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
    // Objects and refs reach the disk before the journal that would make
    // the commit again is deleted.
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
/// `update-index` stages, and `git status` and `git commit` read, the
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
    /// Whether the branch has a commit; a new repository's has none.
    pub(crate) born: bool,
    /// The paths the pathspecs matched that differ between the work tree,
    /// the index and the last commit, or that no commit holds: modified,
    /// deleted, untracked and ignored ones alike, in git's order.
    pub(crate) changed: Vec<PathBuf>,
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
        self.check(finish(isolated(&self.work_tree), &args, None)?, &args)?;
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

    /// Put `paths`, relative to the work tree, in the index as they are on
    /// disk: each file's bytes as they are, through no filter or line-end
    /// conversion; a path with no file is taken out, and one the index
    /// never held is no error.
    pub(crate) fn stage(&self, paths: &[PathBuf]) -> Result<()> {
        self.unset_conversions()?;
        let mut files = Vec::with_capacity(paths.len());
        let mut links = Vec::new();
        for relative in paths {
            let full = self.work_tree.join(relative);
            match fs::symlink_metadata(&full) {
                Ok(metadata) if metadata.file_type().is_symlink() => links.push(relative),
                Ok(_) => files.push(relative),
                Err(e) if e.kind() == ErrorKind::NotFound => files.push(relative),
                Err(source) => return Err(Error::Io { path: full, source }),
            }
        }
        if !files.is_empty() {
            self.update(&files)?;
        }
        if !links.is_empty() {
            self.add_through(&links)?;
        }
        Ok(())
    }

    /// Write [`ATTRIBUTES`] to the repository's `info/attributes` when it
    /// holds anything else, so no attribute converts what [`update`]
    /// stages, whatever a person did to the file.
    ///
    /// [`update`]: Self::update
    fn unset_conversions(&self) -> Result<()> {
        let attributes = self.git_dir.join("info").join("attributes");
        // Neither read nor written when it is no regular file: either one
        // would wait on a FIFO for its other end forever.
        match fs::metadata(&attributes) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(Error::NotAFile { path: attributes });
            }
            Ok(_) | Err(_) => {}
        }
        match fs::read(&attributes) {
            Ok(held) if held == ATTRIBUTES.as_bytes() => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    path: attributes,
                    source,
                });
            }
        }
        let info = self.git_dir.join("info");
        fs::create_dir_all(&info).map_err(Error::io(&info))?;
        fs::write(&attributes, ATTRIBUTES).map_err(Error::io(&attributes))
    }

    /// Stage the files and the missing paths of `paths` in one process,
    /// in one object transaction: each file as its bytes, each missing
    /// path taken out.
    fn update(&self, paths: &[&PathBuf]) -> Result<()> {
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|path| path_bytes(path).into_iter().chain([0]))
            .collect();
        let args = [
            "update-index".as_ref(),
            "-z".as_ref(),
            "--add".as_ref(),
            "--remove".as_ref(),
            "--stdin".as_ref(),
        ];
        self.run(&args, Some(&input)).map(drop)
    }

    /// Stage each of `links`, symlinks, as the bytes of the file it leads
    /// to, which a load reads, through no filter: `update-index` would
    /// stage the link itself.
    fn add_through(&self, links: &[&PathBuf]) -> Result<()> {
        let mut args: Vec<&OsStr> = ["hash-object", "-w", "--no-filters", "--"]
            .iter()
            .map(OsStr::new)
            .collect();
        args.extend(links.iter().map(|path| path.as_os_str()));
        let stdout = self.run(&args, None)?;
        let info = index_info(&stdout, links).ok_or_else(|| Error::GitOutput {
            args: "hash-object -w --no-filters".into(),
            detail: "a different number of object names than paths",
        })?;
        let args = [
            "update-index".as_ref(),
            "-z".as_ref(),
            "--index-info".as_ref(),
        ];
        self.run(&args, Some(&info)).map(drop)
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

    /// The stdout of `git <args>` in this repository, which must exit with
    /// status 0.
    fn run(&self, args: &[&OsStr], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let output = finish(self.command(), args, input)?;
        self.check(output, args)
    }

    fn check(&self, output: Output, args: &[&OsStr]) -> Result<Vec<u8>> {
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(failed(&self.work_tree, args, &output))
        }
    }

    /// [`isolated`] git, pointed at this repository and its work tree.
    fn command(&self) -> Command {
        let mut command = isolated(&self.work_tree);
        command
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree);
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

/// The `update-index -z --index-info` lines that put each of `paths` at the
/// object `hash-object` named for it, one name per line in `stdout`;
/// `None` when the counts differ.
fn index_info(stdout: &[u8], paths: &[&PathBuf]) -> Option<Vec<u8>> {
    let oids: Vec<&[u8]> = stdout
        .split(|&byte| byte == b'\n')
        .filter(|oid| !oid.is_empty())
        .collect();
    if oids.len() != paths.len() {
        return None;
    }
    let mut info = Vec::new();
    for (oid, path) in oids.into_iter().zip(paths) {
        info.extend_from_slice(b"100644 ");
        info.extend_from_slice(oid);
        info.push(b'\t');
        info.extend(path_bytes(path));
        info.push(0);
    }
    Some(info)
}

/// The records of `status --porcelain=v2 --branch -z`; `None` when one is
/// of a kind or shape this reader does not know.
fn parse_status(stdout: &[u8]) -> Option<Status> {
    let mut status = Status {
        born: true,
        changed: Vec::new(),
    };
    let mut records = stdout.split(|&byte| byte == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let (kind, rest) = record.split_first()?;
        let changed = match kind {
            b'#' => {
                if rest == b" branch.oid (initial)" {
                    status.born = false;
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
                born: false,
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

        assert!(status.born);
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

    /// Each attribute that converts what is staged, asked for by a
    /// `.gitattributes` in the work tree, and the repository's own
    /// attributes edited away first: staging restores them and takes the
    /// bytes on disk. Each case fails when its attribute is not unset.
    #[test]
    fn stage_unsets_every_converting_attribute_whatever_info_attributes_held() {
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
        }
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
                born: false,
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
        assert!(parse_status(b"# branch.oid 01ab\0").unwrap().born);
        assert_eq!(parse_status(b"x what\0"), None);
        assert_eq!(parse_status(b"1 .M short\0"), None);
    }
}
