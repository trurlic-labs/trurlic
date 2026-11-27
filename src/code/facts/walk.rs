//! The `mod` walk over a tree's Rust files, which carries a state from
//! each target's root down the declarations that reach each file.
//!
//! The walk goes forward from each crate's targets as cargo finds them
//! without a manifest table: `src/lib.rs`, `src/main.rs`, `build.rs`, and
//! each `src/bin/`, `tests/`, `benches/` and `examples/` file (`x.rs` or
//! `x/main.rs`); a target a manifest's `path =` names elsewhere is not
//! read. From each file it follows the `mod name;` declarations the file
//! records, to `name.rs` or `name/mod.rs` in the module's directory, or
//! to the file a relative `#[path]` names (an absolute one names no file
//! of the tree); the module's directory takes a segment for each inline
//! `mod` around the declaration, none for a function or any other item
//! it stands in. With both `name.rs` and `name/mod.rs` present it follows
//! `name.rs`, and names the other to the state, which rustc refuses.
//!
//! A file an anchor brings in from outside the walk is a root or a
//! module like any other, but the walk does not follow its own
//! declarations: what it declares is read for no other file.

use std::collections::{BTreeMap, BTreeSet, btree_map};

use super::crates::{Crates, Files};
use super::path::{file_name, join, parent, resolve};
use crate::code::index::{DefinitionKind, FileIndex, FileKind, ModuleFile, Role, ScopeId, Segment};

/// The files of a target's root in a crate, by their directory.
const ROOT_FILES: [&str; 3] = ["src/lib.rs", "src/main.rs", "build.rs"];

/// The directories each of whose files is a target's root.
const ROOT_DIRS: [&str; 4] = ["src/bin", "tests", "benches", "examples"];

/// What the walk carries to each file: a state that only rises, so a file
/// is followed again only when the state it is reached with rises. Each
/// implementor rises at most once, so a file is followed at most twice
/// and the walk is linear in the tree.
pub(super) trait Reach<'i>: Sized {
    /// What every state of one walk builds on.
    type Shared;

    /// The state the target's root at `root` starts with.
    fn root(root: &'i str, crates: &Crates<'i>) -> Self;

    /// The state the file `declared` names is reached with from a file
    /// reached with this one.
    fn through(&self, declared: &Declared<'i>, shared: &mut Self::Shared) -> Self;

    /// The state the second file of `declared` is reached with, when it
    /// has one; `None` leaves that file to other declarations.
    fn second_file(&self, declared: &Declared<'i>, shared: &mut Self::Shared) -> Option<Self>;

    /// Join `other` into this state: whether it rose.
    fn join(&mut self, other: Self) -> bool;
}

/// A `mod name;` declaration the walk follows.
pub(super) struct Declared<'i> {
    /// The file it names.
    pub(super) file: &'i str,
    /// `name/mod.rs` when `file` is `name.rs` and both are present.
    pub(super) second_file: Option<&'i str>,
    /// Test code when the declaration is.
    pub(super) role: Role,
    /// The names of the inline modules around it, outermost first, and
    /// its own; `None` when a function or another item that is no module
    /// encloses it, as such a block's module has no path.
    pub(super) segments: Option<Vec<&'i str>>,
}

/// The state each Rust file of `files` and `outside` that a target's walk
/// reaches is reached with, joined over every declaration that reaches
/// it; `crates` is of the same files. The declarations of `outside`'s
/// files are not followed.
pub(super) fn walk<'i, R: Reach<'i>>(
    files: &Files<'i>,
    outside: &Files<'i>,
    crates: &Crates<'i>,
    shared: &mut R::Shared,
) -> BTreeMap<&'i str, R> {
    let roots = roots(files, outside, crates);
    let find = |candidate: &str| {
        files
            .get_key_value(candidate)
            .or_else(|| outside.get_key_value(candidate))
            .map(|(&found, _)| found)
    };
    let mut reached: BTreeMap<&str, R> = BTreeMap::new();
    let mut pending: Vec<(&str, R)> = roots
        .iter()
        .map(|&root| (root, R::root(root, crates)))
        .collect();
    while let Some((path, state)) = pending.pop() {
        let state = match reached.entry(path) {
            btree_map::Entry::Vacant(vacant) => vacant.insert(state),
            btree_map::Entry::Occupied(occupied) => {
                let joined = occupied.into_mut();
                if !joined.join(state) {
                    continue;
                }
                joined
            }
        };
        let Some((_, index)) = files.get(path) else {
            continue;
        };
        let declaring = Declaring::of(path, roots.contains(path), index);
        for module in &index.rust.module_files {
            let Some(declared) = declaring.declared(module, find) else {
                continue;
            };
            if let Some(second) = declared.second_file
                && let Some(reached) = state.second_file(&declared, shared)
            {
                pending.push((second, reached));
            }
            pending.push((declared.file, state.through(&declared, shared)));
        }
    }
    reached
}

/// The Rust files of `files` and `outside` that are a target's root.
fn roots<'i>(files: &Files<'i>, outside: &Files<'i>, crates: &Crates<'i>) -> BTreeSet<&'i str> {
    files
        .iter()
        .chain(outside)
        .filter(|(_, (kind, _))| *kind == FileKind::Rust)
        .map(|(&path, _)| path)
        .filter(|path| relative_to_crate(path, crates).is_some_and(is_target_root))
        .collect()
}

/// Whether `relative`, a path from its crate's directory, is a target's
/// root.
fn is_target_root(relative: &str) -> bool {
    ROOT_FILES.contains(&relative)
        || ROOT_DIRS.iter().any(|dir| {
            let rest = relative
                .strip_prefix(dir)
                .and_then(|rest| rest.strip_prefix('/'));
            rest.is_some_and(|rest| match rest.split_once('/') {
                None => rest.ends_with(".rs"),
                Some((_, file)) => file == "main.rs",
            })
        })
}

/// `path` from its crate's directory; `None` outside every crate.
pub(super) fn relative_to_crate<'p>(path: &'p str, crates: &Crates<'_>) -> Option<&'p str> {
    match crates.crate_of(path)? {
        "" => Some(path),
        root => path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/')),
    }
}

/// A file whose module declarations are followed.
struct Declaring<'i> {
    path: &'i str,
    is_root: bool,
    index: &'i FileIndex,
    /// Its scopes that are modules: the directories of a module file
    /// declared inside them.
    modules: BTreeSet<ScopeId>,
}

impl<'i> Declaring<'i> {
    fn of(path: &'i str, is_root: bool, index: &'i FileIndex) -> Self {
        let modules = index
            .definitions
            .iter()
            .filter(|definition| definition.kind == DefinitionKind::Module)
            .map(|definition| definition.scope)
            .collect();
        Self {
            path,
            is_root,
            index,
            modules,
        }
    }

    /// The declaration `module`, declared here, with the file it names as
    /// `find` finds it by its path; `None` when there is none.
    fn declared(
        &self,
        module: &ModuleFile,
        find: impl Fn(&str) -> Option<&'i str>,
    ) -> Option<Declared<'i>> {
        let name = self.name_of(module.scope)?;
        let inline_names = self.inline_modules(module.scope);
        // A crate's root and a `mod.rs` hold their modules beside them; any
        // other `x.rs` holds them in `x/`.
        let mut dir = parent(self.path).to_owned();
        if !self.is_root && file_name(self.path) != "mod.rs" {
            dir = join(&dir, file_name(self.path).strip_suffix(".rs")?);
        }
        let inline = inline_names
            .iter()
            .fold(dir, |dir, module| join(&dir, module));
        let candidates = match &module.path {
            Some(named) if inline_names.is_empty() => vec![resolve(parent(self.path), named)],
            Some(named) => vec![resolve(&inline, named)],
            None => vec![
                resolve(&inline, &format!("{name}.rs")),
                resolve(&inline, &format!("{name}/mod.rs")),
            ],
        };
        let mut found = candidates
            .into_iter()
            .filter_map(|candidate| find(&candidate?));
        let file = found.next()?;
        let segments = (!self.is_in_item(module.scope)).then(|| {
            let mut segments = inline_names;
            segments.push(name);
            segments
        });
        Some(Declared {
            file,
            second_file: found.next(),
            role: module.role,
            segments,
        })
    }

    /// Whether a scope around the scope `id` is an item that is no module.
    fn is_in_item(&self, id: ScopeId) -> bool {
        let parent_of = |id: ScopeId| {
            let parent = self.index.scopes.get(id.at())?.parent;
            parent.filter(|parent| *parent < id)
        };
        std::iter::successors(parent_of(id), |current| parent_of(*current))
            .any(|current| !self.modules.contains(&current))
    }

    /// The last name of the scope `id`.
    fn name_of(&self, id: ScopeId) -> Option<&'i str> {
        match self.index.scopes.get(id.at())?.name.last()? {
            Segment::Name(name) | Segment::Impl { name, .. } => Some(name),
        }
    }

    /// The names of the inline modules around the scope `id`, outermost
    /// first. A parent's id is below its child's, so the walk ends.
    fn inline_modules(&self, id: ScopeId) -> Vec<&'i str> {
        let mut names = Vec::new();
        let mut next = self
            .index
            .scopes
            .get(id.at())
            .and_then(|scope| scope.parent);
        while let Some(current) = next.filter(|current| *current < id) {
            if self.modules.contains(&current) {
                names.extend(self.name_of(current));
            }
            next = self
                .index
                .scopes
                .get(current.at())
                .and_then(|scope| scope.parent)
                .filter(|parent| *parent < current);
        }
        names.reverse();
        names
    }
}
