//! The history step of the writer protocol. Under `shared` placement the
//! graph directory is a git repository of its own (`git::graph`), and each
//! write ends with one commit to it, whose message names the tool and whose
//! trailers carry `Origin:` and `Session:`. Under `tracked` the project's
//! git holds the history, and trurlic commits nothing.
//!
//! Before a write reaches its commit point, the graph files the last commit
//! does not hold as they are on disk are committed as `hand edit`, so the
//! write's commit holds the write alone; the first commit of a repository
//! holds every graph file. The message travels in the journal, so the
//! opener that finishes an interrupted write makes its commit. A commit is
//! made by content: the write's files are staged as they are on disk, and
//! an index that then matches the last commit means the commit is in the
//! history already, made before the stop or by hand since, and none is made
//! again.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::console::diag;
use crate::git::graph::{Committed, Repository, Status};
use crate::{Error, Result};

use super::Store;
use super::lock::StoreLock;
use super::placement::listed;
use super::schema::{COMPONENTS_DIR, DECISIONS_DIR, PROJECT_FILE, Placement, STATE_DIR};
use super::text::{TextField, fault};

/// The subject of the commit of graph files no tool wrote.
const HAND_EDIT: &str = "hand edit";

/// The writes that commit to the graph's history, named by its subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Init,
    Fmt,
    Migrate,
    Apply,
}

impl Tool {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Fmt => "fmt",
            Self::Migrate => "migrate",
            Self::Apply => "apply",
        }
    }
}

/// The message of a write's history commit: the tool as its subject, and
/// the `Origin:` and `Session:` trailers the caller passed, each one line.
/// The journal carries it, checked again when it is read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MessageFields")]
pub struct CommitMessage {
    tool: Tool,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageFields {
    tool: Tool,
    origin: Option<String>,
    session: Option<String>,
}

impl TryFrom<MessageFields> for CommitMessage {
    type Error = Error;

    fn try_from(fields: MessageFields) -> Result<Self> {
        Self::new(fields.tool, fields.origin, fields.session)
    }
}

impl CommitMessage {
    /// Fails when the origin or the session breaks the one-line rule of
    /// `store::text`: a line break in either would forge a trailer.
    pub fn new(tool: Tool, origin: Option<String>, session: Option<String>) -> Result<Self> {
        for (field, value) in [(TextField::Origin, &origin), (TextField::Session, &session)] {
            if let Some(fault) = value.as_deref().and_then(|text| fault(field, text)) {
                return Err(Error::TrailerText { field, fault });
            }
        }
        Ok(Self {
            tool,
            origin,
            session,
        })
    }

    /// The message of a write that carries no trailer, such as a command's.
    #[must_use]
    pub const fn of(tool: Tool) -> Self {
        Self {
            tool,
            origin: None,
            session: None,
        }
    }

    /// The commit message: the subject, then the trailers the write has,
    /// as a paragraph of their own.
    #[must_use]
    pub fn render(&self) -> String {
        let mut message = self.tool.as_str().to_owned();
        let trailers = [("Origin", &self.origin), ("Session", &self.session)];
        let mut first = true;
        for (key, value) in trailers {
            if let Some(value) = value {
                message.push_str(if first { "\n\n" } else { "\n" });
                first = false;
                message.push_str(&format!("{key}: {value}"));
            }
        }
        message.push('\n');
        message
    }
}

/// Where a commit's history goes, by the graph's placement.
#[derive(Debug, Clone, Copy)]
pub(super) enum History<'m> {
    /// `tracked`: the project's git holds the history.
    Project,
    /// `shared`: a commit to the graph's own repository.
    Graph(&'m CommitMessage),
}

impl<'m> History<'m> {
    pub(super) fn of(placement: Placement, message: &'m CommitMessage) -> Self {
        match placement {
            Placement::Tracked => Self::Project,
            Placement::Shared => Self::Graph(message),
        }
    }
}

/// The repository's branch, as the history step found it before the
/// commit point.
#[derive(Debug)]
pub(super) enum Head {
    Born,
    /// No commit yet: `files` are the graph files on disk then, which the
    /// first commit holds with the write's.
    Unborn {
        files: Vec<PathBuf>,
    },
}

/// What `status` and `check` report of a shared graph's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryFinding {
    /// No repository holds the history; the next write makes one.
    Missing { repository: PathBuf },
    /// A lock git left on the index, which refuses every write until it is
    /// gone.
    IndexLocked { lock: PathBuf },
    /// Graph files the last commit does not hold as they are on disk,
    /// relative to the graph directory, sorted.
    Uncommitted { graph: PathBuf, files: Vec<PathBuf> },
}

impl HistoryFinding {
    /// Whether `check` fails on it: an index lock refuses every write, and
    /// the next write mends the others.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        match self {
            Self::IndexLocked { .. } => true,
            Self::Missing { .. } | Self::Uncommitted { .. } => false,
        }
    }
}

impl fmt::Display for HistoryFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { repository } => write!(
                f,
                "{}: no git repository holds the history of this shared graph; the next \
                 write makes one",
                repository.display()
            ),
            Self::IndexLocked { lock } => write!(
                f,
                "{}: git holds the index of the graph's repository, or a git process that \
                 stopped left this lock; every write is refused until it is gone: remove it \
                 once no git process runs there",
                lock.display()
            ),
            Self::Uncommitted { graph, files } => {
                let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
                write!(
                    f,
                    "{}: {} file(s) differ from the last commit of the graph's history: {}; \
                     `trurlic fmt`, or the next write, commits them as `{HAND_EDIT}`",
                    graph.display(),
                    files.len(),
                    listed(&names)
                )
            }
        }
    }
}

/// The graph's files as pathspecs relative to the graph directory, one
/// level deep as a load reads them; `.state/` and any other file are never
/// matched.
fn graph_files() -> [String; 3] {
    [
        format!(":(glob){PROJECT_FILE}"),
        format!(":(glob){COMPONENTS_DIR}/*.toml"),
        format!(":(glob){DECISIONS_DIR}/*.toml"),
    ]
}

/// What git says of the graph files in `repository`.
fn graph_status(repository: &Repository) -> Result<Status> {
    let specs = graph_files();
    repository.status(&specs.each_ref().map(String::as_str))
}

impl Store {
    fn repository(&self) -> Repository {
        Repository::of(&self.root)
    }

    /// Make the graph's repository, with `.state/` in its `info/exclude`.
    pub(super) fn create_repository(&self, _lock: &StoreLock) -> Result<()> {
        self.repository().create(&[&format!("/{STATE_DIR}/")])
    }

    /// Make the graph's repository when there is none.
    pub(super) fn ensure_repository(&self, lock: &StoreLock) -> Result<()> {
        if self.repository().exists()? {
            return Ok(());
        }
        self.create_repository(lock)
    }

    /// Make the repository of a write's graph again when it is gone, and
    /// say so: its history starts with this write.
    fn make_again(&self, lock: &StoreLock, repository: &Repository) -> Result<()> {
        if repository.exists()? {
            return Ok(());
        }
        self.create_repository(lock)?;
        diag!(
            "warning: {}: no git repository held the history of this shared graph; made \
             one, whose history starts with this write",
            repository.git_dir().display()
        );
        Ok(())
    }

    /// The first commit of a new graph's repository, holding `project.toml`.
    pub(super) fn commit_init(&self, _lock: &StoreLock) -> Result<()> {
        let repository = self.repository();
        repository.stage(&[PathBuf::from(PROJECT_FILE)])?;
        repository.commit(&CommitMessage::of(Tool::Init).render())?;
        Ok(())
    }

    /// The history step before the commit point: the repository made again
    /// when it is gone, a leftover index lock refused, and every changed
    /// graph file committed as `hand edit`. A failure here leaves the
    /// graph as it was.
    pub(super) fn prepare_history(&self, lock: &StoreLock) -> Result<Head> {
        let repository = self.repository();
        self.make_again(lock, &repository)?;
        let index_lock = repository.index_lock();
        if index_lock.try_exists().map_err(Error::io(&index_lock))? {
            return Err(Error::GraphIndexLocked { lock: index_lock });
        }
        let status = graph_status(&repository)?;
        if !status.born {
            return Ok(Head::Unborn {
                files: status.changed,
            });
        }
        if !status.changed.is_empty() {
            repository.stage(&status.changed)?;
            repository.commit(&format!("{HAND_EDIT}\n"))?;
        }
        Ok(Head::Born)
    }

    /// The history step after the files are in place: `paths`, relative to
    /// the graph directory, committed with `message`. `head` is what
    /// [`prepare_history`](Self::prepare_history) found, or `None` when an
    /// opener finishes a stopped write, which asks the repository again.
    pub(super) fn commit_history(
        &self,
        lock: &StoreLock,
        message: &CommitMessage,
        paths: &[PathBuf],
        head: Option<Head>,
    ) -> Result<Committed> {
        let repository = self.repository();
        let head = match head {
            Some(head) => head,
            None => self.head_now(lock, &repository)?,
        };
        let staged: Cow<'_, [PathBuf]> = match head {
            Head::Born => Cow::Borrowed(paths),
            Head::Unborn { files } => {
                // clone: the first commit's set owns the write's paths too.
                let all: BTreeSet<PathBuf> =
                    files.into_iter().chain(paths.iter().cloned()).collect();
                Cow::Owned(all.into_iter().collect())
            }
        };
        repository.stage(&staged)?;
        repository.commit(&message.render())
    }

    /// The branch as it is, for an opener that finishes a stopped write.
    fn head_now(&self, lock: &StoreLock, repository: &Repository) -> Result<Head> {
        self.make_again(lock, repository)?;
        let status = graph_status(repository)?;
        Ok(if status.born {
            Head::Born
        } else {
            Head::Unborn {
                files: status.changed,
            }
        })
    }

    /// What `status` and `check` report of the graph's history: nothing
    /// under `tracked`. Under `shared` it asks git without writing to the
    /// repository, and under the shared lock, so no write is halfway.
    pub fn history_findings(&self, placement: Placement) -> Result<Vec<HistoryFinding>> {
        if placement == Placement::Tracked {
            return Ok(Vec::new());
        }
        let repository = self.repository();
        if !repository.exists()? {
            return Ok(vec![HistoryFinding::Missing {
                repository: repository.git_dir().to_path_buf(),
            }]);
        }
        let mut findings = Vec::new();
        let lock = repository.index_lock();
        if lock.try_exists().map_err(Error::io(&lock))? {
            findings.push(HistoryFinding::IndexLocked { lock });
        }
        let status = {
            let _shared = self.lock_shared()?;
            graph_status(&repository)?
        };
        if !status.changed.is_empty() {
            let files: BTreeSet<PathBuf> = status.changed.into_iter().collect();
            findings.push(HistoryFinding::Uncommitted {
                // clone: the finding outlives the store it names.
                graph: self.root.clone(),
                files: files.into_iter().collect(),
            });
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::store::NodeFile;
    use crate::store::testing::{
        changed_in, graph_git, graph_log, sample_component, sample_decision, setup_store,
        shared_store, write_node,
    };
    use tempfile::TempDir;

    fn message(origin: &str, session: &str) -> CommitMessage {
        CommitMessage::new(Tool::Fmt, Some(origin.into()), Some(session.into())).unwrap()
    }

    /// Write the sample component as `name` through a writer, with
    /// `message`.
    fn write_component(store: &Store, name: &str, message: &CommitMessage) -> Result<()> {
        let writer = store.begin_write()?;
        let write = store.prepare_write(&store.component_path(name), &sample_component())?;
        writer.commit(&[write], &[], message)
    }

    #[test]
    fn each_write_is_one_commit_of_its_files_with_its_trailers() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());

        write_component(&store, "auth", &message("row 1", "s-1")).unwrap();
        write_component(&store, "billing", &message("row 2", "s-2")).unwrap();

        assert_eq!(
            graph_log(&store),
            [
                "fmt\n\nOrigin: row 2\nSession: s-2",
                "fmt\n\nOrigin: row 1\nSession: s-1",
                "init"
            ]
        );
        assert_eq!(changed_in(&store, "HEAD"), ["components/billing.toml"]);
        assert_eq!(changed_in(&store, "HEAD~1"), ["components/auth.toml"]);
        assert_eq!(changed_in(&store, "HEAD~2"), ["project.toml"]);
        assert_eq!(
            graph_git(
                &store,
                &["log", "-1", "--format=%(trailers:key=Origin,valueonly)"]
            ),
            "row 2\n\n"
        );
    }

    #[test]
    fn hand_edits_are_one_commit_before_the_write_which_holds_only_its_own() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        write_component(&store, "auth", &message("row 1", "s-1")).unwrap();
        // Edited, added and deleted by hand, and a file that is no graph
        // file, which no commit takes.
        let project = fs::read_to_string(store.project_path()).unwrap();
        fs::write(
            store.project_path(),
            project.replace("test-project", "edited"),
        )
        .unwrap();
        write_node(
            &store,
            &NodeFile::Decision("d".into()),
            &sample_decision("project"),
        );
        fs::remove_file(store.component_path("auth")).unwrap();
        fs::write(store.root().join("notes.md"), "mine").unwrap();

        write_component(&store, "billing", &message("row 2", "s-2")).unwrap();

        let log = graph_log(&store);
        assert_eq!(
            log[..2],
            ["fmt\n\nOrigin: row 2\nSession: s-2", "hand edit"]
        );
        assert_eq!(changed_in(&store, "HEAD"), ["components/billing.toml"]);
        assert_eq!(
            changed_in(&store, "HEAD~1"),
            ["components/auth.toml", "decisions/d.toml", "project.toml"]
        );
        assert_eq!(store.history_findings(Placement::Shared).unwrap(), []);
        assert_eq!(
            graph_git(&store, &["status", "--porcelain"]),
            "?? notes.md\n"
        );
    }

    #[test]
    fn under_tracked_a_write_makes_no_repository_and_no_commit() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        write_component(&store, "auth", &message("row 1", "s-1")).unwrap();

        assert!(store.component_path("auth").is_file());
        assert!(!store.root().join(".git").exists());
        assert_eq!(store.history_findings(Placement::Tracked).unwrap(), []);
    }

    #[test]
    fn a_leftover_index_lock_refuses_the_write_with_nothing_written() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        let lock = store.root().join(".git/index.lock");
        fs::write(&lock, "").unwrap();

        let err = write_component(&store, "auth", &message("row 1", "s-1")).unwrap_err();

        assert!(
            matches!(&err, Error::GraphIndexLocked { lock: l } if *l == lock),
            "{err:?}"
        );
        assert!(!store.component_path("auth").exists());
        assert!(!store.has_journal().unwrap());
        assert_eq!(
            store.history_findings(Placement::Shared).unwrap(),
            [HistoryFinding::IndexLocked { lock: lock.clone() }]
        );
        fs::remove_file(&lock).unwrap();
        write_component(&store, "auth", &message("row 1", "s-1")).unwrap();
        assert_eq!(graph_log(&store).len(), 2);
    }

    /// Someone deleted `.git`: the write makes the repository again, and
    /// its first commit holds every graph file, the write's among them.
    #[test]
    fn a_missing_repository_is_made_again_by_the_write_whose_commit_holds_every_file() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        write_node(
            &store,
            &NodeFile::Decision("d".into()),
            &sample_decision("project"),
        );
        fs::remove_dir_all(store.root().join(".git")).unwrap();
        assert_eq!(
            store.history_findings(Placement::Shared).unwrap(),
            [HistoryFinding::Missing {
                repository: store.root().join(".git")
            }]
        );

        write_component(&store, "auth", &message("row 1", "s-1")).unwrap();

        assert_eq!(graph_log(&store), ["fmt\n\nOrigin: row 1\nSession: s-1"]);
        assert_eq!(
            changed_in(&store, "HEAD"),
            ["components/auth.toml", "decisions/d.toml", "project.toml"]
        );
        assert_eq!(store.history_findings(Placement::Shared).unwrap(), []);
    }

    #[test]
    fn findings_list_the_uncommitted_files_and_asking_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        let index = store.root().join(".git/index");
        let before = fs::read(&index).unwrap();
        write_node(
            &store,
            &NodeFile::Decision("d".into()),
            &sample_decision("project"),
        );
        // Same bytes, new stat data: a status that saves its refresh
        // rewrites the index.
        let project = fs::read(store.project_path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(store.project_path(), &project).unwrap();

        let findings = store.history_findings(Placement::Shared).unwrap();

        assert_eq!(
            findings,
            [HistoryFinding::Uncommitted {
                graph: store.root().to_path_buf(),
                files: vec![PathBuf::from("decisions/d.toml")],
            }]
        );
        assert_eq!(fs::read(&index).unwrap(), before);
        assert!(!store.root().join(".git/index.lock").exists());
    }

    #[test]
    fn a_message_names_the_tool_and_carries_the_trailers_it_has() {
        let both = CommitMessage::new(Tool::Fmt, Some("plan row".into()), Some("s-1".into()));
        assert_eq!(
            both.unwrap().render(),
            "fmt\n\nOrigin: plan row\nSession: s-1\n"
        );
        let session = CommitMessage::new(Tool::Migrate, None, Some("s-1".into()));
        assert_eq!(session.unwrap().render(), "migrate\n\nSession: s-1\n");
        assert_eq!(CommitMessage::of(Tool::Init).render(), "init\n");
    }

    #[test]
    fn a_trailer_that_is_not_one_plain_line_is_refused() {
        for (origin, session, field) in [
            (Some("a\nSession: forged"), None, TextField::Origin),
            (None, Some(" "), TextField::Session),
            (Some("ok"), Some("bidi \u{202E}"), TextField::Session),
        ] {
            let err = CommitMessage::new(
                Tool::Fmt,
                origin.map(str::to_owned),
                session.map(str::to_owned),
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::TrailerText { field: f, .. } if f == field),
                "{err:?}"
            );
        }
        let long = "o".repeat(201);
        assert!(CommitMessage::new(Tool::Fmt, Some(long), None).is_err());
    }

    #[test]
    fn a_message_reads_back_from_the_journal_and_is_checked_again() {
        let message = CommitMessage::new(Tool::Fmt, Some("row".into()), Some("s".into())).unwrap();
        let text = toml::to_string(&message).unwrap();
        assert_eq!(toml::from_str::<CommitMessage>(&text).unwrap(), message);
        let bare = toml::to_string(&CommitMessage::of(Tool::Init)).unwrap();
        assert_eq!(bare, "tool = \"init\"\n");

        let forged = "tool = \"fmt\"\norigin = \"a\\nSession: b\"\n";
        assert!(toml::from_str::<CommitMessage>(forged).is_err());
    }

    #[test]
    fn findings_name_their_files_and_only_a_lock_fails_check() {
        let uncommitted = HistoryFinding::Uncommitted {
            graph: PathBuf::from("/p/.trurlic"),
            files: vec!["decisions/a.toml".into(), "project.toml".into()],
        };
        assert_eq!(
            uncommitted.to_string(),
            "/p/.trurlic: 2 file(s) differ from the last commit of the graph's history: \
             decisions/a.toml, project.toml; `trurlic fmt`, or the next write, commits them \
             as `hand edit`"
        );
        assert!(!uncommitted.is_error());
        let lock = HistoryFinding::IndexLocked {
            lock: PathBuf::from("/p/.trurlic/.git/index.lock"),
        };
        assert!(
            lock.to_string()
                .starts_with("/p/.trurlic/.git/index.lock: ")
        );
        assert!(lock.is_error());
        assert!(
            !HistoryFinding::Missing {
                repository: PathBuf::from("/p/.trurlic/.git")
            }
            .is_error()
        );
    }

    #[test]
    fn the_graph_files_are_the_three_kinds_one_level_deep() {
        assert_eq!(
            graph_files(),
            [
                ":(glob)project.toml",
                ":(glob)components/*.toml",
                ":(glob)decisions/*.toml"
            ]
        );
    }
}
