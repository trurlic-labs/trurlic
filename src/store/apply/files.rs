//! The files a batch commits: the file of each node whose value changed,
//! written in canonical form, and the file of each node it removed,
//! deleted. The writer's graph follows the commit ([`Writer::land`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use super::edit::Before;
use super::refusal::{OpAt, Rule, refused};
use crate::store::format::{Canonical, Store};
use crate::store::graph::NodeFile;
use crate::store::write::{CommitMessage, PendingWrite, Writer};
use crate::{Error, Result};

impl Writer<'_> {
    /// Commit the files the batch changed from `before`, and count them
    /// written and removed; `changed_by` names the op that last changed a
    /// node, for a file a load could not read back. The writer's graph
    /// follows the files: past the commit point it is the batch's, before
    /// it the one `before` holds.
    pub(super) fn land(
        &mut self,
        before: Before,
        changed_by: ChangedBy<'_, '_>,
        message: &CommitMessage,
    ) -> Result<(usize, usize)> {
        let committed = self
            .files(&before, changed_by)
            .and_then(|(writes, removes)| {
                self.commit(&writes, &removes, message)?;
                Ok((writes.len(), removes.len()))
            });
        match committed {
            Ok(counts) => {
                self.settle(&before);
                Ok(counts)
            }
            Err(e @ Error::WritePending { .. }) => {
                self.settle(&before);
                Err(e)
            }
            Err(e) => {
                before.restore(&mut self.state);
                Err(e)
            }
        }
    }

    /// The writes and removals that take the files from `before` to the
    /// writer's graph.
    fn files(
        &self,
        before: &Before,
        changed_by: ChangedBy<'_, '_>,
    ) -> Result<(Vec<PendingWrite>, Vec<PathBuf>)> {
        let mut files = Files {
            store: self.store,
            changed_by,
            writes: Vec::new(),
            removes: Vec::new(),
        };
        let state = &self.state;
        let (decision, component) = (Store::decision_path, Store::component_path);
        files.add(
            &before.decisions,
            &state.decisions,
            decision,
            NodeFile::Decision,
        )?;
        files.add(
            &before.components,
            &state.components,
            component,
            NodeFile::Component,
        )?;
        Ok((files.writes, files.removes))
    }

    /// Drop from the non-canonical list every file the batch wrote or
    /// removed: each is canonical, or gone.
    fn settle(&mut self, before: &Before) {
        let state = &mut self.state;
        state.non_canonical.retain(|file| match file {
            NodeFile::Project => true,
            NodeFile::Component(name) => !before.components.contains_key(name),
            NodeFile::Decision(name) => !before.decisions.contains_key(name),
        });
    }
}

/// The op that last changed a node, by the node's name.
type ChangedBy<'f, 'n> = &'f dyn Fn(&str) -> Option<&'n OpAt>;

/// The files a batch commits, gathered one node kind at a time.
struct Files<'f, 'n> {
    store: &'f Store,
    changed_by: ChangedBy<'f, 'n>,
    writes: Vec<PendingWrite>,
    removes: Vec<PathBuf>,
}

impl Files<'_, '_> {
    /// Add the writes and removals that take the nodes of one kind from
    /// `before` to `now`, each node's file at `path` and, in a refusal,
    /// named by `file`: a node whose value is as it was is not written. A
    /// canonical text over the cap a load reads refuses the batch, naming
    /// the op that last changed the node.
    fn add<T: Canonical + DeserializeOwned + PartialEq>(
        &mut self,
        before: &BTreeMap<String, Option<T>>,
        now: &BTreeMap<String, T>,
        path: fn(&Store, &str) -> PathBuf,
        file: fn(String) -> NodeFile,
    ) -> Result<()> {
        for (name, old) in before {
            match now.get(name) {
                Some(new) if old.as_ref() != Some(new) => {
                    let write = self.store.prepare_write(&path(self.store, name), new);
                    let write = write.map_err(|error| self.too_large(error, name, file))?;
                    self.writes.push(write);
                }
                Some(_) => {}
                None if old.is_some() => self.removes.push(path(self.store, name)),
                None => {}
            }
        }
        Ok(())
    }

    /// `error`; for a canonical text over the cap a load reads, the
    /// refusal naming the file `file` gives the node `name` and the op that
    /// last changed it.
    fn too_large(&self, error: Error, name: &str, file: fn(String) -> NodeFile) -> Error {
        match error {
            Error::CanonicalTooLarge { bytes, max, .. } => {
                // clone: the refusal owns the op it names.
                let op = (self.changed_by)(name).cloned();
                let file = file(name.to_owned());
                refused(op, None, Rule::FileTooLarge { file, bytes, max })
            }
            other => other,
        }
    }
}
