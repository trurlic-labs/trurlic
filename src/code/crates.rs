//! The crates of a tree: which directory each file's crate is, which are
//! workspaces, and the type-defining macros each crate holds, which an
//! invocation in it reaches by name (`expand/resolve.rs`).
//!
//! A file's crate is the nearest directory above it with a `Cargo.toml`,
//! or the file alone when there is none; its workspace is the nearest
//! directory from the crate's up whose `Cargo.toml` has a `workspace`
//! table. A walked file's crate is read from the walked manifests alone.
//! A file an anchor brought from outside the walk is read from those and
//! the manifests the anchors brought. A manifest an anchor brings thus
//! governs no file the walk took, so what one anchor resolves to never
//! depends on the others. Only walked files bring macros to their crate.
//!
//! A name reaches the file's own macros of it first: a `macro_rules!`
//! macro is in textual scope where it is defined, so it shadows one of
//! another module. Else it reaches its crate's of the name, else the
//! `#[macro_export]` ones of its workspace. A name several macros answer
//! reaches each of them, and the expansion defines only what they agree
//! on.

use std::collections::{BTreeMap, BTreeSet};

use super::expand::{Files, Held};
use super::index::FileIndex;
use super::symbol::Segment;

/// The manifest that makes a directory a crate.
pub(crate) const MANIFEST: &str = "Cargo.toml";

/// The crate directories and workspaces of a set of manifests, sorted.
#[derive(Default)]
struct Layout<'i> {
    crate_roots: Vec<&'i str>,
    workspaces: Vec<&'i str>,
}

impl<'i> Layout<'i> {
    fn of<'a>(manifests: impl Iterator<Item = (&'i str, &'a FileIndex)>) -> Self {
        let mut layout = Self::default();
        for (path, index) in manifests {
            layout.crate_roots.push(parent(path));
            if has_workspace_table(index) {
                layout.workspaces.push(parent(path));
            }
        }
        // A path sorts before its siblings that extend its name (`a` before
        // `a-b`), but `a/b` after `a-b`: the parents need their own sort.
        for found in [&mut layout.crate_roots, &mut layout.workspaces] {
            found.sort_unstable();
            found.dedup();
        }
        layout
    }

    /// The crate directory of `path`, or `path` itself outside every crate.
    fn crate_of<'p>(&self, path: &'p str) -> &'p str {
        ancestors(path)
            .find(|dir| self.crate_roots.binary_search(dir).is_ok())
            .unwrap_or(path)
    }

    /// The workspace of the crate `owner`; none outside every crate.
    fn workspace_of<'p>(&self, owner: &'p str) -> Option<&'p str> {
        self.crate_roots.binary_search(&owner).ok()?;
        std::iter::once(owner)
            .chain(ancestors(owner))
            .find(|dir| self.workspaces.binary_search(dir).is_ok())
    }
}

/// The macros of one crate, or of every crate's exports, by name.
type ByName<'i> = BTreeMap<&'i str, Vec<Held<'i>>>;

/// The crates of the tree, which directories are workspaces, and the
/// type-defining macros of each crate.
pub(super) struct Crates<'i> {
    /// Each crate's directory (`""` for the code root) to its macros, and
    /// each lone file outside a crate to its own.
    macros: BTreeMap<&'i str, ByName<'i>>,
    /// The `#[macro_export]` macros of every crate.
    exported: ByName<'i>,
    /// What the walked manifests say: what governs a walked file.
    walked: Layout<'i>,
    /// What those and the manifests an anchor brought say: what governs a
    /// file the walk did not take.
    all: Layout<'i>,
    /// The files the walk did not take.
    outside: BTreeSet<&'i str>,
}

impl<'i> Crates<'i> {
    /// The crates of `files`, those the walk took, and of `outside`, the
    /// files an anchor brought: the macros of `files`' Rust files alone.
    pub(super) fn of(files: &Files<'i>, outside: &Files<'i>) -> Self {
        let manifests = |set: &Files<'i>| -> Vec<(&'i str, &'i FileIndex)> {
            set.iter()
                .filter(|(path, _)| file_name(path) == MANIFEST)
                .map(|(path, (_, index))| (*path, *index))
                .collect()
        };
        let walked = manifests(files);
        let brought = manifests(outside);
        let mut crates = Self {
            macros: BTreeMap::new(),
            exported: BTreeMap::new(),
            walked: Layout::of(walked.iter().copied()),
            all: Layout::of(walked.iter().chain(&brought).copied()),
            outside: outside.keys().copied().collect(),
        };
        for (&path, (_, index)) in files {
            let owner = crates.crate_of(path);
            for held in held_in_file(path, owner, index) {
                let name = held.found.name.as_str();
                if held.found.exported {
                    crates.exported.entry(name).or_default().push(held);
                }
                let own = crates.macros.entry(owner).or_default();
                own.entry(name).or_default().push(held);
            }
        }
        crates
    }

    fn layout_of(&self, path: &str) -> &Layout<'i> {
        match self.outside.contains(path) {
            true => &self.all,
            false => &self.walked,
        }
    }

    /// The crate directory of `path`, or `path` itself outside every crate.
    pub(super) fn crate_of<'p>(&self, path: &'p str) -> &'p str {
        self.layout_of(path).crate_of(path)
    }

    /// The macros an invocation in `index`, the file at `path`, reaches.
    pub(super) fn reach<'a>(&'a self, path: &'i str, index: &'i FileIndex) -> Reach<'a, 'i> {
        let layout = self.layout_of(path);
        let owner = layout.crate_of(path);
        Reach {
            crates: self,
            own: held_in_file(path, owner, index).collect(),
            owner,
            workspace: layout.workspace_of(owner),
        }
    }

    /// The macros of `name` the crate directory `owner` holds.
    fn held_in(&self, owner: &str, name: &str) -> Vec<Held<'i>> {
        self.macros
            .get(owner)
            .and_then(|own| own.get(name))
            .cloned() // clone: a lookup's macros are its caller's to keep
            .unwrap_or_default()
    }
}

/// The macros of `index`, the file at `path` of the crate `owner`.
fn held_in_file<'i>(
    path: &'i str,
    owner: &'i str,
    index: &'i FileIndex,
) -> impl Iterator<Item = Held<'i>> {
    index
        .rust
        .macros
        .iter()
        .enumerate()
        .map(move |(at, found)| Held {
            found,
            file: path,
            at,
            owner,
        })
}

/// The macros one file's invocations reach by name: its own, its
/// crate's, its workspace's exports.
pub(super) struct Reach<'a, 'i> {
    crates: &'a Crates<'i>,
    own: Vec<Held<'i>>,
    owner: &'i str,
    workspace: Option<&'i str>,
}

impl<'i> Reach<'_, 'i> {
    /// The macros `name` reaches from the file.
    pub(super) fn named(&self, name: &str) -> Vec<Held<'i>> {
        let own: Vec<Held<'i>> = self
            .own
            .iter()
            .filter(|held| held.found.name == name)
            .copied()
            .collect();
        if !own.is_empty() {
            return own;
        }
        let crate_macros = self.crates.held_in(self.owner, name);
        if !crate_macros.is_empty() {
            return crate_macros;
        }
        let Some(workspace) = self.workspace else {
            return Vec::new();
        };
        self.crates
            .exported
            .get(name)
            .into_iter()
            .flatten()
            .filter(|held| is_within(held.owner, workspace))
            .copied()
            .collect()
    }

    /// The macros of `name` the crate directory `owner` holds, which a
    /// path through `$crate::` names.
    pub(super) fn in_crate(&self, owner: &str, name: &str) -> Vec<Held<'i>> {
        self.crates.held_in(owner, name)
    }
}

/// Whether a manifest's index defines a top-level `workspace` table.
fn has_workspace_table(index: &FileIndex) -> bool {
    let workspace = [Segment::name("workspace")];
    index.definitions.iter().any(|d| {
        index
            .scopes
            .get(d.scope.at())
            .is_some_and(|scope| scope.parent.is_none() && scope.name == workspace)
    })
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The directory of `path`; `""` for the code root.
pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The directories above `path`, nearest first, the code root (`""`)
/// last.
pub(super) fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(path);
    std::iter::from_fn(move || {
        let current = rest?;
        rest = (!current.is_empty()).then(|| parent(current));
        rest
    })
}

/// Whether `dir` is `root` or under it.
pub(super) fn is_within(dir: &str, root: &str) -> bool {
    root.is_empty()
        || dir
            .strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}
