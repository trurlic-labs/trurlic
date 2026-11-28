//! A file's place in the code tree: its crate and the module it is in,
//! read by the `mod` walk (`walk.rs`) for a Rust file and from its
//! directory for every other kind. A module path is an id into the
//! module paths of its tree ([`ModulePaths`]), each a parent and one
//! segment, so a state carries an id and the walk stays linear however
//! deep the paths run; a path is spelled only when read.

use std::collections::BTreeMap;

use super::crates::{Crates, Files};
use super::path::parent;
use super::walk::{Declared, Reach, walk};
use crate::code::index::FileKind;

/// Where a file of the tree stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Place<'i> {
    /// The directory of its crate's manifest, `""` at the code root;
    /// `None` outside every crate. A [`Module::Rust`] file is in the
    /// crate of the target that reaches it; any other file, a Rust one at
    /// no one module included, is in the crate of the nearest manifest
    /// above it.
    pub(crate) crate_dir: Option<&'i str>,
    pub(crate) module: Module<'i>,
}

/// The module a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Module<'i> {
    /// A Rust file the walk reaches at one module: the module's own file,
    /// `name.rs` and `name/mod.rs` alike, so a one-file module and a
    /// directory module are one node. `path` is from the root module of
    /// `root`, the directory of the target's root file, and empty for the
    /// root itself. Targets whose roots share a directory share its root
    /// module (`src/lib.rs` and `src/main.rs`), as a file both declare has
    /// one path in each. An inline `mod` is a segment of the paths below
    /// it and a definition of the file that holds it, never a file of its
    /// own. Two declarations of one file at one path (cfg twins) are one.
    Rust { root: &'i str, path: ModulePath },
    /// A file of a language with no module in its syntax (Python,
    /// TypeScript, JavaScript, C, C++) or of a lexical kind: in its
    /// directory, the tree of these languages being the directory tree.
    Directory,
    /// A Rust file no target's walk reaches (a target a manifest's `path`
    /// names, a stray file): in its directory, in no module.
    Unreached,
    /// A Rust file the walk reaches at no one module path, for the reason
    /// stated: in its directory. A file it declares is at none for the
    /// same reason.
    Unresolved(Unresolved),
}

/// Why a Rust file the walk reaches is at no one module path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unresolved {
    /// Reached at two or more: a second `#[path]` to it, a target's root
    /// that a file also declares, a declaration from targets of two root
    /// directories, or two of the reasons below at once.
    Several,
    /// Declared by a `mod` inside a function or another item that is no
    /// module: a block's module, which no path names.
    InItem,
    /// Declared as `name` where both `name.rs` and `name/mod.rs` are,
    /// which rustc refuses: each of the two is.
    TwoFiles,
}

/// A module path: the root module's, empty, or an id of [`ModulePaths`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ModulePath(Option<usize>);

impl ModulePath {
    /// The root module's path.
    pub(super) const ROOT: Self = Self(None);
}

/// Every module path the walk reaches, each once, as its parent's and its
/// last segment: two paths spelled alike are one id.
#[derive(Debug, Default)]
pub(super) struct ModulePaths<'i> {
    /// By id: the parent and the last segment.
    paths: Vec<(ModulePath, &'i str)>,
    ids: BTreeMap<(ModulePath, &'i str), usize>,
}

impl<'i> ModulePaths<'i> {
    /// `parent` and `segment` below it.
    pub(super) fn child(&mut self, parent: ModulePath, segment: &'i str) -> ModulePath {
        let next = self.paths.len();
        let id = *self.ids.entry((parent, segment)).or_insert(next);
        if id == next {
            self.paths.push((parent, segment));
        }
        ModulePath(Some(id))
    }

    /// `segment` below `parent`, when a path of the tree is.
    pub(super) fn find(&self, parent: ModulePath, segment: &str) -> Option<ModulePath> {
        let id = self.ids.get(&(parent, segment))?;
        Some(ModulePath(Some(*id)))
    }

    /// The path above `path`; `None` for the root module's.
    pub(super) fn parent(&self, path: ModulePath) -> Option<ModulePath> {
        let (parent, _) = self.paths.get(path.0?)?;
        Some(*parent)
    }

    /// The last segment of `path`; `None` for the root module's.
    pub(super) fn last(&self, path: ModulePath) -> Option<&'i str> {
        let (_, segment) = self.paths.get(path.0?)?;
        Some(*segment)
    }

    /// The segments of `path`, outermost first.
    pub(super) fn segments(&self, path: ModulePath) -> Vec<&'i str> {
        let mut segments = Vec::new();
        let mut next = path.0;
        while let Some((parent, segment)) = next.and_then(|id| self.paths.get(id)) {
            segments.push(*segment);
            next = parent.0;
        }
        segments.reverse();
        segments
    }
}

/// The place of each of `files`, the walked files, those with no index
/// standing as files with no declaration; `crates` is of the same files.
/// The module paths the places hold are added to `paths`.
pub(super) fn places<'i>(
    files: &Files<'i>,
    crates: &Crates<'i>,
    paths: &mut ModulePaths<'i>,
) -> BTreeMap<&'i str, Place<'i>> {
    let mut reached = walk::<ReachedAt<'_>>(files, &Files::new(), crates, paths);
    files
        .iter()
        .map(|(&path, (kind, _))| {
            let at_directory = |module| Place {
                crate_dir: crates.crate_of(path),
                module,
            };
            let place = match (*kind == FileKind::Rust, reached.remove(path)) {
                (false, _) => at_directory(Module::Directory),
                (true, None) => at_directory(Module::Unreached),
                (true, Some(ReachedAt::Unresolved(why))) => at_directory(Module::Unresolved(why)),
                (
                    true,
                    Some(ReachedAt::Module {
                        crate_dir,
                        root,
                        path,
                    }),
                ) => Place {
                    crate_dir,
                    module: Module::Rust { root, path },
                },
            };
            (path, place)
        })
        .collect()
}

/// The module the walk reaches a file at. A state rises once, to
/// `Unresolved(Several)`, so a file is followed at most twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReachedAt<'i> {
    /// A [`Module::Rust`] of the crate `crate_dir`.
    Module {
        crate_dir: Option<&'i str>,
        root: &'i str,
        path: ModulePath,
    },
    Unresolved(Unresolved),
}

impl<'i> Reach<'i> for ReachedAt<'i> {
    type Shared = ModulePaths<'i>;

    fn root(root: &'i str, crates: &Crates<'i>) -> Self {
        Self::Module {
            crate_dir: crates.crate_of(root),
            root: parent(root),
            path: ModulePath::ROOT,
        }
    }

    fn through(&self, declared: &Declared<'i>, paths: &mut ModulePaths<'i>) -> Self {
        let Self::Module {
            crate_dir,
            root,
            path,
        } = *self
        else {
            return *self;
        };
        if declared.second_file.is_some() {
            return Self::Unresolved(Unresolved::TwoFiles);
        }
        let Some(segments) = &declared.segments else {
            return Self::Unresolved(Unresolved::InItem);
        };
        let path = segments
            .iter()
            .fold(path, |path, segment| paths.child(path, segment));
        Self::Module {
            crate_dir,
            root,
            path,
        }
    }

    fn second_file(&self, declared: &Declared<'i>, paths: &mut ModulePaths<'i>) -> Option<Self> {
        Some(self.through(declared, paths))
    }

    fn join(&mut self, other: Self) -> bool {
        if *self == Self::Unresolved(Unresolved::Several) || *self == other {
            return false;
        }
        *self = Self::Unresolved(Unresolved::Several);
        true
    }
}
