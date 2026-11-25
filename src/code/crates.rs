//! The crates of a tree: which directory each file's crate is, which are
//! workspaces, and the type-defining macros each crate holds, the arms of
//! which an invocation in it sees (`expand/table.rs`).
//!
//! A file's crate is the nearest directory above it with a `Cargo.toml`,
//! or the file alone when there is none; its workspace is the nearest
//! directory from the crate's up whose `Cargo.toml` has a `workspace`
//! table. A walked file's crate is read from the walked manifests alone.
//! A file an anchor brought from outside the walk is read from those and
//! the manifests the anchors brought. A manifest an anchor brings thus
//! governs no file the walk took, so what one anchor resolves to never
//! depends on the others. Only walked files bring macros.

use std::collections::{BTreeMap, BTreeSet};

use super::expand::{Arms, Files, Held};
use super::index::FileIndex;
use super::index::TypeMacro;
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

/// The crates of the tree, which directories are workspaces, and the
/// type-defining macros of each crate.
pub(super) struct Crates<'i> {
    /// Each crate's directory (`""` for the code root) to its macros, and
    /// each lone file outside a crate to its own.
    macros: BTreeMap<&'i str, Vec<&'i TypeMacro>>,
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
            walked: Layout::of(walked.iter().copied()),
            all: Layout::of(walked.iter().chain(&brought).copied()),
            outside: outside.keys().copied().collect(),
        };
        for (&path, (_, index)) in files {
            if !index.rust.macros.is_empty() {
                let owner = crates.crate_of(path);
                crates
                    .macros
                    .entry(owner)
                    .or_default()
                    .extend(&index.rust.macros);
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

    /// The arms an invocation in the file at `path` sees but its own,
    /// resolved: its crate's macros, then its workspace's exported ones its
    /// crate does not shadow by name. A hand-off reaches the macro of its
    /// name these hold, or through `$crate::` the one of the crate holding
    /// the macro that hands it on.
    pub(super) fn arms_of(&self, path: &str) -> Arms<'i> {
        let layout = self.layout_of(path);
        let owner = layout.crate_of(path);
        let mut arms = Arms::default();
        let own = self.macros.get_key_value(owner).into_iter();
        for (dir, found) in own.flat_map(|(dir, found)| found.iter().map(move |f| (*dir, *f))) {
            arms.add(Held {
                found,
                owner: dir,
                exported: false,
            });
        }
        let workspace = layout.workspace_of(owner);
        let shadowed = arms.names();
        let exported = self
            .macros
            .iter()
            .filter(|(dir, _)| workspace.is_some_and(|root| is_within(dir, root)))
            .flat_map(|(dir, found)| found.iter().map(move |found| (*dir, *found)))
            .filter(|(_, found)| found.exported && !shadowed.contains(found.name.as_str()));
        for (dir, found) in exported {
            arms.add(Held {
                found,
                owner: dir,
                exported: true,
            });
        }
        arms.resolve(|arms, from, hand_off| match hand_off.from_crate {
            true => self.held_in(from.owner, &hand_off.macro_name),
            false => arms.held(&hand_off.macro_name),
        });
        arms
    }

    /// The macro of `name` the crate directory `owner` holds.
    pub(super) fn held_in(&self, owner: &str, name: &str) -> Option<Held<'i>> {
        let (dir, found) = self.macros.get_key_value(owner)?;
        let found = found.iter().find(|found| found.name == name)?;
        Some(Held {
            found,
            owner: dir,
            exported: false,
        })
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
