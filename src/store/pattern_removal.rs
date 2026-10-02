//! Pattern removal.
//!
//! A pattern is a synthesis over decisions: nothing in the graph points at
//! it, so removing one never strands another node. Its outgoing `member_of`
//! and `applies_to` edges go with it, and the member decisions stay.

use crate::{Error, Result};

use super::state::ProjectState;
use super::{Store, StoreLock};

impl Store {
    /// Delete a pattern's node file and every edge it owns in one commit.
    /// Fails with [`Error::PatternNotFound`] when no such pattern exists.
    pub fn remove_pattern(
        &self,
        lock: &StoreLock,
        state: &mut ProjectState,
        name: &str,
    ) -> Result<()> {
        let Some(snapshot) = state.patterns.remove(name) else {
            return Err(Error::PatternNotFound(name.into()));
        };
        let removed = state.remove_graph_node(name);
        let removes = vec![self.pattern_path(name)];

        if let Err(e) = self.commit_with_graph(lock, vec![], removes, state) {
            state.patterns.insert(name.into(), snapshot);
            state.restore_graph_node(removed);
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{Attribution, EdgeKind};
    use crate::store::testing::setup_store_with_components;
    use crate::store::{RecordDecisionParams, RecordPatternParams};
    use tempfile::TempDir;

    fn record(store: &Store, lock: &StoreLock, state: &mut ProjectState, choice: &str) -> String {
        let params = RecordDecisionParams {
            component: "auth",
            choice,
            reason: "Test reasoning",
            depends_on: &[],
            alternatives: &[],
            constrains: &[],
            tags: &[],
            attribution: Attribution::User,
            code_refs: &[],
        };
        store.record_decision(lock, state, params).unwrap()
    }

    /// A store with two `auth` decisions grouped by the pattern `token-hygiene`.
    fn store_with_pattern(dir: &std::path::Path) -> (Store, ProjectState, [String; 2]) {
        let (store, mut state) = setup_store_with_components(dir, &[("auth", "Auth")]);
        let lock = store.lock().unwrap();
        let members = [
            record(&store, &lock, &mut state, "Use JWT"),
            record(&store, &lock, &mut state, "Rotate tokens"),
        ];
        let params = RecordPatternParams {
            name: "token-hygiene",
            description: "Coordinated token handling",
            decisions: &members,
            components: &[],
            tags: &[],
        };
        store.record_pattern(&lock, &mut state, params).unwrap();
        drop(lock);
        (store, state, members)
    }

    #[test]
    fn remove_pattern_deletes_node_and_edges_but_keeps_members() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, members) = store_with_pattern(tmp.path());
        let lock = store.lock().unwrap();

        store
            .remove_pattern(&lock, &mut state, "token-hygiene")
            .unwrap();
        drop(lock);

        let reloaded = store.load_state().unwrap();
        assert!(!reloaded.patterns.contains_key("token-hygiene"));
        assert!(!store.pattern_path("token-hygiene").exists());
        let pattern_edges = reloaded.graph_index.edges.iter().filter(|e| {
            e.from == "token-hygiene" && matches!(e.kind, EdgeKind::MemberOf | EdgeKind::AppliesTo)
        });
        assert_eq!(pattern_edges.count(), 0);
        for member in &members {
            assert!(reloaded.decisions.contains_key(member.as_str()));
        }
        assert!(reloaded.validate().is_empty(), "{:?}", reloaded.validate());
    }

    #[test]
    fn remove_pattern_rejects_unknown_name_and_leaves_state() {
        let tmp = TempDir::new().unwrap();
        let (store, mut state, _) = store_with_pattern(tmp.path());
        let lock = store.lock().unwrap();

        let err = store
            .remove_pattern(&lock, &mut state, "no-such-pattern")
            .unwrap_err();

        assert!(matches!(err, Error::PatternNotFound(ref n) if n == "no-such-pattern"));
        assert!(state.patterns.contains_key("token-hygiene"));
    }
}
