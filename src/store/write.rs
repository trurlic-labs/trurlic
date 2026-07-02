use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::graph::Severity;
use super::limits::MAX_HISTORY_ENTRIES;
use super::schema::{
    Attribution, CodeRef, Component, ComponentFile, Decision, DecisionFile, EdgeEntry, EdgeKind,
    GraphIndex, HistoryEntry, NodeEntry, NodeKind, Pattern, PatternFile,
};
use super::state::{
    ProjectState, is_reserved_node_name, is_valid_kebab_case, slugify, unique_decision_stem,
};
use super::{Store, StoreLock};

// ── PendingWrite ─────────────────────────────────────────────────────────────

/// A file write staged for batch commit.
/// Created via [`Store::prepare_write`], executed via [`Store::commit_batch`].
#[must_use = "a pending write must be passed to commit_batch or commit_with_graph"]
pub(crate) struct PendingWrite {
    target: PathBuf,
    content: String,
}

impl PendingWrite {
    /// BLAKE3 hash of the serialized content that will be written.
    #[must_use]
    pub(crate) fn content_hash(&self) -> String {
        super::hash_bytes(self.content.as_bytes())
    }
}

// ── Store write methods ─────────────────────────────────────────────────

// ── RecordDecisionParams ────────────────────────────────────────────────

/// Parameters for [`Store::record_decision`].
///
/// Callers are responsible for validating that `component` exists (or is
/// `"project"`), that all `depends_on`/`constrains` targets exist, and that
/// names are valid kebab-case. The shared write path does not re-validate —
/// it trusts the caller and focuses on atomic mutation.
pub struct RecordDecisionParams<'a> {
    pub component: &'a str,
    pub choice: &'a str,
    pub reason: &'a str,
    pub alternatives: &'a [String],
    pub depends_on: &'a [String],
    pub constrains: &'a [String],
    pub tags: &'a [String],
    pub attribution: Attribution,
    pub code_refs: &'a [CodeRef],
}

// ── AmendDecisionParams ─────────────────────────────────────────────

/// Parameters for [`Store::amend_decision`].
///
/// All fields are optional; the method requires at least one `Some`.
/// Transport-specific quality checks (field length limits, reason
/// minimums) belong in the adapter — this struct carries only the
/// values to write.
pub struct AmendDecisionParams<'a> {
    pub choice: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub tags: Option<&'a [String]>,
    pub code_refs: Option<&'a [CodeRef]>,
}

// ── ReviseDecisionParams ────────────────────────────────────────────

/// Parameters for [`Store::revise_decision`].
///
/// All fields are optional; the method requires at least one `Some`.
/// `writes_history` is decided by the caller: set it whenever the revision
/// touches a substantive field (choice or reason) so the pre-edit values are
/// versioned into history. Tag and code-ref edits update in place and leave
/// no history, so they carry `writes_history = false`.
pub struct ReviseDecisionParams<'a> {
    pub choice: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub tags: Option<Vec<String>>,
    pub code_refs: Option<Vec<CodeRef>>,
    pub writes_history: bool,
}

// ── RecordPatternParams ─────────────────────────────────────────────

/// Parameters for [`Store::record_pattern`].
///
/// If `components` is empty, the method infers applies‐to components
/// from the decisions' owning components (excluding `"project"`).
pub struct RecordPatternParams<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub decisions: &'a [String],
    pub components: &'a [String],
    pub tags: &'a [String],
}

impl Store {
    /// Write `value` to `target` atomically via `.state/tmp/`.
    /// Serializes to TOML, writes to a temp file, validates by deserializing
    /// back from disk, then renames to the final path. Caller **must** hold
    /// a [`StoreLock`].
    pub(crate) fn write_atomic<T: Serialize + DeserializeOwned>(
        &self,
        _lock: &StoreLock,
        target: &Path,
        value: &T,
    ) -> Result<()> {
        self.verify_path(target)?;

        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        let filename = target
            .file_name()
            .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
        let tmp_path = tmp_dir.join(filename);

        let content = toml::to_string_pretty(value)?;

        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(Error::Io(e));
        }

        // Validate written file by deserializing back — catches partial
        // writes, encoding corruption, and serialization round-trip issues.
        let readback = match fs::read_to_string(&tmp_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::Io(e));
            }
        };
        if let Err(e) = toml::from_str::<T>(&readback) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Validation(format!(
                "write verification failed: written file does not deserialize: {e}"
            )));
        }

        if let Some(parent) = target.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        if let Err(e) = fs::rename(&tmp_path, target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        Ok(())
    }

    /// Serialize a value to TOML and verify the round-trip.
    /// Returns a [`PendingWrite`] for use with [`commit_batch`](Self::commit_batch).
    /// The content is deserialized back to `T` at this stage so that type-safe
    /// verification happens while the type is still known; `commit_batch`
    /// then verifies filesystem-level integrity via byte-compare.
    pub(crate) fn prepare_write<T: Serialize + DeserializeOwned>(
        &self,
        target: &Path,
        value: &T,
    ) -> Result<PendingWrite> {
        self.verify_path(target)?;

        let content = toml::to_string_pretty(value)?;
        toml::from_str::<T>(&content).map_err(|e| {
            Error::Validation(format!("serialization round-trip verification failed: {e}"))
        })?;
        Ok(PendingWrite {
            target: target.to_path_buf(),
            content,
        })
    }

    /// Execute a batch of writes and removes as a two-phase commit.
    ///
    /// Phase 1: write all content to `.state/tmp/`.
    /// Phase 2: verify each temp file (byte-compare; type-safe check was in `prepare_write`).
    /// Phase 3: rename all temp files to final paths (each atomic on POSIX).
    ///          If `graph_update` is `Some`, `graph.toml` is appended as the
    ///          **last** rename — serving as the commit point per the storage spec.
    /// Phase 4: remove old files (best-effort — renames already committed).
    ///
    /// Caller **must** hold a [`StoreLock`].
    pub(crate) fn commit_batch(
        &self,
        _lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: Vec<PathBuf>,
        graph_update: Option<GraphIndex>,
    ) -> Result<()> {
        if writes.is_empty() && removes.is_empty() && graph_update.is_none() {
            return Ok(());
        }

        // Build the full set of writes: node files first, graph.toml last.
        let mut all_writes = writes;

        if let Some(mut index) = graph_update {
            index.nodes.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            index
                .edges
                .sort_unstable_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
            let content = toml::to_string_pretty(&index)?;
            toml::from_str::<GraphIndex>(&content).map_err(|e| {
                Error::Validation(format!("graph index round-trip verification failed: {e}"))
            })?;
            let target = self.graph_path();
            self.verify_path(&target)?;
            all_writes.push(PendingWrite { target, content });
        }

        // Verify all target paths up-front before touching the filesystem.
        for write in &all_writes {
            self.verify_path(&write.target)?;
        }
        for path in &removes {
            self.verify_path(path)?;
        }

        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        // Phase 1: Write all to tmp
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(all_writes.len());

        for (i, write) in all_writes.iter().enumerate() {
            let filename = write
                .target
                .file_name()
                .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
            let tmp_name = format!("{i}_{}", filename.to_string_lossy());
            let tmp_path = tmp_dir.join(tmp_name);

            if let Err(e) = fs::write(&tmp_path, &write.content) {
                cleanup_tmp_files(&staged);
                return Err(Error::Io(e));
            }
            staged.push((tmp_path, write.target.clone()));
        }

        // Phase 2: Verify write integrity — type-safe deserialization already
        // happened in prepare_write; this byte-compare catches filesystem-level
        // corruption (partial writes, bitflips) on the validated content.
        for (i, (tmp_path, _)) in staged.iter().enumerate() {
            let readback = match fs::read_to_string(tmp_path) {
                Ok(s) => s,
                Err(e) => {
                    cleanup_tmp_files(&staged);
                    return Err(Error::Io(e));
                }
            };
            if readback != all_writes[i].content {
                cleanup_tmp_files(&staged);
                return Err(Error::Validation(
                    "batch write verification failed: content mismatch".into(),
                ));
            }
        }

        // Ensure parent directories exist before renaming.
        for (_, target) in &staged {
            if let Some(parent) = target.parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                cleanup_tmp_files(&staged);
                return Err(Error::Io(e));
            }
        }

        // Phase 3: Rename all to final paths.
        // graph.toml is last (appended last to all_writes).
        for (i, (tmp_path, target)) in staged.iter().enumerate() {
            if let Err(e) = fs::rename(tmp_path, target) {
                // Clean the failed tmp file and all remaining staged files.
                let _ = fs::remove_file(tmp_path);
                for (remaining, _) in staged.iter().skip(i + 1) {
                    let _ = fs::remove_file(remaining);
                }
                return Err(Error::Io(e));
            }
        }

        // Phase 4: Remove old files.
        //
        // Best-effort: renames (Phase 3) already committed the new state.
        // A remove failure here leaves an orphan file but does NOT roll back
        // the successful writes. Crash recovery and `trurlic check` will
        // surface any resulting inconsistency.
        for path in &removes {
            if let Err(e) = fs::remove_file(path)
                && e.kind() != ErrorKind::NotFound
            {
                eprintln!("warning: failed to remove {}: {e}", path.display());
            }
        }

        Ok(())
    }

    /// Validate the full graph derived from `state`, then commit node files
    /// and a normalized `graph.toml` in one atomic transaction.
    ///
    /// This is the primary write path for all graph-mutating operations.
    /// It builds an [`InMemoryGraph`] from the current state, runs all
    /// validation checks, and — only if the graph is error-free — exports
    /// a deterministically sorted index and commits it alongside the
    /// provided node file writes. `graph.toml` is renamed last, serving
    /// as the commit point per the storage spec.
    ///
    /// On success, `state.graph` is updated in-place with the validated
    /// graph — callers do **not** need to call `rebuild_graph()`.
    fn commit_with_graph(
        &self,
        lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: Vec<PathBuf>,
        state: &mut ProjectState,
    ) -> Result<()> {
        // Pre-check: duplicate node names in the index would cause silent
        // data loss during InMemoryGraph construction (HashMap overwrite).
        {
            let mut seen = HashSet::with_capacity(state.graph_index.nodes.len());
            for node in &state.graph_index.nodes {
                if !seen.insert(&node.name) {
                    return Err(Error::GraphIntegrity(format!(
                        "duplicate node name `{}` in graph index",
                        node.name
                    )));
                }
            }
        }

        let graph = state.build_graph();
        let issues = graph.validate();
        let errors: Vec<&str> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.message.as_str())
            .collect();
        if !errors.is_empty() {
            return Err(Error::GraphIntegrity(errors.join("; ")));
        }
        let index = graph.to_index();
        self.commit_batch(lock, writes, removes, Some(index))?;

        // Reuse the validated graph — avoids a redundant rebuild_graph() in
        // every caller.
        state.graph = graph;

        Ok(())
    }

    pub(crate) fn remove_file(&self, _lock: &StoreLock, target: &Path) -> Result<()> {
        self.verify_path(target)?;
        Ok(fs::remove_file(target)?)
    }

    // ── Record decision (shared write path) ────────────────────────────

    /// Record a new decision to disk with full graph validation and rollback.
    ///
    /// Single write path for CLI `decide`, MCP `record_decision`, and design
    /// conversation extraction. Derives a unique filename stem from
    /// `params.choice`, builds the `DecisionFile`, adds the node and all
    /// requested edges to the graph index, and commits atomically.
    ///
    /// On success, `state` is updated in-place (including graph cache
    /// rebuild) and the decision stem is returned. On failure, `state` is
    /// rolled back to its pre-call condition.
    ///
    /// **Callers must** validate inputs before calling: component existence,
    /// edge target existence, name format. This function trusts those
    /// invariants and focuses on atomic mutation + graph integrity.
    pub fn record_decision(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        params: RecordDecisionParams<'_>,
    ) -> Result<String> {
        // The store is the trust boundary — validate refs here even though
        // MCP/map callers also validate, so no write path can bypass it.
        super::validate_code_refs(params.code_refs)?;

        let stem = unique_decision_stem(state, &slugify(params.choice))?;

        let decision = DecisionFile {
            decision: Decision {
                component: params.component.into(),
                choice: params.choice.into(),
                reason: params.reason.into(),
                alternatives: params.alternatives.to_vec(),
                tags: params.tags.to_vec(),
                attribution: params.attribution,
                created: Utc::now(),
                code_refs: params.code_refs.to_vec(),
                history: Vec::new(),
            },
        };

        let write = self.prepare_write(&self.decision_path(&stem), &decision)?;
        let hash = write.content_hash();

        // Checkpoint for rollback — O(1) since all mutations are appends.
        let checkpoint = state.graph_checkpoint();

        state.graph_index.nodes.push(NodeEntry {
            name: stem.clone(),
            kind: NodeKind::Decision,
            tags: params.tags.to_vec(),
            hash,
        });

        state.graph_index.edges.push(EdgeEntry {
            from: stem.clone(),
            to: params.component.into(),
            kind: EdgeKind::BelongsTo,
        });

        for dep in params.depends_on {
            state.graph_index.edges.push(EdgeEntry {
                from: stem.clone(),
                to: dep.clone(),
                kind: EdgeKind::DependsOn,
            });
        }

        for con in params.constrains {
            state.graph_index.edges.push(EdgeEntry {
                from: stem.clone(),
                to: con.clone(),
                kind: EdgeKind::Constrains,
            });
        }

        state.decisions.insert(stem.clone(), Arc::new(decision));

        if let Err(e) = self.commit_with_graph(lock, vec![write], vec![], state) {
            state.decisions.remove(&stem);
            state.rollback_graph(checkpoint);
            return Err(e);
        }

        Ok(stem)
    }

    // ── Add component (shared write path) ───────────────────────────────

    /// Create a new component with full validation and atomic commit.
    ///
    /// Single write path for CLI `add component`, MCP `add_component`,
    /// and map `POST /api/component`. Validates name format, uniqueness,
    /// and cross-type collisions, then commits atomically.
    ///
    /// On success, `state` is updated in-place (including graph cache).
    /// On failure, `state` is rolled back.
    pub fn add_component(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
        description: &str,
    ) -> Result<()> {
        if !is_valid_kebab_case(name) {
            return Err(Error::InvalidName(name.into()));
        }
        if is_reserved_node_name(name) {
            return Err(Error::ReservedName(name.into()));
        }
        if state.components.contains_key(name) {
            return Err(Error::ComponentExists(name.into()));
        }
        if state.is_node_name_taken(name) {
            return Err(Error::Validation(format!(
                "name `{name}` is already used by an existing decision or pattern"
            )));
        }

        let comp = ComponentFile {
            component: Component {
                name: name.into(),
                description: description.into(),
            },
        };

        let write = self.prepare_write(&self.component_path(name), &comp)?;
        let hash = write.content_hash();

        let checkpoint = state.graph_checkpoint();
        state.graph_index.nodes.push(NodeEntry {
            name: name.into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash,
        });
        state.components.insert(name.into(), Arc::new(comp));

        if let Err(e) = self.commit_with_graph(lock, vec![write], vec![], state) {
            state.rollback_graph(checkpoint);
            state.components.remove(name);
            return Err(e);
        }

        Ok(())
    }

    // ── Remove component (shared write path) ────────────────────────────

    /// Remove a component from disk and the graph index.
    ///
    /// **Callers must** run cascade pre-flight (`check_component_cascade`)
    /// and decide how to handle blockers before calling. This method does
    /// not check cascade rules — it trusts the caller.
    ///
    /// On failure, `state` is rolled back.
    pub fn remove_component(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
    ) -> Result<()> {
        if !state.components.contains_key(name) {
            return Err(Error::ComponentNotFound(name.into()));
        }

        let comp_snapshot = state.components.remove(name);
        let removed = state.remove_graph_node(name);
        let removes = vec![self.component_path(name)];

        if let Err(e) = self.commit_with_graph(lock, vec![], removes, state) {
            if let Some(c) = comp_snapshot {
                state.components.insert(name.into(), c);
            }
            state.restore_graph_node(removed);
            return Err(e);
        }

        Ok(())
    }

    // ── Add connection (shared write path) ──────────────────────────────

    /// Connect two components with full validation and atomic commit.
    ///
    /// On success, `state` is updated in-place. On failure, rolled back.
    pub fn add_connection(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        from: &str,
        to: &str,
    ) -> Result<()> {
        if !state.components.contains_key(from) {
            return Err(Error::ComponentNotFound(from.into()));
        }
        if !state.components.contains_key(to) {
            return Err(Error::ComponentNotFound(to.into()));
        }
        if from == to {
            return Err(Error::SelfConnection(from.into()));
        }

        let duplicate = state
            .graph_index
            .edges
            .iter()
            .any(|e| e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo);
        if duplicate {
            return Err(Error::DuplicateConnection {
                from: from.into(),
                to: to.into(),
            });
        }

        let checkpoint = state.graph_checkpoint();
        state.graph_index.edges.push(EdgeEntry {
            from: from.into(),
            to: to.into(),
            kind: EdgeKind::ConnectsTo,
        });

        if let Err(e) = self.commit_with_graph(lock, vec![], vec![], state) {
            state.rollback_graph(checkpoint);
            return Err(e);
        }

        Ok(())
    }

    // ── Remove connection (shared write path) ───────────────────────────

    /// Remove a connection between two components.
    ///
    /// On failure, `state` is rolled back.
    pub fn remove_connection(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let existed = state
            .graph_index
            .edges
            .iter()
            .any(|e| e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo);
        if !existed {
            return Err(Error::ConnectionNotFound {
                from: from.into(),
                to: to.into(),
            });
        }

        let removed_edge = EdgeEntry {
            from: from.into(),
            to: to.into(),
            kind: EdgeKind::ConnectsTo,
        };

        state
            .graph_index
            .edges
            .retain(|e| !(e.from == from && e.to == to && e.kind == EdgeKind::ConnectsTo));

        if let Err(e) = self.commit_with_graph(lock, vec![], vec![], state) {
            state.graph_index.edges.push(removed_edge);
            return Err(e);
        }

        Ok(())
    }

    // ── Remove decision (shared write path) ─────────────────────────────

    /// Remove a decision from disk and the graph index.
    ///
    /// **Callers must** run cascade pre-flight (`check_decision_cascade`)
    /// and decide how to handle blockers before calling.
    ///
    /// On failure, `state` is rolled back.
    pub fn remove_decision(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
    ) -> Result<()> {
        if !state.decisions.contains_key(name) {
            return Err(Error::DecisionNotFound(name.into()));
        }

        let dec_snapshot = state.decisions.remove(name);
        let removed = state.remove_graph_node(name);
        let removes = vec![self.decision_path(name)];

        if let Err(e) = self.commit_with_graph(lock, vec![], removes, state) {
            if let Some(d) = dec_snapshot {
                state.decisions.insert(name.into(), d);
            }
            state.restore_graph_node(removed);
            return Err(e);
        }

        Ok(())
    }

    // ── Amend decision (shared write path) ────────────────────────────

    /// Amend an existing decision in place: update choice, reason, and/or
    /// tags without changing the `created` timestamp.
    ///
    /// Single write path for MCP `update_decision(mode=amend)` and map
    /// `PUT /api/decision/:name`. Handles state mutation, graph‐index tag
    /// sync, and rollback on commit failure.
    ///
    /// **Callers** validate transport‐specific quality constraints (field
    /// lengths, reason minimums) before calling. This method enforces
    /// baseline correctness only (non‐empty fields, at least one change).
    pub fn amend_decision(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
        params: AmendDecisionParams<'_>,
    ) -> Result<()> {
        if params.choice.is_none()
            && params.reason.is_none()
            && params.tags.is_none()
            && params.code_refs.is_none()
        {
            return Err(Error::Validation(
                "at least one of choice, reason, tags, or code_refs is required".into(),
            ));
        }
        if let Some(c) = params.choice
            && c.trim().is_empty()
        {
            return Err(Error::Validation("choice must not be empty".into()));
        }
        if let Some(r) = params.reason
            && r.trim().is_empty()
        {
            return Err(Error::Validation("reason must not be empty".into()));
        }

        let old_dec = state
            .decisions
            .get(name)
            .ok_or_else(|| Error::DecisionNotFound(name.into()))?
            .clone();

        let mut amended = DecisionFile::clone(&old_dec);
        if let Some(c) = params.choice {
            amended.decision.choice = c.into();
        }
        if let Some(r) = params.reason {
            amended.decision.reason = r.into();
        }
        if let Some(t) = params.tags {
            amended.decision.tags = t.to_vec();
        }
        if let Some(refs) = params.code_refs {
            super::validate_code_refs(refs)?;
            amended.decision.code_refs = refs.to_vec();
        }

        let write = self.prepare_write(&self.decision_path(name), &amended)?;
        let hash = write.content_hash();

        // Mutate state. Save only the affected fields for rollback.
        state.decisions.insert(name.into(), Arc::new(amended));
        let old_hash = state.update_node_hash(name, hash);
        let old_tags = if let Some(t) = params.tags {
            state
                .graph_index
                .nodes
                .iter_mut()
                .find(|n| n.name == name)
                .map(|n| std::mem::replace(&mut n.tags, t.to_vec()))
        } else {
            None
        };

        if let Err(e) = self.commit_with_graph(lock, vec![write], vec![], state) {
            state.decisions.insert(name.into(), old_dec);
            if let Some(h) = old_hash {
                state.update_node_hash(name, h);
            }
            if let Some(t) = old_tags
                && let Some(n) = state.graph_index.nodes.iter_mut().find(|n| n.name == name)
            {
                n.tags = t;
            }
            return Err(e);
        }

        Ok(())
    }

    // ── Revise decision (shared write path) ───────────────────────────

    /// Revise an existing decision in place, versioning the prior choice and
    /// reason into history.
    ///
    /// Single write path for MCP `update_decision(mode=revise)` and map
    /// `PUT /api/decision/:name`. When `params.writes_history` is set, the
    /// pre-edit choice and reason are appended to the decision's history as a
    /// [`HistoryEntry`] before the new values overwrite them. History is a
    /// ring buffer capped at [`MAX_HISTORY_ENTRIES`]: once full, pushing a new
    /// entry drops the oldest. Tag and code-ref edits apply without leaving
    /// history.
    ///
    /// The decision's name, `created` timestamp, and every graph edge survive
    /// unchanged — revision never creates a new node, so no edge is ever
    /// orphaned.
    ///
    /// **Callers** validate transport-specific quality constraints (field
    /// lengths, reason minimums) before calling. This method enforces baseline
    /// correctness only (non-empty fields, at least one change).
    pub fn revise_decision(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
        params: ReviseDecisionParams<'_>,
    ) -> Result<()> {
        if params.choice.is_none()
            && params.reason.is_none()
            && params.tags.is_none()
            && params.code_refs.is_none()
        {
            return Err(Error::Validation(
                "at least one of choice, reason, tags, or code_refs is required".into(),
            ));
        }
        if let Some(c) = params.choice
            && c.trim().is_empty()
        {
            return Err(Error::Validation("choice must not be empty".into()));
        }
        if let Some(r) = params.reason
            && r.trim().is_empty()
        {
            return Err(Error::Validation("reason must not be empty".into()));
        }
        if let Some(refs) = &params.code_refs {
            super::validate_code_refs(refs)?;
        }

        let old_dec = state
            .decisions
            .get(name)
            .ok_or_else(|| Error::DecisionNotFound(name.into()))?
            .clone();

        let mut revised = DecisionFile::clone(&old_dec);

        // Version the pre-edit substantive fields before overwriting them.
        if params.writes_history {
            revised.decision.history.push(HistoryEntry {
                choice: revised.decision.choice.clone(),
                reason: revised.decision.reason.clone(),
                changed_at: Utc::now(),
            });
            // Ring buffer: the oldest entry falls off once the cap is exceeded.
            if revised.decision.history.len() > MAX_HISTORY_ENTRIES {
                revised.decision.history.remove(0);
            }
        }

        if let Some(c) = params.choice {
            revised.decision.choice = c.into();
        }
        if let Some(r) = params.reason {
            revised.decision.reason = r.into();
        }
        if let Some(t) = &params.tags {
            revised.decision.tags = t.clone();
        }
        if let Some(refs) = params.code_refs {
            revised.decision.code_refs = refs;
        }

        let write = self.prepare_write(&self.decision_path(name), &revised)?;
        let hash = write.content_hash();

        // Mutate state. Save only the affected fields for rollback.
        state.decisions.insert(name.into(), Arc::new(revised));
        let old_hash = state.update_node_hash(name, hash);
        let old_tags = if let Some(t) = &params.tags {
            state
                .graph_index
                .nodes
                .iter_mut()
                .find(|n| n.name == name)
                .map(|n| std::mem::replace(&mut n.tags, t.clone()))
        } else {
            None
        };

        if let Err(e) = self.commit_with_graph(lock, vec![write], vec![], state) {
            state.decisions.insert(name.into(), old_dec);
            if let Some(h) = old_hash {
                state.update_node_hash(name, h);
            }
            if let Some(t) = old_tags
                && let Some(n) = state.graph_index.nodes.iter_mut().find(|n| n.name == name)
            {
                n.tags = t;
            }
            return Err(e);
        }

        Ok(())
    }

    // ── Rename component (shared write path) ────────────────────────────

    /// Rename a component, updating all references (decisions, graph
    /// index nodes, graph index edges) atomically.
    ///
    /// Validates the new name, uniqueness, and cross-type collisions.
    /// On failure, `state` is rolled back to its pre-call condition.
    pub fn rename_component(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        old: &str,
        new: &str,
    ) -> Result<()> {
        if !is_valid_kebab_case(new) {
            return Err(Error::InvalidName(new.into()));
        }
        if is_reserved_node_name(new) {
            return Err(Error::ReservedName(new.into()));
        }
        if !state.components.contains_key(old) {
            return Err(Error::ComponentNotFound(old.into()));
        }
        if state.components.contains_key(new) {
            return Err(Error::ComponentExists(new.into()));
        }
        if state.is_node_name_taken(new) {
            return Err(Error::Validation(format!(
                "name `{new}` is already used by an existing decision or pattern"
            )));
        }

        // Snapshot graph_index for rollback — rename touches nodes and
        // edges in-place, making selective undo error-prone. The index
        // is small (hundreds of entries), so a full clone is cheap
        // insurance for a once-per-invocation CLI operation.
        let old_graph_index = state.graph_index.clone();

        let affected_decisions: Vec<String> = state
            .decisions
            .iter()
            .filter(|(_, dec)| dec.decision.component == old)
            .map(|(dname, _)| dname.clone())
            .collect();

        // Apply in-memory mutations.
        let old_comp = state
            .components
            .remove(old)
            .ok_or_else(|| Error::ComponentNotFound(old.into()))?;
        let mut renamed = ComponentFile::clone(&old_comp);
        renamed.component.name = new.into();
        state.components.insert(new.into(), Arc::new(renamed));

        for dec in state.decisions.values_mut() {
            if dec.decision.component == old {
                Arc::make_mut(dec).decision.component = new.into();
            }
        }

        for node in &mut state.graph_index.nodes {
            if node.name == old {
                node.name = new.into();
            }
        }
        for edge in &mut state.graph_index.edges {
            if edge.from == old {
                edge.from = new.into();
            }
            if edge.to == old {
                edge.to = new.into();
            }
        }

        // Prepare writes and update hashes.
        let mut writes = Vec::new();

        let comp_write =
            self.prepare_write(&self.component_path(new), state.components[new].as_ref())?;
        if let Some(node) = state.graph_index.nodes.iter_mut().find(|n| n.name == new) {
            node.hash = comp_write.content_hash();
        }
        writes.push(comp_write);

        for dname in &affected_decisions {
            let dec_write = self.prepare_write(
                &self.decision_path(dname),
                state.decisions[dname.as_str()].as_ref(),
            )?;
            if let Some(node) = state
                .graph_index
                .nodes
                .iter_mut()
                .find(|n| n.name == *dname)
            {
                node.hash = dec_write.content_hash();
            }
            writes.push(dec_write);
        }

        let removes = vec![self.component_path(old)];

        if let Err(e) = self.commit_with_graph(lock, writes, removes, state) {
            // Rollback: revert component rename.
            if let Some(comp) = state.components.remove(new) {
                let mut reverted = ComponentFile::clone(&comp);
                reverted.component.name = old.into();
                state.components.insert(old.into(), Arc::new(reverted));
            }
            // Revert decision component fields.
            for dec in state.decisions.values_mut() {
                if dec.decision.component == new {
                    Arc::make_mut(dec).decision.component = old.into();
                }
            }
            // Revert graph_index entirely (node names + edge refs + hashes).
            state.graph_index = old_graph_index;
            return Err(e);
        }

        Ok(())
    }

    // ── Record pattern (shared write path) ──────────────────────────────

    /// Record a new pattern to disk with full graph validation and
    /// rollback.
    ///
    /// Validates that all referenced decisions exist, that the derived
    /// slug is unique, and that ≥ 2 decisions are referenced. If
    /// `params.components` is empty, applies‐to components are inferred
    /// from the decisions' owning components (excluding `"project"`).
    ///
    /// On success, `state` is updated in-place and the pattern slug
    /// (derived filename stem) is returned. On failure, `state` is
    /// rolled back.
    pub fn record_pattern(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        params: RecordPatternParams<'_>,
    ) -> Result<String> {
        if params.decisions.len() < 2 {
            return Err(Error::Validation(
                "a pattern must reference at least 2 decisions".into(),
            ));
        }

        for dname in params.decisions {
            if !state.decisions.contains_key(dname.as_str()) {
                return Err(Error::DecisionNotFound(dname.clone()));
            }
        }

        // Resolve component list: explicit or inferred from decisions.
        let components: Vec<String> = if params.components.is_empty() {
            let mut inferred: HashSet<String> = HashSet::new();
            for dname in params.decisions {
                if let Some(dec) = state.decisions.get(dname.as_str()) {
                    let comp = &dec.decision.component;
                    if comp != "project" {
                        inferred.insert(comp.clone());
                    }
                }
            }
            inferred.into_iter().collect()
        } else {
            for cname in params.components {
                if !state.components.contains_key(cname.as_str()) {
                    return Err(Error::ComponentNotFound(cname.clone()));
                }
            }
            params.components.to_vec()
        };

        let slug = slugify(params.name);

        if is_reserved_node_name(&slug) {
            return Err(Error::ReservedName(slug));
        }
        if state.is_node_name_taken(&slug) {
            return Err(Error::Validation(format!(
                "name `{slug}` is already used by an existing node"
            )));
        }

        let pattern = PatternFile {
            pattern: Pattern {
                name: params.name.into(),
                description: params.description.into(),
            },
        };

        let write = self.prepare_write(&self.pattern_path(&slug), &pattern)?;
        let hash = write.content_hash();

        let checkpoint = state.graph_checkpoint();

        state.graph_index.nodes.push(NodeEntry {
            name: slug.clone(),
            kind: NodeKind::Pattern,
            tags: params.tags.to_vec(),
            hash,
        });

        for dname in params.decisions {
            state.graph_index.edges.push(EdgeEntry {
                from: slug.clone(),
                to: dname.clone(),
                kind: EdgeKind::MemberOf,
            });
        }

        for cname in &components {
            state.graph_index.edges.push(EdgeEntry {
                from: slug.clone(),
                to: cname.clone(),
                kind: EdgeKind::AppliesTo,
            });
        }

        state.patterns.insert(slug.clone(), Arc::new(pattern));

        if let Err(e) = self.commit_with_graph(lock, vec![write], vec![], state) {
            state.patterns.remove(&slug);
            state.rollback_graph(checkpoint);
            return Err(e);
        }

        Ok(slug)
    }

    // ── Crash recovery ───────────────────────────────────────────────────

    pub fn clean_stale_tmp(&self) -> Result<usize> {
        let tmp_dir = self.tmp_dir();
        let entries = match fs::read_dir(&tmp_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(Error::Io(e)),
        };

        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            if entry.path().is_file() {
                match fs::remove_file(entry.path()) {
                    Ok(()) => count += 1,
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        }
        Ok(count)
    }
}

fn cleanup_tmp_files(staged: &[(PathBuf, PathBuf)]) {
    for (tmp_path, _) in staged {
        let _ = fs::remove_file(tmp_path);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::testing::*;
    use tempfile::TempDir;

    // ── crash recovery ───────────────────────────────────────────────────

    #[test]
    fn clean_stale_tmp_removes_leftovers() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        fs::create_dir_all(&tmp_dir).unwrap();
        fs::write(tmp_dir.join("stale.toml"), "leftover").unwrap();
        fs::write(tmp_dir.join("another.toml"), "leftover").unwrap();

        let count = store.clean_stale_tmp().unwrap();
        assert_eq!(count, 2);

        assert_eq!(store.clean_stale_tmp().unwrap(), 0);
    }

    #[test]
    fn clean_stale_tmp_no_tmp_dir() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        assert_eq!(store.clean_stale_tmp().unwrap(), 0);
    }

    // ── atomic write guarantees ──────────────────────────────────────────

    #[test]
    fn atomic_write_leaves_no_tmp_on_success() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after atomic write");
        }
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(crate::store::STORE_DIR);
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        let store = Store::at(root);
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        assert!(store.component_path("auth").exists());
    }

    #[test]
    fn atomic_write_rejects_path_outside_root() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let outside = tmp.path().join("outside.toml");
        let err = store.write_atomic(&lock, &outside, &comp).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }

    // ── commit_batch ─────────────────────────────────────────────────────

    #[test]
    fn commit_batch_writes_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp1 = sample_component("auth");
        let comp2 = sample_component("database");

        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp1)
                .unwrap(),
            store
                .prepare_write(&store.component_path("database"), &comp2)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, vec![], None).unwrap();

        let read1 = store.read_component("auth").unwrap();
        assert_eq!(read1, comp1);
        let read2 = store.read_component("database").unwrap();
        assert_eq!(read2, comp2);
    }

    #[test]
    fn commit_batch_writes_and_removes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let old = sample_component("old-name");
        store
            .write_atomic(&lock, &store.component_path("old-name"), &old)
            .unwrap();

        let new = sample_component("new-name");
        let writes = vec![
            store
                .prepare_write(&store.component_path("new-name"), &new)
                .unwrap(),
        ];
        let removes = vec![store.component_path("old-name")];

        store.commit_batch(&lock, writes, removes, None).unwrap();

        assert!(store.component_path("new-name").exists());
        assert!(!store.component_path("old-name").exists());
    }

    #[test]
    fn commit_batch_leaves_no_tmp_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, vec![], None).unwrap();

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after batch commit");
        }
    }

    #[test]
    fn commit_batch_tolerates_already_removed_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let removes = vec![store.component_path("nonexistent")];
        store.commit_batch(&lock, vec![], removes, None).unwrap();
    }

    #[test]
    fn commit_batch_writes_graph_update() {
        use crate::store::schema::*;
        use chrono::Utc;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let index = GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes: vec![NodeEntry {
                name: "test".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "abc".into(),
            }],
            edges: vec![],
        };

        store
            .commit_batch(&lock, vec![], vec![], Some(index))
            .unwrap();

        assert!(store.graph_path().exists());
        let read_back: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(read_back.nodes.len(), 1);
        assert_eq!(read_back.nodes[0].name, "test");
    }

    #[test]
    fn commit_batch_sorts_graph_index() {
        use crate::store::schema::*;
        use chrono::Utc;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Deliberately unsorted nodes and edges.
        let index = GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes: vec![
                NodeEntry {
                    name: "z-node".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "z".into(),
                },
                NodeEntry {
                    name: "a-node".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "a".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "z-node".into(),
                    to: "a-node".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "a-node".into(),
                    to: "z-node".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };

        store
            .commit_batch(&lock, vec![], vec![], Some(index))
            .unwrap();

        let read_back: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(read_back.nodes[0].name, "a-node");
        assert_eq!(read_back.nodes[1].name, "z-node");
        assert_eq!(read_back.edges[0].from, "a-node");
        assert_eq!(read_back.edges[1].from, "z-node");
    }

    #[test]
    fn content_hash_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let comp = sample_component("auth");
        let w1 = store
            .prepare_write(&store.component_path("auth"), &comp)
            .unwrap();
        let w2 = store
            .prepare_write(&store.component_path("auth"), &comp)
            .unwrap();
        assert_eq!(w1.content_hash(), w2.content_hash());
        assert_eq!(w1.content_hash().len(), 64);
    }

    // ── commit_with_graph ────────────────────────────────────────────────

    #[test]
    fn commit_with_graph_validates_and_writes() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let write = store
            .prepare_write(&store.component_path("auth"), &comp)
            .unwrap();
        let hash = write.content_hash();

        let mut state = store.load_state().unwrap();
        state.graph_index.nodes.push(NodeEntry {
            name: "auth".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash,
        });
        state.components.insert("auth".into(), Arc::new(comp));

        store
            .commit_with_graph(&lock, vec![write], vec![], &mut state)
            .unwrap();

        assert!(store.component_path("auth").exists());

        let index: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert!(index.nodes.iter().any(|n| n.name == "auth"));
    }

    #[test]
    fn commit_with_graph_rejects_invalid_graph() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut state = store.load_state().unwrap();
        state.graph_index.nodes.push(NodeEntry {
            name: "orphan".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: "fake".into(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "orphan".into(),
            to: "nonexistent".into(),
            kind: EdgeKind::BelongsTo,
        });
        state.decisions.insert(
            "orphan".into(),
            sample_decision("orphan", "nonexistent").into(),
        );

        let err = store
            .commit_with_graph(&lock, vec![], vec![], &mut state)
            .unwrap_err();
        assert!(matches!(err, Error::GraphIntegrity(_)));
    }

    #[test]
    fn commit_with_graph_normalizes_index() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let c1 = sample_component("z-comp");
        let w1 = store
            .prepare_write(&store.component_path("z-comp"), &c1)
            .unwrap();
        let c2 = sample_component("a-comp");
        let w2 = store
            .prepare_write(&store.component_path("a-comp"), &c2)
            .unwrap();

        let mut state = store.load_state().unwrap();
        // Push in reverse-alphabetical order.
        state.graph_index.nodes.push(NodeEntry {
            name: "z-comp".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: w1.content_hash(),
        });
        state.graph_index.nodes.push(NodeEntry {
            name: "a-comp".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: w2.content_hash(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "z-comp".into(),
            to: "a-comp".into(),
            kind: EdgeKind::ConnectsTo,
        });
        state.components.insert("z-comp".into(), Arc::new(c1));
        state.components.insert("a-comp".into(), Arc::new(c2));

        store
            .commit_with_graph(&lock, vec![w1, w2], vec![], &mut state)
            .unwrap();

        let index: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        let names: Vec<&str> = index.nodes.iter().map(|n| n.name.as_str()).collect();
        // Should be sorted regardless of insertion order.
        assert_eq!(names[0], "a-comp");
    }

    // ── remove_file ──────────────────────────────────────────────────────

    #[test]
    fn remove_file_deletes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let path = store.component_path("auth");
        store.write_atomic(&lock, &path, &comp).unwrap();
        assert!(path.exists());

        store.remove_file(&lock, &path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn remove_file_rejects_path_outside_root() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let outside = tmp.path().join("important-file");
        fs::write(&outside, "do not delete").unwrap();

        let err = store.remove_file(&lock, &outside).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
        assert!(outside.exists(), "file outside root must not be deleted");
    }

    // ── code_refs validation at the store boundary ───────────────────────

    #[test]
    fn record_decision_rejects_invalid_code_ref() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state) = setup_store_with_components(tmp.path(), &[("auth", "Auth")]);
        let lock = store.lock().unwrap();

        // Path traversal must be refused even though this bypasses the MCP layer.
        let refs = vec![CodeRef {
            file: "../escape.rs".into(),
            symbol: None,
        }];
        let err = store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "Use JWT",
                    reason: "Stateless",
                    depends_on: &[],
                    alternatives: &[],
                    constrains: &[],
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &refs,
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    #[test]
    fn amend_decision_rejects_invalid_code_ref() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state) = setup_store_with_components(tmp.path(), &[("auth", "Auth")]);
        let lock = store.lock().unwrap();

        let stem = store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "Use JWT",
                    reason: "Stateless",
                    depends_on: &[],
                    alternatives: &[],
                    constrains: &[],
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();

        let bad = vec![CodeRef {
            file: "/etc/passwd".into(),
            symbol: None,
        }];
        let err = store
            .amend_decision(
                &lock,
                &mut state,
                &stem,
                AmendDecisionParams {
                    choice: None,
                    reason: None,
                    tags: None,
                    code_refs: Some(&bad),
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    // ── revise decision ──────────────────────────────────────────────────

    /// Record a single decision under `auth` and return the store, its state,
    /// and the decision's stem — the shared starting point for revise tests.
    fn setup_one_decision(dir: &Path) -> (Store, ProjectState, String) {
        let (store, mut state) = setup_store_with_components(dir, &[("auth", "Auth")]);
        let lock = store.lock().unwrap();
        let stem = store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "Use JWT",
                    reason: "Stateless auth",
                    depends_on: &[],
                    alternatives: &[],
                    constrains: &[],
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();
        drop(lock);
        (store, state, stem)
    }

    fn revise_params<'a>(
        choice: Option<&'a str>,
        reason: Option<&'a str>,
        tags: Option<Vec<String>>,
        code_refs: Option<Vec<CodeRef>>,
    ) -> ReviseDecisionParams<'a> {
        let writes_history = choice.is_some() || reason.is_some();
        ReviseDecisionParams {
            choice,
            reason,
            tags,
            code_refs,
            writes_history,
        }
    }

    #[test]
    fn revise_pushes_old_choice_and_reason_to_history() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(Some("Use OAuth"), Some("Delegated auth"), None, None),
            )
            .unwrap();

        let dec = &state.decisions[&stem].decision;
        assert_eq!(dec.choice, "Use OAuth");
        assert_eq!(dec.reason, "Delegated auth");
        assert_eq!(dec.history.len(), 1);
        // The entry captures the values as they stood *before* the revision.
        assert_eq!(dec.history[0].choice, "Use JWT");
        assert_eq!(dec.history[0].reason, "Stateless auth");
    }

    #[test]
    fn revise_preserves_name_and_all_edges() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state) = setup_store_with_components(tmp.path(), &[("auth", "Auth")]);
        let lock = store.lock().unwrap();

        let base = store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "Base decision",
                    reason: "Foundation",
                    depends_on: &[],
                    alternatives: &[],
                    constrains: &[],
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();
        let dependent = store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "Dependent decision",
                    reason: "Builds on base",
                    depends_on: std::slice::from_ref(&base),
                    alternatives: &[],
                    constrains: std::slice::from_ref(&base),
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();

        let edges_before = state.graph_index.edges.clone();

        store
            .revise_decision(
                &lock,
                &mut state,
                &dependent,
                revise_params(Some("Revised dependent"), None, None, None),
            )
            .unwrap();

        // The node keeps its name and every edge survives — revision never
        // rewires the graph.
        assert!(state.decisions.contains_key(&dependent));
        assert_eq!(state.graph_index.edges, edges_before);
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == dependent && e.to == base && e.kind == EdgeKind::DependsOn)
        );
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == dependent && e.to == base && e.kind == EdgeKind::Constrains)
        );
    }

    #[test]
    fn revise_tags_only_writes_no_history() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(None, None, Some(vec!["security".into()]), None),
            )
            .unwrap();

        let dec = &state.decisions[&stem].decision;
        assert_eq!(dec.tags, vec!["security".to_string()]);
        assert!(dec.history.is_empty());
        // Metadata edits mirror to the graph-index node tags.
        let node = state
            .graph_index
            .nodes
            .iter()
            .find(|n| n.name == stem)
            .unwrap();
        assert_eq!(node.tags, vec!["security".to_string()]);
    }

    #[test]
    fn revise_code_refs_only_writes_no_history() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        let refs = vec![CodeRef {
            file: "src/auth/token.rs".into(),
            symbol: Some("validate".into()),
        }];
        store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(None, None, None, Some(refs)),
            )
            .unwrap();

        let dec = &state.decisions[&stem].decision;
        assert_eq!(dec.code_refs.len(), 1);
        assert!(dec.history.is_empty());
    }

    #[test]
    fn revise_choice_and_tags_writes_exactly_one_entry() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(
                    Some("New choice"),
                    None,
                    Some(vec!["security".into()]),
                    None,
                ),
            )
            .unwrap();

        let dec = &state.decisions[&stem].decision;
        // History versions substantive fields only — one entry, not one per
        // changed field.
        assert_eq!(dec.history.len(), 1);
        assert_eq!(dec.history[0].choice, "Use JWT");
        assert_eq!(dec.tags, vec!["security".to_string()]);
    }

    #[test]
    fn revise_ring_buffer_drops_oldest_past_limit() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        // MAX_HISTORY_ENTRIES + 1 revisions overflow the ring buffer by one.
        for i in 0..=MAX_HISTORY_ENTRIES {
            let choice = format!("choice-{i}");
            store
                .revise_decision(
                    &lock,
                    &mut state,
                    &stem,
                    revise_params(Some(&choice), None, None, None),
                )
                .unwrap();
        }

        let dec = &state.decisions[&stem].decision;
        assert_eq!(dec.history.len(), MAX_HISTORY_ENTRIES);
        // The original "Use JWT" (pushed by the first revise) has fallen off;
        // the oldest surviving entry is the pre-edit value of the second revise.
        assert_eq!(dec.history[0].choice, "choice-0");
    }

    #[test]
    fn revise_history_stays_chronological() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        for choice in ["one", "two", "three"] {
            store
                .revise_decision(
                    &lock,
                    &mut state,
                    &stem,
                    revise_params(Some(choice), None, None, None),
                )
                .unwrap();
        }

        let dec = &state.decisions[&stem].decision;
        assert_eq!(dec.history.len(), 3);
        // Oldest first: choices are appended in revision order and timestamps
        // never decrease.
        let choices: Vec<&str> = dec.history.iter().map(|h| h.choice.as_str()).collect();
        assert_eq!(choices, vec!["Use JWT", "one", "two"]);
        for pair in dec.history.windows(2) {
            assert!(pair[0].changed_at <= pair[1].changed_at);
        }
    }

    #[test]
    fn revise_with_no_fields_errors() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        let err = store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(None, None, None, None),
            )
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    #[test]
    fn revise_survives_reload_from_disk() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, stem) = setup_one_decision(tmp.path());
        let lock = store.lock().unwrap();

        store
            .revise_decision(
                &lock,
                &mut state,
                &stem,
                revise_params(
                    Some("Persisted choice"),
                    Some("Persisted reason"),
                    None,
                    None,
                ),
            )
            .unwrap();
        drop(lock);

        // The history must round-trip through the TOML node file, not just
        // live in memory.
        let reloaded = store.load_state().unwrap();
        let dec = &reloaded.decisions[&stem].decision;
        assert_eq!(dec.choice, "Persisted choice");
        assert_eq!(dec.history.len(), 1);
        assert_eq!(dec.history[0].choice, "Use JWT");
    }
}
