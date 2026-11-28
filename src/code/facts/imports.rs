//! The imports between a tree's Rust files: each path a file's index
//! holds (`index::Import`) resolved to the file of the module it goes
//! into, by name and path alone ([`Resolver`]).
//!
//! The edge is an approximation by names and paths: no macro is expanded,
//! no type is inferred, no glob is followed, no `cfg` is evaluated. What
//! resolves is resolved by the rules [`Resolver`] states; what they cannot
//! settle is stated unresolved. Read by names and paths alone, they cannot
//! see, and so may resolve past or to another file than rustc:
//!
//! - what a derive or an attribute macro makes or rewrites in the item it
//!   marks: an item in its module, or a `use` an attribute macro on a
//!   function prepends to its body;
//! - an item in a block inside a field's type (`[u8; { struct x; .. }]`),
//!   which the walk of a file's items does not enter;
//! - which `cfg` branch is built: every branch's items and paths are read
//!   at once;
//! - a `self` or `super` path in a `macro_rules!` body, read from the
//!   module that defines the macro, where rustc reads it from each module
//!   that invokes it;
//! - the 2015 edition, whose `use` paths start at the crate root.
//!
//! Nor is a dependency read that no path into the crate spells where it
//! stands: a name an `extern crate` binds, a path a macro puts together
//! from tokens handed to it (`$root::x`), a method call or a field access.

use std::collections::{BTreeMap, BTreeSet};

use super::crates::Files;
use super::places::{Module, ModulePath, ModulePaths, Place};
use crate::code::index::{
    Before, Binds, DefinitionKind, FileIndex, ImportKind, Role, ScopeId, Segment,
};
use crate::code::parse::{Led, lead};

/// The imports of one file into one file, or into none it resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileEdge<'i> {
    /// The file they resolve to; `None` for imports that do not resolve.
    pub(crate) to: Option<&'i str>,
    /// The imports that are dependencies of the file: every import but a
    /// re-export.
    pub(crate) imports: usize,
    /// The re-exports, `use`s a visibility opens, which are none.
    pub(crate) reexports: usize,
    /// Whether every one of them, dependency or re-export, is test code.
    pub(crate) test: bool,
}

/// The edges of each walked Rust file that imports anything, by its path:
/// its unresolved imports first, then its imports of each file in path
/// order. `paths` gains the path of each inline module; `role` gives the
/// role of a file the tree makes test code whole.
pub(super) fn file_edges<'i>(
    files: &Files<'i>,
    places: &BTreeMap<&'i str, Place<'i>>,
    paths: &mut ModulePaths<'i>,
    role: impl Fn(&str) -> Role,
) -> BTreeMap<&'i str, Vec<FileEdge<'i>>> {
    let mut resolver = Resolver::of(files, places, paths);
    let mut edges = BTreeMap::new();
    for (&file, (_, index)) in files {
        let mut tally: BTreeMap<Option<&str>, FileEdge<'_>> = BTreeMap::new();
        let file_role = role(file);
        for import in &index.rust.imports {
            let to = match &import.kind {
                ImportKind::Unresolvable => None,
                ImportKind::Use(Binds::Glob) | ImportKind::Reexport(Binds::Glob) => {
                    resolver.glob_target(file, import.last)
                }
                ImportKind::Code | ImportKind::Use(_) | ImportKind::Reexport(_) => {
                    resolver.target(file, import.last)
                }
            };
            let edge = tally.entry(to).or_insert(FileEdge {
                to,
                imports: 0,
                reexports: 0,
                test: true,
            });
            // A re-export binds a name and is no dependency of its file.
            match import.kind {
                ImportKind::Code | ImportKind::Use(_) | ImportKind::Unresolvable => {
                    edge.imports += 1;
                }
                ImportKind::Reexport(_) => edge.reexports += 1,
            }
            edge.test &= import.role.or(file_role) == Role::Test;
        }
        if !tally.is_empty() {
            edges.insert(file, tally.into_values().collect());
        }
    }
    edges
}

/// A module: the directory of its root module's root and its path from
/// there.
type ModuleKey<'i> = (&'i str, ModulePath);

/// A step of a file's imports: the file and the step's place.
type StepKey<'i> = (&'i str, usize);

/// What holds a module: a file, and the scope of the inline module in
/// it, `None` for the file's own.
type Holder<'i> = (&'i str, Option<ScopeId>);

/// Where a path has got to after one of its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position<'i> {
    /// Into a module, to be read on.
    Module(ModuleKey<'i>),
    /// Into `child`, a module whose name, `name`, the module above, `of`,
    /// also defines or binds as an item: the module where the path goes
    /// on; where it ends, the one file both are in, or none.
    ModuleOrItem {
        child: ModuleKey<'i>,
        of: ModuleKey<'i>,
        name: &'i str,
    },
    /// Into an item that file defines: what follows is the item's.
    Item(&'i str),
    /// Nowhere it can follow.
    Unresolved,
}

/// What a step gives: its position, or the step whose position it needs
/// first.
enum Advance<'i> {
    Known(Position<'i>),
    Needs(StepKey<'i>),
}

/// The modules of a tree's Rust files, what each defines and binds, and
/// the position of each step resolved so far.
///
/// A path starts in the module it stands in. `crate` (or `$crate`) leads
/// to the root module of the importing file's root directory, `self` to
/// the module, `super` to the one above (`lead`). Each next segment goes
/// into a module below, the longest prefix that is a module winning, an
/// inline module resolving to the file that holds it. At a path's last
/// segment, a child module whose name the module also defines or binds as
/// an item, or may bind by a glob `use` or a macro invocation as an item,
/// could be either. So the path is unresolved there, but where module and
/// item are in one file; with more segments after it, it is the module,
/// and a glob's path is the module. Past the modules, a name the module
/// defines (an item, an enum's variant and an associated item after it
/// alike) ends at the module's file. A name the module binds by a `use`,
/// with a visibility or not, goes where that `use`'s path goes, so
/// re-exports are followed through chains, each step resolved once and a
/// cycle ended as unresolved.
///
/// An import does not resolve when:
///
/// - its file is at no module path, or it stands in a module declared
///   inside an item;
/// - `super` goes above the root;
/// - a name is neither a module nor defined nor bound once: a macro-made
///   item, a glob-imported name, a binding the index does not hold, a
///   macro's metavariable;
/// - a module's file is at no one place;
/// - its last segment is both a child module and an item (above);
/// - it ends at a module, or at a name, that more than one file holds:
///   the root of a library and a binary sharing a directory, `cfg` twins;
/// - the index holds it unresolvable: a path led by a child module's name
///   where that name may be bound anew (`parse/rust/rebinding.rs`).
struct Resolver<'t, 'i> {
    files: &'t Files<'i>,
    paths: &'t ModulePaths<'i>,
    /// The holders of each module.
    holders: BTreeMap<ModuleKey<'i>, Vec<Holder<'i>>>,
    /// The module each holder is.
    modules: BTreeMap<Holder<'i>, ModuleKey<'i>>,
    names: Names<'i>,
    positions: BTreeMap<StepKey<'i>, Position<'i>>,
}

impl<'t, 'i> Resolver<'t, 'i> {
    fn of(
        files: &'t Files<'i>,
        places: &BTreeMap<&'i str, Place<'i>>,
        paths: &'t mut ModulePaths<'i>,
    ) -> Self {
        let mut holders: BTreeMap<ModuleKey<'i>, Vec<Holder<'i>>> = BTreeMap::new();
        let mut modules = BTreeMap::new();
        for (&file, (_, index)) in files {
            let Some(Module::Rust { root, path }) = places.get(file).map(|place| place.module)
            else {
                continue;
            };
            for (scope, path) in inline_modules(index, path, paths) {
                holders.entry((root, path)).or_default().push((file, scope));
                modules.insert((file, scope), (root, path));
            }
        }
        let names = Names::of(files, &modules);
        Self {
            files,
            paths,
            holders,
            modules,
            names,
            positions: BTreeMap::new(),
        }
    }

    /// The file the import of `file` whose last step is `last` resolves
    /// to; `None` when it does not resolve.
    fn target(&mut self, file: &'i str, last: usize) -> Option<&'i str> {
        let position = self.position((file, last));
        self.file_of(position)
    }

    /// The file the glob of the import of `file` whose last step is `last`
    /// takes its names from: a module, whatever item of its name the
    /// module above holds, as only a module or an enum is globbed.
    fn glob_target(&mut self, file: &'i str, last: usize) -> Option<&'i str> {
        match self.position((file, last)) {
            Position::Module(module) | Position::ModuleOrItem { child: module, .. } => {
                self.only_file(module)
            }
            Position::Item(file) => Some(file),
            Position::Unresolved => None,
        }
    }

    /// The one file that holds `module`; `None` when several do.
    fn only_file(&self, module: ModuleKey<'i>) -> Option<&'i str> {
        only(self.holders.get(&module)?.iter().map(|(file, _)| *file))
    }

    /// The position after the step `key`, each step it needs resolved
    /// first from an explicit stack, each once; a step that needs one
    /// still being resolved closes a cycle and is unresolved.
    fn position(&mut self, key: StepKey<'i>) -> Position<'i> {
        let mut stack = vec![key];
        let mut open = BTreeSet::from([key]);
        // The position of the step last taken off the stack: `key`'s once
        // the stack is empty, as `key` is at its bottom.
        let mut last = Position::Unresolved;
        while let Some(&top) = stack.last() {
            last = match self.positions.get(&top) {
                Some(position) => *position,
                None => match self.advance(top) {
                    Advance::Known(position) => position,
                    Advance::Needs(next) if open.contains(&next) => Position::Unresolved,
                    Advance::Needs(next) => {
                        stack.push(next);
                        open.insert(next);
                        continue;
                    }
                },
            };
            self.positions.insert(top, last);
            stack.pop();
            open.remove(&top);
        }
        last
    }

    /// The position after the step `key`, from the position before it.
    fn advance(&self, (file, at): StepKey<'i>) -> Advance<'i> {
        let Some(step) = self
            .files
            .get(file)
            .and_then(|(_, index)| index.rust.steps.get(at))
        else {
            return Advance::Known(Position::Unresolved);
        };
        let before = match step.before {
            Before::Start(scope) => match self.modules.get(&(file, scope)) {
                Some(module) => return self.next(*module, &step.segment),
                None => Position::Unresolved,
            },
            Before::Step(before) => match self.positions.get(&(file, before)) {
                Some(position) => *position,
                None => return Advance::Needs((file, before)),
            },
        };
        match before {
            Position::Module(module) | Position::ModuleOrItem { child: module, .. } => {
                self.next(module, &step.segment)
            }
            Position::Item(file) => Advance::Known(Position::Item(file)),
            Position::Unresolved => Advance::Known(Position::Unresolved),
        }
    }

    /// The position after `segment` in `module`, as [`Resolver`] states.
    fn next(&self, module: ModuleKey<'i>, segment: &'i str) -> Advance<'i> {
        let (root, path) = module;
        let parent = |(root, path): ModuleKey<'i>| Some((root, self.paths.parent(path)?));
        match lead(segment, module, (root, ModulePath::ROOT), parent) {
            Led::Module(led) => return Advance::Known(Position::Module(led)),
            Led::AboveRoot => return Advance::Known(Position::Unresolved),
            Led::Name => {}
        }
        let child = self.paths.find(path, segment).map(|child| (root, child));
        let child = child.filter(|child| self.holders.contains_key(child));
        // The item a module's name also is waits for the path's end: a
        // segment with more after it is the module, as a `use`'s own first
        // segment is.
        let opaque = |holders: &[Holder<'i>]| {
            holders
                .iter()
                .any(|holder| self.names.opaque.contains(holder))
        };
        let holders = self.holders.get(&module).map_or(&[][..], Vec::as_slice);
        match (child, self.name_in(module, segment)) {
            (Some(child), None) if !opaque(holders) => Advance::Known(Position::Module(child)),
            (Some(child), _) => Advance::Known(Position::ModuleOrItem {
                child,
                of: module,
                name: segment,
            }),
            (None, Some(named)) => named,
            (None, None) => Advance::Known(Position::Unresolved),
        }
    }

    /// The one file a path that ends at `position` resolves to; `None`
    /// when it resolves to none. A path that ends at a module and an item
    /// of one name resolves where both are in one file.
    fn file_of(&mut self, position: Position<'i>) -> Option<&'i str> {
        match position {
            Position::Item(file) => Some(file),
            Position::Module(module) => self.only_file(module),
            Position::ModuleOrItem { child, of, name } => {
                let module_file = self.only_file(child)?;
                let item = match self.name_in(of, name)? {
                    Advance::Known(item) => item,
                    Advance::Needs(binding) => self.position(binding),
                };
                let item_file = match item {
                    Position::Item(file) => Some(file),
                    Position::Module(module) => self.only_file(module),
                    Position::ModuleOrItem { .. } | Position::Unresolved => None,
                };
                (item_file == Some(module_file)).then_some(module_file)
            }
            Position::Unresolved => None,
        }
    }

    /// Where `name`, as an item, is in `module`: the file of the item it
    /// defines, or where the one `use` that binds it goes; `None` when it
    /// is neither, a module it declares alone included. A name more than
    /// one file defines, or more than one `use` binds, is unresolved.
    fn name_in(&self, module: ModuleKey<'i>, name: &'i str) -> Option<Advance<'i>> {
        let holders = self.holders.get(&module).map_or(&[][..], Vec::as_slice);
        let key = |&(file, scope): &Holder<'i>| (file, scope, name);
        let defines = |holder: &Holder<'i>| self.names.defined.get(&key(holder)) == Some(&true);
        let items: Vec<&str> = holders.iter().filter(|h| defines(h)).map(|h| h.0).collect();
        if !items.is_empty() {
            let one = only(items.into_iter());
            return Some(Advance::Known(
                one.map_or(Position::Unresolved, Position::Item),
            ));
        }
        let bindings: Vec<StepKey<'i>> = holders
            .iter()
            .flat_map(|holder| {
                let lasts = self.names.bound.get(&key(holder));
                let lasts = lasts.map_or(&[][..], Vec::as_slice);
                lasts.iter().map(|last| (holder.0, *last))
            })
            .collect();
        match bindings.as_slice() {
            [] => None,
            [binding] => Some(match self.positions.get(binding) {
                Some(position) => Advance::Known(*position),
                None => Advance::Needs(*binding),
            }),
            [_, _, ..] => Some(Advance::Known(Position::Unresolved)),
        }
    }
}

/// The one file `files` name, however often; `None` for none or several.
fn only<'f>(files: impl Iterator<Item = &'f str>) -> Option<&'f str> {
    let files: BTreeSet<&str> = files.collect();
    let mut files = files.into_iter();
    files.next().filter(|_| files.next().is_none())
}

/// The scope and module path of each module `index`, the index of a file
/// at the module path `path`, holds: its own, `None`, and each inline one
/// no item but a module holds, whose path `paths` gains.
fn inline_modules<'i>(
    index: &'i FileIndex,
    path: ModulePath,
    paths: &mut ModulePaths<'i>,
) -> Vec<(Option<ScopeId>, ModulePath)> {
    let declared: BTreeSet<ScopeId> = index.rust.module_files.iter().map(|m| m.scope).collect();
    let mut inline: Vec<ScopeId> = index
        .definitions
        .iter()
        .filter(|d| d.kind == DefinitionKind::Module && !declared.contains(&d.scope))
        .map(|d| d.scope)
        .collect();
    // A scope's parent is below it, so a module's path is known before
    // the paths of the modules inside it.
    inline.sort_unstable();
    inline.dedup();
    let mut found: BTreeMap<Option<ScopeId>, ModulePath> = BTreeMap::from([(None, path)]);
    for scope in inline {
        let Some(own) = index.scopes.get(scope.at()) else {
            continue;
        };
        let (Some(&above), [Segment::Name(name)]) = (found.get(&own.parent), own.name.as_slice())
        else {
            continue;
        };
        found.insert(Some(scope), paths.child(above, name));
    }
    found.into_iter().collect()
}

/// A name in a holder of a module: the file, the inline module's scope or
/// `None` for the file's own, and the name.
type HeldName<'i> = (&'i str, Option<ScopeId>, &'i str);

/// What the holders of a tree's modules define and bind.
struct Names<'i> {
    /// Each name a holder defines: whether anything but a module.
    defined: BTreeMap<HeldName<'i>, bool>,
    /// The last step of each `use` leaf that binds a name in a holder.
    bound: BTreeMap<HeldName<'i>, Vec<usize>>,
    /// The holders a glob `use`, or a macro invocation as an item, may
    /// bind any name in.
    opaque: BTreeSet<Holder<'i>>,
}

impl<'i> Names<'i> {
    /// What each holder of `modules`, among `files`, defines and binds.
    fn of(files: &Files<'i>, modules: &BTreeMap<Holder<'i>, ModuleKey<'i>>) -> Self {
        let mut names = Self {
            defined: BTreeMap::new(),
            bound: BTreeMap::new(),
            opaque: BTreeSet::new(),
        };
        for (&file, (_, index)) in files {
            if modules.contains_key(&(file, None)) {
                names.add(file, index);
            }
        }
        names
    }

    /// Add what `index`, the file at `file`, defines and binds.
    fn add(&mut self, file: &'i str, index: &'i FileIndex) {
        for definition in &index.definitions {
            let Some(scope) = index.scopes.get(definition.scope.at()) else {
                continue;
            };
            let [Segment::Name(name)] = scope.name.as_slice() else {
                continue;
            };
            if definition.kind != DefinitionKind::Reexport {
                let item = definition.kind != DefinitionKind::Module;
                *self.defined.entry((file, scope.parent, name)).or_default() |= item;
            }
        }
        for module in &index.rust.macro_modules {
            self.opaque.insert((file, *module));
        }
        let starts = step_modules(index);
        for import in &index.rust.imports {
            let (ImportKind::Use(binds) | ImportKind::Reexport(binds)) = &import.kind else {
                continue;
            };
            let name = match binds {
                Binds::Last => index
                    .rust
                    .steps
                    .get(import.last)
                    .map(|s| s.segment.as_str()),
                Binds::Alias(name) => Some(name.as_str()),
                Binds::Glob => {
                    if let Some(scope) = starts.get(import.last) {
                        self.opaque.insert((file, *scope));
                    }
                    None
                }
                Binds::Nothing => None,
            };
            if let (Some(name), Some(scope)) = (name, starts.get(import.last)) {
                let lasts = self.bound.entry((file, *scope, name)).or_default();
                lasts.push(import.last);
            }
        }
    }
}

/// The inline module each step of `index` stands in: its path's first
/// step's.
fn step_modules(index: &FileIndex) -> Vec<Option<ScopeId>> {
    let mut modules: Vec<Option<ScopeId>> = Vec::with_capacity(index.rust.steps.len());
    for step in &index.rust.steps {
        let module = match step.before {
            Before::Start(scope) => scope,
            Before::Step(before) => modules.get(before).copied().flatten(),
        };
        modules.push(module);
    }
    modules
}

#[cfg(test)]
mod tests {
    use super::super::tree::CodeTree;
    use crate::code::testing::facts;

    /// The edges of each file of `files`, as the tree spells them.
    fn imports_of(files: &[(&str, &str)]) -> Vec<String> {
        let facts = facts(files, &[]);
        CodeTree::of(&facts).spelled_imports()
    }

    #[test]
    fn a_pub_use_is_a_re_export_apart_and_a_path_through_it_reaches_the_file_defining_the_item() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod model;\nmod store;\nmod user;\npub(crate) use store::Store;\n",
            ),
            ("src/model.rs", "pub struct Anchor;\n"),
            (
                "src/store/mod.rs",
                "mod format;\npub(crate) use format::Store;\npub use crate::model::Anchor;\n",
            ),
            ("src/store/format.rs", "pub struct Store;\n"),
            (
                "src/user.rs",
                "use crate::Store;\nfn f(a: crate::store::Anchor) -> crate::store::format::Store { a }\n",
            ),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/lib.rs -> src/store/format.rs 0 re-exports 1",
                "src/store/mod.rs -> src/model.rs 0 re-exports 1",
                "src/store/mod.rs -> src/store/format.rs 0 re-exports 1",
                "src/user.rs -> src/model.rs 1",
                "src/user.rs -> src/store/format.rs 2",
            ]
        );
    }

    #[test]
    fn a_variant_or_an_associated_item_past_a_module_is_an_import_of_the_module_s_file() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod model;\nmod user;\n"),
            (
                "src/model.rs",
                "pub enum Kind { A }\nimpl Kind { pub fn new() -> Self { Self::A } }\n",
            ),
            (
                "src/user.rs",
                "fn f() { crate::model::Kind::A; crate::model::Kind::new(); }\n",
            ),
        ];

        assert_eq!(imports_of(&files), ["src/user.rs -> src/model.rs 2"]);
    }

    #[test]
    fn a_last_segment_both_a_child_module_and_an_item_is_unresolved_and_a_middle_one_the_module() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod index;\nmod dispatch;\nmod user;\npub(crate) use dispatch::index;\n",
            ),
            ("src/index.rs", "pub struct Thing;\n"),
            ("src/dispatch.rs", "pub fn index() {}\n"),
            (
                "src/user.rs",
                "fn f() { crate::index(); crate::index::Thing; }\n",
            ),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/lib.rs -> src/dispatch.rs 0 re-exports 1",
                "src/user.rs -> ? 1",
                "src/user.rs -> src/index.rs 1",
            ]
        );
    }

    #[test]
    fn a_child_module_whose_name_may_be_bound_anew_is_unresolved() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod x;\nmod y;\nmod c;\nmod d;\n\
                 pub fn f() { use crate::y::*; x::g(); }\n\
                 pub fn h() { mod x { pub fn g() {} } x::g(); }\n\
                 pub fn k() { use crate::y::x; x(); x::g(); }\n",
            ),
            ("src/x.rs", "pub fn g() {}\n"),
            ("src/y.rs", "pub mod x { pub fn g() {} }\npub fn x() {}\n"),
            (
                "src/c.rs",
                "mod w;\nmod v;\nuse v::*;\npub fn f() { self::w(); self::w::T; }\n",
            ),
            ("src/c/w.rs", "pub struct T;\n"),
            ("src/c/v.rs", "pub fn w() {}\n"),
            (
                "src/d.rs",
                "mod x;\nmake!(x);\npub fn f() { self::x(); self::x::g(); }\n",
            ),
            ("src/d/x.rs", "pub fn g() {}\n"),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/c.rs -> ? 1",
                "src/c.rs -> src/c/v.rs 1",
                "src/c.rs -> src/c/w.rs 1",
                "src/d.rs -> ? 1",
                "src/d.rs -> src/d/x.rs 1",
                "src/lib.rs -> ? 3",
                "src/lib.rs -> src/y.rs 2",
            ]
        );
    }

    #[test]
    fn a_path_through_a_macros_metavariable_never_resolves() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod x;\nmacro_rules! reach { ($m:ident) => { crate::$m::g(); crate::x::g() } }\n",
            ),
            ("src/x.rs", "pub fn g() {}\n"),
        ];

        assert_eq!(
            imports_of(&files),
            ["src/lib.rs -> ? 1", "src/lib.rs -> src/x.rs 1"]
        );
    }

    #[test]
    fn a_use_in_a_block_binds_no_name_of_its_module() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod a;\nmod b;\nmod user;\nuse crate::b::*;\nfn f() { use crate::a::X; }\n",
            ),
            ("src/a.rs", "pub struct X;\n"),
            ("src/b.rs", "pub struct X;\n"),
            ("src/user.rs", "use super::X;\n"),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/lib.rs -> src/a.rs 1",
                "src/lib.rs -> src/b.rs 1",
                "src/user.rs -> ? 1",
            ]
        );
    }

    #[test]
    fn an_import_that_does_not_resolve_is_kept_as_unresolved() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod made;\nmod glob;\nmod model;\nmod twice;\n#[path = \"twice.rs\"]\nmod again;\n\
                 mod cycle;\nmod user;\nfn f() { super::above(); }\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            (
                "src/made.rs",
                "macro_rules! make { ($n:ident) => { pub struct $n; } }\nmake!(Made);\n",
            ),
            ("src/glob.rs", "pub use crate::model::*;\n"),
            ("src/model.rs", "pub struct Anchor;\n"),
            ("src/twice.rs", "pub fn x() {}\n"),
            (
                "src/cycle.rs",
                "pub use self::b as c;\npub use self::c as b;\n",
            ),
            (
                "src/user.rs",
                "use super::*;\nfn f() {\n    crate::made::Made;\n    crate::glob::Anchor;\n    \
                 crate::twice::x();\n    crate::cycle::c;\n    crate::model::Anchor;\n}\n",
            ),
            ("src/stray.rs", "use crate::model::Anchor;\n"),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/cycle.rs -> ? 0 re-exports 2",
                "src/glob.rs -> src/model.rs 0 re-exports 1",
                "src/lib.rs -> ? 1",
                "src/stray.rs -> ? 1",
                "src/user.rs -> ? 5",
                "src/user.rs -> src/model.rs 1",
            ]
        );
    }

    #[test]
    fn an_import_in_test_code_is_flagged_and_an_edge_is_test_code_when_every_import_behind_it_is() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
            (
                "src/a.rs",
                "use crate::b::B;\n#[cfg(test)]\nmod tests {\n    use crate::c::C;\n    use crate::b::B;\n}\n",
            ),
            (
                "src/b.rs",
                "pub struct B;\n#[test]\nfn t() { crate::c::C; }\n#[cfg(test)]\nmod tests;\n",
            ),
            ("src/b/tests.rs", "use super::B;\n"),
            ("src/c.rs", "pub struct C;\n"),
        ];

        assert_eq!(
            imports_of(&files),
            [
                "src/a.rs -> src/b.rs 2",
                "src/a.rs -> src/c.rs 1 test",
                "src/b.rs -> src/c.rs 1 test",
                "src/b/tests.rs -> src/b.rs 1 test",
            ]
        );
    }

    #[test]
    fn a_chain_of_reexports_through_every_file_is_followed_once_for_all_its_importers() {
        // Each file re-exports the next one's item and imports it from the
        // first: a walk of the chain per import would be quadratic.
        let count = 10_000;
        let declared: String = (0..=count).map(|n| format!("mod m{n};\n")).collect();
        let texts: Vec<String> = (0..count)
            .map(|n| {
                format!(
                    "pub use crate::m{}::X;\nfn f() {{ crate::m0::X; }}\n",
                    n + 1
                )
            })
            .collect();
        let names: Vec<String> = (0..=count).map(|n| format!("src/m{n}.rs")).collect();
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", declared.as_str()),
        ];
        files.extend(
            names
                .iter()
                .map(String::as_str)
                .zip(texts.iter().map(String::as_str)),
        );
        files.push((names[count].as_str(), "pub struct X;\n"));

        let imports = imports_of(&files);

        let last = format!(" -> {} 1 re-exports 1", names[count]);
        assert_eq!(imports.len(), count);
        assert!(
            imports.iter().all(|edge| edge.ends_with(&last)),
            "{imports:?}"
        );
    }
}
