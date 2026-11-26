//! The reads the MCP tools and `trurlic query` make of a loaded graph:
//! a component's decisions and neighbours, a decision's dependencies, and
//! the decisions anchored in a path. Each answer is in a fixed order.

use std::collections::{BTreeSet, VecDeque};

use crate::store::model::ProjectState;
use crate::store::model::{Anchor, AnchorFault, Decision, PROJECT_COMPONENT};

/// A decision as a query answers it: its name and its file.
pub(crate) type Named<'a> = (&'a str, &'a Decision);

impl ProjectState {
    /// The decisions of `component`, by name.
    #[must_use]
    pub fn decisions_of(&self, component: &str) -> Vec<Named<'_>> {
        self.decisions
            .iter()
            .filter(|(_, decision)| decision.component == component)
            .map(|(name, decision)| (name.as_str(), decision))
            .collect()
    }

    /// The project-wide decisions, by name.
    #[must_use]
    pub(crate) fn project_decisions(&self) -> Vec<Named<'_>> {
        self.decisions_of(PROJECT_COMPONENT)
    }

    /// The components that connect to `component`, by name.
    #[must_use]
    pub(crate) fn connects_from(&self, component: &str) -> Vec<&str> {
        self.components
            .iter()
            .filter(|(_, from)| from.connects_to.contains(component))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The decisions of the components `component` connects to or from,
    /// by component, then by name.
    #[must_use]
    pub(crate) fn related_decisions(&self, component: &str) -> Vec<Named<'_>> {
        let mut connected: BTreeSet<&str> = self.connects_from(component).into_iter().collect();
        if let Some(own) = self.components.get(component) {
            connected.extend(own.connects_to.iter().map(String::as_str));
        }
        connected
            .into_iter()
            .flat_map(|neighbour| self.decisions_of(neighbour))
            .collect()
    }

    /// The decisions `seeds` depend on, directly or through at most
    /// `max_depth` links, nearest first; the seeds themselves are left out,
    /// and a cycle is walked once.
    #[must_use]
    pub(crate) fn dependencies(&self, seeds: &[&str], max_depth: usize) -> Vec<Named<'_>> {
        let mut visited: BTreeSet<&str> = seeds.iter().copied().collect();
        let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
        let targets = |name: &str| {
            self.decisions
                .get(name)
                .into_iter()
                .flat_map(|decision| decision.depends_on.iter().map(String::as_str))
        };
        for &seed in seeds {
            for target in targets(seed) {
                if visited.insert(target) {
                    queue.push_back((target, 1));
                }
            }
        }
        let mut found = Vec::new();
        while let Some((name, depth)) = queue.pop_front() {
            let Some((key, decision)) = self.decisions.get_key_value(name) else {
                continue;
            };
            found.push((key.as_str(), decision));
            if depth < max_depth {
                for target in targets(name) {
                    if visited.insert(target) {
                        queue.push_back((target, depth + 1));
                    }
                }
            }
        }
        found
    }

    /// The decisions with an anchor that overlaps `path`, a file or a
    /// directory as [`normalize_path`] returns it (see [`Anchor::overlaps`]),
    /// by component, then by name.
    #[must_use]
    pub(crate) fn decisions_in(&self, path: &str) -> Vec<Named<'_>> {
        let mut found: Vec<Named<'_>> = self
            .decisions
            .iter()
            .filter(|(_, decision)| decision.anchors.iter().any(|anchor| anchor.overlaps(path)))
            .map(|(name, decision)| (name.as_str(), decision))
            .collect();
        found.sort_by(|a, b| (&a.1.component, a.0).cmp(&(&b.1.component, b.0)));
        found
    }
}

/// The anchors of `decision` that overlap `path`.
pub(crate) fn anchors_in<'a>(decision: &'a Decision, path: &str) -> Vec<&'a Anchor> {
    decision
        .anchors
        .iter()
        .filter(|anchor| anchor.overlaps(path))
        .collect()
}

/// `path` in the one spelling an anchor holds: a leading `./`, runs of `/`
/// and a trailing `/` dropped. Refused like an anchor's file when it could
/// name a place outside the code root.
pub(crate) fn normalize_path(path: &str) -> Result<String, AnchorFault> {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    let normalized = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let normalized = if trimmed.starts_with('/') {
        format!("/{normalized}")
    } else {
        normalized
    };
    Anchor::new(normalized, None).map(|anchor| anchor.file().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{
        anchored, depends_on_state, empty_state, sample_component, sample_decision,
    };

    fn names(found: &[Named<'_>]) -> Vec<String> {
        found.iter().map(|(name, _)| (*name).to_owned()).collect()
    }

    /// `auth` connects to `store`, `gateway` to `auth`; two decisions each.
    fn neighbourhood() -> ProjectState {
        let mut state = empty_state();
        for (name, connects_to) in [
            ("auth", vec!["store"]),
            ("gateway", vec!["auth"]),
            ("store", vec![]),
        ] {
            let mut component = sample_component();
            component.connects_to = connects_to.into_iter().map(String::from).collect();
            state.components.insert(name.into(), component);
            for index in [2, 1] {
                state
                    .decisions
                    .insert(format!("{name}-{index}"), sample_decision(name));
            }
        }
        state
            .decisions
            .insert("rule".into(), sample_decision(PROJECT_COMPONENT));
        state
    }

    #[test]
    fn a_component_has_its_own_decisions_by_name() {
        let state = neighbourhood();
        assert_eq!(names(&state.decisions_of("auth")), ["auth-1", "auth-2"]);
        assert_eq!(names(&state.project_decisions()), ["rule"]);
        assert!(state.decisions_of("nothing").is_empty());
    }

    #[test]
    fn related_decisions_come_from_both_directions_by_component() {
        let state = neighbourhood();
        assert_eq!(state.connects_from("auth"), ["gateway"]);
        assert_eq!(
            names(&state.related_decisions("auth")),
            ["gateway-1", "gateway-2", "store-1", "store-2"]
        );
        assert!(state.related_decisions("nothing").is_empty());
    }

    #[test]
    fn dependencies_are_walked_nearest_first_within_the_depth() {
        let state = depends_on_state(&[("a", "b"), ("b", "c"), ("c", "d"), ("a", "e")]);
        assert_eq!(names(&state.dependencies(&["a"], 1)), ["b", "e"]);
        assert_eq!(names(&state.dependencies(&["a"], 3)), ["b", "e", "c", "d"]);
        assert_eq!(names(&state.dependencies(&["a", "b"], 1)), ["e", "c"]);
    }

    #[test]
    fn a_dependency_cycle_is_walked_once() {
        let state = depends_on_state(&[("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(names(&state.dependencies(&["a"], 10)), ["b", "c"]);
    }

    #[test]
    fn decisions_in_a_path_match_its_files_and_directories() {
        let mut state = empty_state();
        state
            .decisions
            .insert("z-load".into(), anchored("store", &["src/store/load.rs"]));
        state
            .decisions
            .insert("a-mcp".into(), anchored("mcp", &["src/mcp/mod.rs"]));
        state
            .decisions
            .insert("b-dir".into(), anchored("store", &["src/store"]));
        state
            .decisions
            .insert("none".into(), sample_decision("store"));

        assert_eq!(
            names(&state.decisions_in("src/store/load.rs")),
            ["b-dir", "z-load"]
        );
        assert_eq!(
            names(&state.decisions_in("src")),
            ["a-mcp", "b-dir", "z-load"]
        );
        assert_eq!(names(&state.decisions_in("src/store/other.rs")), ["b-dir"]);
        assert!(state.decisions_in("src/sto").is_empty());
        let load = &state.decisions["z-load"];
        assert_eq!(anchors_in(load, "src/store").len(), 1);
        assert!(anchors_in(load, "src/mcp").is_empty());
    }

    #[test]
    fn a_path_is_normalized_to_its_one_spelling() {
        for (path, normalized) in [
            ("./src/store/", "src/store"),
            ("src//store///load.rs", "src/store/load.rs"),
            ("src", "src"),
        ] {
            assert_eq!(normalize_path(path).unwrap(), normalized, "{path}");
        }
    }

    #[test]
    fn a_path_outside_the_code_root_is_refused() {
        for (path, fault) in [
            ("/etc/passwd", AnchorFault::Absolute),
            ("../escape", AnchorFault::Traversal),
            ("src\\x.rs", AnchorFault::Backslash),
            ("C:/x.rs", AnchorFault::Absolute),
            ("", AnchorFault::EmptyPath),
            ("./", AnchorFault::EmptyPath),
        ] {
            assert_eq!(normalize_path(path), Err(fault), "{path:?}");
        }
    }
}
