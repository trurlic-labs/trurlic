//! Test helpers of the store: stores on disk, sample nodes and states,
//! and the graph's own git as a test reads it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::Store;
use crate::store::format::{Canonical, canonical};
use crate::store::model::{
    Anchor, COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, FORMAT_VERSION, NodeFile,
    PROJECT_COMPONENT, PROJECT_FILE, Placement, Project, ProjectState, Quality, STATE_DIR,
    STORE_DIR, Status, Timestamp,
};

pub fn setup_store(dir: &Path) -> Store {
    setup_store_with_version(dir, &FORMAT_VERSION.to_string())
}

/// An empty store whose `project.toml` states `version` as its
/// `format_version`, whatever the text, beside the fields of format 0.5.
pub fn setup_store_with_version(dir: &Path, version: &str) -> Store {
    let root = dir.join(STORE_DIR);
    fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
    fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
    fs::create_dir_all(root.join(STATE_DIR)).unwrap();
    let project = format!(
        "format_version = \"{version}\"\nname = \"test-project\"\n\
         description = \"A test project\"\nplacement = \"tracked\"\n"
    );
    fs::write(root.join(PROJECT_FILE), project).unwrap();
    Store::at(root)
}

/// An empty `shared` store, whose repository holds the `init` commit.
pub fn shared_store(dir: &Path) -> Store {
    Store::create(dir, "test-project", Placement::Shared).unwrap()
}

/// `git <args>` in the graph directory of `store`, as a person with no
/// user or system configuration; it must succeed.
pub fn graph_git(store: &Store, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(store.root())
        .args([
            "-c",
            "user.name=someone",
            "-c",
            "user.email=someone@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The messages of the graph's history, newest first, each trimmed of
/// the line breaks around it.
pub fn graph_log(store: &Store) -> Vec<String> {
    graph_git(store, &["log", "--format=%B%x00"])
        .split('\0')
        .map(|message| message.trim_matches('\n').to_owned())
        .filter(|message| !message.is_empty())
        .collect()
}

/// The files the commit `rev` changed, sorted.
pub fn changed_in(store: &Store, rev: &str) -> Vec<String> {
    let names = graph_git(
        store,
        &["show", "--name-only", "--format=", "--no-renames", rev],
    );
    let mut names: Vec<String> = names.lines().map(str::to_owned).collect();
    names.sort();
    names
}

/// `value` written to `file` in its canonical form, outside any commit.
pub fn write_node<T: Canonical>(store: &Store, file: &NodeFile, value: &T) {
    fs::write(store.path_of(file), canonical(value)).unwrap();
}

pub fn ts() -> Timestamp {
    "2025-06-01T10:00:00Z".parse().unwrap()
}

pub fn sample_component() -> Component {
    Component {
        description: "A sample component".into(),
        connects_to: std::collections::BTreeSet::new(),
    }
}

/// An accepted decision of `component` with no sets and no origin.
pub fn sample_decision(component: &str) -> Decision {
    Decision {
        component: component.into(),
        choice: "Choice of the sample".into(),
        reason: "Reason of the sample decision".into(),
        alternatives: vec![],
        tags: std::collections::BTreeSet::new(),
        status: Status::Accepted { via: None },
        origin: None,
        created: ts(),
        anchors: std::collections::BTreeSet::new(),
        depends_on: std::collections::BTreeSet::new(),
    }
}

/// A `[quality]` pattern no cache holds over [`coin_flips`]: its lazy
/// DFA tracks where the last twenty-one letters hold an `a`.
pub const OUTGROWS_ITS_CACHE: &str = "(?:a|b)*a(?:a|b){20}$";

/// `len` letters `a` and `b` in an order fixed by `seed`, with no
/// pattern to them a short window repeats.
pub fn coin_flips(len: usize, seed: u64) -> String {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if state >> 63 == 0 { 'a' } else { 'b' }
        })
        .collect()
}

/// [`sample_decision`] anchored at each of `files`.
pub fn anchored(component: &str, files: &[&str]) -> Decision {
    let mut decision = sample_decision(component);
    decision.anchors = files
        .iter()
        .map(|&file| Anchor::new(file.into(), None).unwrap())
        .collect();
    decision
}

/// A state with no node.
pub fn empty_state() -> ProjectState {
    ProjectState {
        project: Project {
            format_version: FORMAT_VERSION,
            name: "test".into(),
            description: String::new(),
            placement: Placement::Tracked,
            quality: Quality::default(),
        },
        components: BTreeMap::new(),
        decisions: BTreeMap::new(),
        non_canonical: Vec::new(),
        code_root: PathBuf::new(),
    }
}

/// Project-wide decisions joined by `depends_on` edges in the given
/// order: a graph whose only issues are the ones the edges make.
pub fn depends_on_state(edges: &[(&str, &str)]) -> ProjectState {
    let mut state = empty_state();
    for &(from, to) in edges {
        for name in [from, to] {
            state
                .decisions
                .entry(name.to_owned())
                .or_insert_with(|| sample_decision(PROJECT_COMPONENT));
        }
        if let Some(decision) = state.decisions.get_mut(from) {
            decision.depends_on.insert(to.to_owned());
        }
    }
    state
}
