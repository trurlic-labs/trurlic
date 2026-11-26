//! The history step of the writer protocol. Under `shared` placement the
//! graph directory is a git repository of its own (`git::graph`), and each
//! write ends with one commit to it, whose message names the tool and whose
//! trailers carry `Origin:` and `Session:`. Under `tracked` the project's
//! git holds the history, and trurlic commits nothing.
//!
//! Before a write reaches its commit point, the graph files the last commit
//! does not hold as they are on disk are committed as `hand edit`, so the
//! write's commit holds the write alone, and the write's staged files are
//! written to the repository as blobs. The journal carries a
//! [`HistoryStep`]: the message, the commit the write's goes on, and the id
//! of each blob. The commit's index is set from those ids, so it holds the
//! write's bytes whatever the files hold by the time it is made: a hand
//! edit made between a stopped write and the opener that finishes it is
//! left uncommitted, for the next `hand edit` commit. The first commit of a
//! repository holds every graph file as well.
//!
//! An opener that finishes a stopped write looks for its commit first: the
//! commit after the step's parent on the branch's first-parent line, when
//! it holds the write's blobs, is the write's, made before the stop, and
//! reworded, amended, packed or followed by others since.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::console::diag;
use crate::git::graph::{Committed, Oid, Repository, Status};
use crate::{Error, Result};

use super::failpoint::{self, Site};
use super::lock::StoreLock;
use super::message::{CommitMessage, Tool};
use crate::store::format::Store;
use crate::store::graph::listed;
use crate::store::model::{COMPONENTS_DIR, DECISIONS_DIR, PROJECT_FILE, Placement, STATE_DIR};

/// The subject of the commit of graph files no tool wrote.
const HAND_EDIT: &str = "hand edit";

/// Where a commit's history goes, by the graph's placement.
#[derive(Debug, Clone, Copy)]
pub(in crate::store) enum History<'m> {
    /// `tracked`: the project's git holds the history.
    Project,
    /// `shared`: a commit to the graph's own repository.
    Graph(&'m CommitMessage),
}

impl<'m> History<'m> {
    pub(in crate::store) fn of(placement: Placement, message: &'m CommitMessage) -> Self {
        match placement {
            Placement::Tracked => Self::Project,
            Placement::Shared => Self::Graph(message),
        }
    }
}

/// The repository's branch, as the history step found it.
#[derive(Debug)]
pub(super) enum Head {
    /// The commit the write's goes on.
    Born { parent: Oid },
    /// No commit yet: `files` are the graph files on disk, which the first
    /// commit holds with the write's.
    Unborn { files: Vec<PathBuf> },
}

impl Head {
    fn parent(&self) -> Option<&Oid> {
        match self {
            Self::Born { parent } => Some(parent),
            Self::Unborn { .. } => None,
        }
    }
}

/// A write's history commit as its journal carries it: everything the
/// commit needs once the files are in place, so a journal that names only
/// this step finishes the write without touching a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryStep {
    message: CommitMessage,
    /// The branch's commit before the write's; `None` when it had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<Oid>,
    /// Each file the write puts in place, at the blob of its bytes.
    #[serde(default, rename = "blob", skip_serializing_if = "Vec::is_empty")]
    blobs: Vec<Blob>,
    /// The files the write removes.
    #[serde(default, rename = "remove", skip_serializing_if = "Vec::is_empty")]
    removes: Vec<PathBuf>,
}

/// A file of a write and the blob of its bytes, relative to the graph
/// directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Blob {
    path: PathBuf,
    id: Oid,
}

impl HistoryStep {
    /// The step of a write of `message` on `head` that puts each file of
    /// `blobs` in place at its blob and removes `removes`.
    pub(super) fn new(
        message: &CommitMessage,
        head: &Head,
        blobs: impl IntoIterator<Item = (PathBuf, Oid)>,
        removes: &[PathBuf],
    ) -> Self {
        Self {
            // clone: the journal owns its copy of the message.
            message: message.clone(),
            // clone: the journal owns its copy; the writer keeps the head.
            parent: head.parent().cloned(),
            blobs: blobs
                .into_iter()
                .map(|(path, id)| Blob { path, id })
                .collect(),
            // clone: the step keeps its own list of the removals, which a
            // journal narrowed to the step holds without the file step.
            removes: removes.to_vec(),
        }
    }

    /// Every path the step names: its blobs', then its removals.
    pub(super) fn paths(&self) -> impl Iterator<Item = &Path> {
        self.blobs
            .iter()
            .map(|blob| blob.path.as_path())
            .chain(self.removes.iter().map(PathBuf::as_path))
    }
}

/// What an opener that finishes a stopped write finds in the repository.
enum Found<'s> {
    /// The write's commit, made before the stop.
    Made,
    /// No commit of the write yet; `missing` are the step's blobs and
    /// parent the repository lost since, to a gc or a repository made again.
    Waits {
        head: Head,
        missing: BTreeSet<&'s Oid>,
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
                let names: Vec<_> = files.iter().map(|file| file.to_string_lossy()).collect();
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

/// Set the index of `repository` for the commit of `step` on `head`: each
/// blob at its path and each removal taken out. A blob in `missing`, lost
/// since the step was written, is staged from its file as it is on disk,
/// and so is every other graph file of a first commit.
fn set_index(
    repository: &Repository,
    step: &HistoryStep,
    head: &Head,
    missing: &BTreeSet<&Oid>,
) -> Result<()> {
    let mut from_disk = BTreeSet::new();
    let mut entries = Vec::with_capacity(step.blobs.len() + step.removes.len());
    for blob in &step.blobs {
        if missing.contains(&blob.id) {
            diag!(
                "warning: {}: the graph's repository lost the bytes an interrupted write staged \
                 for this file; its history commit holds the file as it is",
                blob.path.display()
            );
            from_disk.insert(blob.path.as_path());
        } else {
            entries.push((blob.path.as_path(), &blob.id));
        }
    }
    let null = match head {
        Head::Born { parent } => Some(parent.null()),
        // No commit holds a removed file, and the files on disk no
        // longer include it.
        Head::Unborn { files } => {
            let ours: BTreeSet<&Path> = entries.iter().map(|(path, _)| *path).collect();
            from_disk.extend(
                files
                    .iter()
                    .map(PathBuf::as_path)
                    .filter(|f| !ours.contains(f)),
            );
            None
        }
    };
    if let Some(null) = &null {
        entries.extend(step.removes.iter().map(|path| (path.as_path(), null)));
    }
    if !from_disk.is_empty() {
        let from_disk: Vec<PathBuf> = from_disk.into_iter().map(Path::to_path_buf).collect();
        repository.stage(&from_disk)?;
    }
    repository.set_index(&entries)
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
        let Some(parent) = status.head else {
            return Ok(Head::Unborn {
                files: status.changed,
            });
        };
        if status.changed.is_empty() {
            return Ok(Head::Born { parent });
        }
        repository.stage(&status.changed)?;
        failpoint::hit(Site::HistoryStaged);
        let parent = match repository.commit(&format!("{HAND_EDIT}\n"))? {
            Committed::Made => repository.head()?,
            Committed::Nothing => parent,
        };
        Ok(Head::Born { parent })
    }

    /// Write the staged files `staged`, each in `.state/tmp/`, to the graph's
    /// repository as blobs, in one batch, and return their ids in order.
    pub(super) fn write_blobs(&self, _lock: &StoreLock, staged: &[&Path]) -> Result<Vec<Oid>> {
        let names: Vec<&OsStr> = staged.iter().filter_map(|path| path.file_name()).collect();
        let index = self.temp_path();
        let written = self
            .repository()
            .write_blobs(&self.tmp_dir(), &names, &index);
        // A scratch index left by a failure is a temp file recovery removes.
        let _ = fs::remove_file(&index);
        written
    }

    /// The history step after the files are in place: the index set to the
    /// step's blobs and removals, then the commit. `head` is what
    /// [`prepare_history`](Self::prepare_history) found, or `None` when an
    /// opener finishes a stopped write, which asks the repository again and
    /// makes no commit the history already holds.
    pub(super) fn commit_history(
        &self,
        lock: &StoreLock,
        step: &HistoryStep,
        head: Option<Head>,
    ) -> Result<Committed> {
        let repository = self.repository();
        let (head, missing) = match head {
            Some(head) => (head, BTreeSet::new()),
            None => match self.look_for_commit(lock, &repository, step)? {
                Found::Made => return Ok(Committed::Nothing),
                Found::Waits { head, missing } => (head, missing),
            },
        };
        set_index(&repository, step, &head, &missing)?;
        failpoint::hit(Site::HistoryStaged);
        repository.commit(&step.message.render())
    }

    /// The repository as it is, for an opener that finishes a stopped
    /// write: its commit already made, or the head the commit goes on.
    fn look_for_commit<'s>(
        &self,
        lock: &StoreLock,
        repository: &Repository,
        step: &'s HistoryStep,
    ) -> Result<Found<'s>> {
        self.make_again(lock, repository)?;
        let status = graph_status(repository)?;
        let asked: Vec<&Oid> = step
            .blobs
            .iter()
            .map(|blob| &blob.id)
            .chain(step.parent.as_ref())
            .collect();
        let missing = repository.missing(&asked)?;
        let Some(head) = status.head else {
            let head = Head::Unborn {
                files: status.changed,
            };
            return Ok(Found::Waits { head, missing });
        };
        // Past the parent, a commit by hand or the write's own; past a
        // parent the repository lost, it was made again since, and holds no
        // commit of the write.
        let parent_lost = step.parent.as_ref().is_some_and(|p| missing.contains(p));
        if step.parent.as_ref() != Some(&head)
            && !parent_lost
            && let Some(next) = repository.first_after(step.parent.as_ref())?
        {
            let null = head.null();
            let entries: Vec<(&Path, &Oid)> = step
                .blobs
                .iter()
                .map(|blob| (blob.path.as_path(), &blob.id))
                .chain(step.removes.iter().map(|path| (path.as_path(), &null)))
                .collect();
            if repository.holds(&next, &entries)? {
                return Ok(Found::Made);
            }
        }
        Ok(Found::Waits {
            head: Head::Born { parent: head },
            missing,
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
