//! Loading the graph from its files: `project.toml` and every node file,
//! each read as its type, and nothing else.
//!
//! A load fails only when a file cannot be read as its type: it cannot be
//! read, it is no regular file, its name ends in `.toml` but is not UTF-8,
//! it is larger than [`MAX_NODE_FILE_BYTES`], it is not TOML, or it holds a
//! key, a value or a combination of values its type has no place for. Everything a loaded graph can hold but should not, such as an edge
//! to no node or two nodes of one name, is an issue `validate.rs` reports,
//! so a hand edit that breaks the graph leaves it readable to `check`.
//!
//! Nothing here takes a lock. Every load runs inside `access`, which holds
//! the shared or the exclusive lock around it, so a load never reads a
//! commit halfway through its renames.

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, ReadDir};
use std::io::{ErrorKind, Read};
use std::path::Path;

use rayon::prelude::*;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::Store;
use super::canonical::{Canonical, is_canonical};
use crate::store::limits::MAX_NODE_FILE_BYTES;
use crate::store::model::{COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, Project};
use crate::store::model::{NodeFile, ProjectState};

/// One byte past the cap, so a read that fills it proves the file too large.
const READ_LIMIT: u64 = MAX_NODE_FILE_BYTES as u64 + 1;

/// The bytes of the node file or `project.toml` at `path`, refused unopened
/// when it is no regular file ([`ensure_regular`]), and unread past
/// [`MAX_NODE_FILE_BYTES`]: the read stops at the cap, so a file that grows
/// while it is read costs no more.
pub(super) fn read_node_bytes(path: &Path) -> Result<Vec<u8>> {
    let hint = ensure_regular(path)?.len().min(READ_LIMIT);
    let file = File::open(path).map_err(Error::io(path))?;
    let mut bytes = Vec::with_capacity(usize::try_from(hint).unwrap_or(0));
    file.take(READ_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(Error::io(path))?;
    if bytes.len() > MAX_NODE_FILE_BYTES {
        return Err(Error::NodeTooLarge {
            path: path.to_path_buf(),
            max: MAX_NODE_FILE_BYTES,
        });
    }
    Ok(bytes)
}

/// The metadata of `path`, refused unless it is a regular file once its
/// symlinks are followed: checked before it is opened, so a FIFO never
/// blocks a read.
pub(super) fn ensure_regular(path: &Path) -> Result<Metadata> {
    let metadata = fs::metadata(path).map_err(Error::io(path))?;
    if metadata.is_file() {
        Ok(metadata)
    } else {
        Err(Error::NotAFile {
            path: path.to_path_buf(),
        })
    }
}

/// The entries of `dir`; `None` when it is gone.
pub(super) fn read_dir_if_present(dir: &Path) -> Result<Option<ReadDir>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

/// The stems of the node files in `dir`, sorted; none when `dir` is gone.
/// A node file is every entry whose name ends in `.toml`, as the history's
/// pathspecs match them, so none is skipped: a name that is not UTF-8 is
/// [`Error::NodeFileName`]. A stem that is no node name is listed, and
/// validation reports it; an entry that is no regular file is refused by
/// the read of it ([`read_node_bytes`]), which runs in parallel.
pub(super) fn list_toml_stems(dir: &Path) -> Result<Vec<String>> {
    let Some(entries) = read_dir_if_present(dir)? else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::io(dir))?;
        let name = entry.file_name();
        if !name.as_encoded_bytes().ends_with(b".toml") {
            continue;
        }
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".toml")) else {
            return Err(Error::NodeFileName { path: entry.path() });
        };
        names.push(stem.to_owned());
    }
    names.sort_unstable();
    Ok(names)
}

/// A node file read as `T`, and whether its bytes are `T`'s canonical form.
struct NodeRead<T> {
    name: String,
    node: T,
    canonical: bool,
}

impl Store {
    /// The graph of `project`, read from `project_bytes`, and of every node
    /// file. The two kinds are read concurrently: listing a directory and
    /// waiting for its last file are serial steps of each.
    pub(super) fn read_graph(
        &self,
        project: Project,
        project_bytes: &[u8],
    ) -> Result<ProjectState> {
        let (components, decisions) = rayon::join(
            || self.read_nodes::<Component>(COMPONENTS_DIR),
            || self.read_nodes::<Decision>(DECISIONS_DIR),
        );
        let (components, decisions) = (components?, decisions?);
        let mut non_canonical = Vec::new();
        if !is_canonical(&project, project_bytes) {
            non_canonical.push(NodeFile::Project);
        }
        let components = by_name(components, NodeFile::Component, &mut non_canonical);
        let decisions = by_name(decisions, NodeFile::Decision, &mut non_canonical);
        Ok(ProjectState {
            project,
            components,
            decisions,
            non_canonical,
            // clone: each load's state owns the root it was read for.
            code_root: self.code_root.clone(),
        })
    }

    /// Every file of `dir` read as `T`, in name order, in parallel. When
    /// several fail, the error is the first file's by name, whichever thread
    /// met it first.
    fn read_nodes<T: DeserializeOwned + Canonical + Send>(
        &self,
        dir: &str,
    ) -> Result<Vec<NodeRead<T>>> {
        let dir = self.root.join(dir);
        let reads: Vec<Result<NodeRead<T>>> = list_toml_stems(&dir)?
            .into_par_iter()
            .map(|name| {
                let path = dir.join(format!("{name}.toml"));
                let bytes = read_node_bytes(&path)?;
                let node: T = toml::from_slice(&bytes).map_err(Error::toml(&path))?;
                let canonical = is_canonical(&node, &bytes);
                Ok(NodeRead {
                    name,
                    node,
                    canonical,
                })
            })
            .collect();
        reads.into_iter().collect()
    }
}

/// The nodes of `reads`, which are sorted by name, keyed by name; each one
/// whose bytes are not canonical is pushed to `non_canonical` as `file`
/// names it.
fn by_name<T>(
    reads: Vec<NodeRead<T>>,
    file: fn(String) -> NodeFile,
    non_canonical: &mut Vec<NodeFile>,
) -> BTreeMap<String, T> {
    reads
        .into_iter()
        .map(|read| {
            if !read.canonical {
                // clone: the list and the map each own the name.
                non_canonical.push(file(read.name.clone()));
            }
            (read.name, read.node)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::store::canonical::canonical;
    use crate::store::testing::{sample_component, sample_decision, setup_store, write_node};
    use tempfile::TempDir;

    #[test]
    fn load_state_reads_every_node_file_and_nothing_else() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        write_node(
            &store,
            &NodeFile::Component("auth".into()),
            &sample_component(),
        );
        write_node(
            &store,
            &NodeFile::Decision("sign".into()),
            &sample_decision("auth"),
        );
        fs::write(store.root().join("graph.toml"), "not = [toml").unwrap();
        fs::write(store.root().join(DECISIONS_DIR).join("notes.md"), "#").unwrap();

        let state = store.load_state().unwrap();

        assert_eq!(state.components.keys().collect::<Vec<_>>(), ["auth"]);
        assert_eq!(state.decisions["sign"], sample_decision("auth"));
        assert_eq!(state.non_canonical, []);
    }

    #[test]
    fn a_node_file_that_does_not_parse_is_named() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let broken = store.decision_path("broken");
        fs::write(&broken, "choice = [\n").unwrap();

        let err = store.load_state().map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Toml { path, .. } if *path == broken),
            "{err:?}"
        );
        assert!(err.to_string().starts_with(&broken.display().to_string()));
    }

    #[test]
    fn of_two_broken_files_the_first_by_name_is_named() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        for name in ["b", "a", "c"] {
            fs::write(store.decision_path(name), "x = [\n").unwrap();
        }
        for _ in 0..20 {
            let err = store.load_state().map(|_| ()).unwrap_err();
            assert!(
                matches!(&err, Error::Toml { path, .. } if *path == store.decision_path("a")),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_node_file_that_cannot_be_read_is_named() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let unreadable = store.decision_path("unreadable");
        fs::write(&unreadable, "").unwrap();
        let mut permissions = fs::metadata(&unreadable).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o000);
        fs::set_permissions(&unreadable, permissions).unwrap();
        if File::open(&unreadable).is_ok() {
            // Run as root, which reads any file.
            return;
        }

        let err = store.load_state().map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Io { path, .. } if *path == unreadable),
            "{err:?}"
        );
    }

    /// `mkfifo <path>`: std makes no FIFO.
    fn fifo(path: &Path) {
        let made = std::process::Command::new("mkfifo").arg(path).status();
        assert!(made.unwrap().success(), "mkfifo {}", path.display());
    }

    /// A directory, a FIFO, a symlink to either, and `project.toml` as a
    /// FIFO are refused unopened, naming the entry: a reader that opened
    /// the FIFO would wait for a writer forever.
    #[test]
    fn an_entry_that_is_no_regular_file_is_refused_unopened() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let decisions = store.root().join(DECISIONS_DIR);
        /// What a case makes at the entry's path.
        type Make = fn(&Path);
        let cases: [(&str, Make); 4] = [
            ("directory.toml", |path| fs::create_dir(path).unwrap()),
            ("fifo.toml", fifo),
            ("to-fifo.toml", |path| {
                fifo(&path.with_file_name("elsewhere"));
                std::os::unix::fs::symlink("elsewhere", path).unwrap();
            }),
            ("to-dir.toml", |path| {
                std::os::unix::fs::symlink(".", path).unwrap();
            }),
        ];
        for (name, make) in cases {
            let path = decisions.join(name);
            make(&path);

            let err = store.load_state().map(|_| ()).unwrap_err();

            assert!(
                matches!(&err, Error::NotAFile { path: named } if *named == path),
                "{name}: {err:?}"
            );
            fs::remove_file(&path)
                .or_else(|_| fs::remove_dir(&path))
                .unwrap();
        }
        let linked = decisions.join("linked.toml");
        write_node(
            &store,
            &NodeFile::Decision("target".into()),
            &sample_decision("project"),
        );
        std::os::unix::fs::symlink("target.toml", &linked).unwrap();
        assert!(store.load_state().unwrap().decisions.contains_key("linked"));

        let project = store.project_path();
        fs::remove_file(&project).unwrap();
        fifo(&project);
        let err = store.load_state().map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, Error::NotAFile { path } if *path == project),
            "{err:?}"
        );
    }

    /// Every entry the history commits is a node file to the load: one
    /// named `.toml` is the empty stem, an issue; one whose name is not
    /// UTF-8 is refused, naming it.
    #[test]
    fn every_entry_named_toml_is_a_node_file_none_skipped() {
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let decisions = store.root().join(DECISIONS_DIR);
        fs::write(
            decisions.join(".toml"),
            canonical(&sample_decision("project")),
        )
        .unwrap();
        let state = store.load_state().unwrap();
        assert!(state.decisions.contains_key(""));
        assert_eq!(
            state.validate()[0].kind,
            crate::store::IssueKind::NameInvalid
        );

        let foreign = decisions.join(std::ffi::OsStr::from_bytes(b"caf\xe9.toml"));
        fs::write(&foreign, canonical(&sample_decision("project"))).unwrap();
        let err = store.load_state().map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, Error::NodeFileName { path } if *path == foreign),
            "{err:?}"
        );
    }

    #[test]
    fn a_node_file_over_the_cap_is_refused_before_it_is_parsed() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let at_cap = store.component_path("at-cap");
        let mut text = canonical(&sample_component()).into_bytes();
        text.resize(MAX_NODE_FILE_BYTES, b'\n');
        fs::write(&at_cap, &text).unwrap();
        assert!(store.load_state().is_ok(), "a file at the cap loads");

        // One byte more, and not TOML: only the size can refuse it.
        text.push(b'[');
        fs::write(&at_cap, &text).unwrap();
        let err = store.load_state().map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::NodeTooLarge { path, max: MAX_NODE_FILE_BYTES } if *path == at_cap),
            "{err:?}"
        );
        assert!(err.to_string().contains(&at_cap.display().to_string()));
    }

    #[test]
    fn a_hand_edit_that_is_not_canonical_loads_and_is_listed() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let decision = sample_decision("project");
        write_node(&store, &NodeFile::Decision("kept".into()), &decision);
        let edited =
            canonical(&decision).replace("status = \"accepted\"\n", "") + "status = 'accepted'\n";
        fs::write(store.decision_path("edited"), edited).unwrap();

        let state = store.load_state().unwrap();

        assert_eq!(state.decisions["edited"], decision);
        assert_eq!(state.non_canonical, [NodeFile::Decision("edited".into())]);
    }
}
