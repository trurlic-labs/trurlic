//! A Rust file's imports (`index::Import`): the paths its reading names
//! ([`Reading::paths`]) that go into its crate, each with the role of the
//! bytes it stands in, added to its index after the walk of its items.
//!
//! A path goes into the crate when its first segment is `crate`,
//! `$crate`, `self` or `super`, or the name of a module the file declares
//! in the module the path stands in (`mod child;` then `child::f()`). No
//! other path is one: a dependency's, `std`'s, an `extern crate` alias's
//! (`extern crate self as x`), or the crate's own name from another
//! target. Nor is a path led by a name a `use` brought into scope: the
//! `use` is the import (`use crate::store as s; s::x()` imports `store`
//! once). A trailing `self` of a `use` list is no step: `use a::{self}`
//! imports `a`. A path in an attribute's string is one, a doc comment's
//! none, and `$crate` in a `macro_rules!` body is one of the file that
//! defines the macro. A `use` in a block or among a macro's tokens is an
//! import that binds nothing in the module, a re-export where a
//! visibility opens it among a macro's tokens. A path led by a child
//! module's name where the name may be bound anew is unresolvable
//! (`rebinding.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::paths::{End, Named, PathKind, Paths, Spelled};
use super::reading::Reading;
use super::rebinding::Rebinding;
use crate::code::index::{
    Before, Binds, DefinitionKind, Import, ImportKind, IndexFault, PathStep, Role, ScopeId, Segment,
};
use crate::code::parse::tree_index::TreeIndex;

/// What the walk of a file's items finds of where its bytes stand.
#[derive(Default)]
pub(super) struct Regions<'t> {
    /// The bytes of each item that is test code where what holds it is
    /// not, so none is inside another.
    tests: Vec<Range<usize>>,
    /// The scope of each inline module, by the first byte of its `mod`
    /// item.
    modules: BTreeMap<usize, ScopeId>,
    /// Each name an item that stands in a block, or a generic parameter
    /// of an item, binds, with the bytes it binds it over.
    bound: Vec<(&'t str, Range<usize>)>,
}

impl<'t> Regions<'t> {
    /// Note `name`, which an item in a block or a generic parameter binds
    /// over `bytes`.
    pub(super) fn add_bound(&mut self, name: &'t str, bytes: Range<usize>) {
        self.bound.push((name, bytes));
    }

    /// Note `bytes`, an item that is test code where what holds it is not.
    pub(super) fn add_test(&mut self, bytes: Range<usize>) {
        self.tests.push(bytes);
    }

    /// Note `scope`, of the inline module whose `mod` item starts at byte
    /// `start`.
    pub(super) fn add_module(&mut self, start: usize, scope: ScopeId) {
        self.modules.insert(start, scope);
    }

    /// The role of the byte `at`; `tests` sorted by their first byte.
    fn role_at(&self, at: usize) -> Role {
        let after = self.tests.partition_point(|bytes| bytes.start <= at);
        let holder = after
            .checked_sub(1)
            .and_then(|holder| self.tests.get(holder));
        match holder.is_some_and(|bytes| at < bytes.end) {
            true => Role::Test,
            false => Role::Code,
        }
    }
}

/// Add to `build` the imports `reading`, the reading of its file, names,
/// with `regions`, what the walk found of the file's bytes, and each
/// step of their paths once: a list's prefix is one chain of steps for
/// all its entries.
pub(super) fn add<'t>(
    reading: &Reading<'t>,
    mut regions: Regions<'t>,
    build: &mut TreeIndex<'_>,
) -> Result<(), IndexFault> {
    regions.tests.sort_unstable_by_key(|bytes| bytes.start);
    let paths = reading.paths();
    let bound = std::mem::take(&mut regions.bound);
    let setting = Setting {
        reading,
        paths: &paths,
        regions: &regions,
        declared: declared_modules(build),
        rebinding: Rebinding::of(bound),
    };
    let mut steps: Vec<Option<usize>> = vec![None; reading.tokens.len()];
    for named in &paths.named {
        let Some(kept) = setting.kept(named) else {
            continue;
        };
        let last = add_steps(&kept, (reading, &paths), &mut steps, build)?;
        build.add_import(Import {
            last,
            role: kept.role,
            kind: kept.kind,
        })?;
    }
    Ok(())
}

/// The names of the modules the file declares, `mod` items inline or in
/// a file of their own, by the scope they stand in.
type Declared = BTreeMap<Option<ScopeId>, BTreeSet<String>>;

/// The modules `build` has defined, as [`Declared`] holds them.
fn declared_modules(build: &TreeIndex<'_>) -> Declared {
    let mut declared = Declared::new();
    let modules = build.definitions().iter();
    for module in modules.filter(|definition| definition.kind == DefinitionKind::Module) {
        let Some(scope) = build.scopes.get(module.scope) else {
            continue;
        };
        // A module's scope is its name alone; an impl's segment is none.
        if let [Segment::Name(name)] = scope.name.as_slice() {
            // clone: the names outlive the borrow of the scope tree, which
            // adding the steps takes.
            declared
                .entry(scope.parent)
                .or_default()
                .insert(name.clone());
        }
    }
    declared
}
/// What a file's paths are read in: its reading and its paths, where its
/// test items and inline modules are, the modules it declares, and where
/// a name may be bound anew.
struct Setting<'r, 't> {
    reading: &'r Reading<'t>,
    paths: &'r Paths,
    regions: &'r Regions<'r>,
    declared: Declared,
    rebinding: Rebinding<'r>,
}

/// A path that is an import: its last segment's token, the inline module
/// it stands in, the role of its bytes, and its kind.
struct Kept {
    last: usize,
    module: Option<ScopeId>,
    role: Role,
    kind: ImportKind,
}

impl Setting<'_, '_> {
    /// `named` as an import; `None` when it goes into no module of the
    /// crate, or stands in an inline module with no scope. A path led by
    /// a child module's name where the name may be bound anew is
    /// unresolvable ([`Rebinding`]).
    fn kept(&self, named: &Named) -> Option<Kept> {
        let reading = self.reading;
        let first = reading.tokens.get(named.first)?;
        let module = match first.module {
            None => None,
            Some(inline) => {
                let item = reading.modules.get(inline)?;
                Some(*self.regions.modules.get(&item.start_byte())?)
            }
        };
        let is_self = |at: usize| reading.tokens.get(at).is_some_and(|t| t.text == "self");
        let last = match is_self(named.last) && named.last != named.first {
            true => self.paths.before(named.last)?,
            false => named.last,
        };
        // A word alone in code is no path, `self` of `a::self` left out.
        if named.kind == PathKind::Code && last == named.first {
            return None;
        }
        let kind = match Spelled::of(&first.text) {
            Spelled::Crate | Spelled::Current | Spelled::Parent => self.kind_of(named),
            Spelled::Name(name) => {
                let declared = self.declared.get(&module);
                if !declared.is_some_and(|names| names.contains(name)) {
                    return None;
                }
                match self.rebinding.may_rebind(name, first.start, reading) {
                    true => ImportKind::Unresolvable,
                    false => self.kind_of(named),
                }
            }
        };
        Some(Kept {
            last,
            module,
            role: self.regions.role_at(first.start),
            kind,
        })
    }

    /// The import kind of `named`, a path into the crate, with what it
    /// binds if it is a `use` of the module.
    fn kind_of(&self, named: &Named) -> ImportKind {
        let binds = || match (bound(named, self.reading, self.paths), named.end) {
            (_, End::Glob) => Binds::Glob,
            (Some(_), End::Path) => Binds::Last,
            (Some(name), End::Alias(_)) => Binds::Alias(name.to_owned()),
            (None, End::Path | End::Alias(_)) => Binds::Nothing,
        };
        match named.kind {
            PathKind::Code => ImportKind::Code,
            PathKind::Use => ImportKind::Use(binds()),
            PathKind::Reexport => ImportKind::Reexport(binds()),
            PathKind::InMacro { exported: true } => ImportKind::Reexport(Binds::Nothing),
            PathKind::InBlock | PathKind::InMacro { exported: false } => {
                ImportKind::Use(Binds::Nothing)
            }
        }
    }
}

/// The name the `use` leaf `named` binds: its last segment, the segment
/// before a trailing `self`, or its alias; none for a glob or `as _`.
fn bound<'r>(named: &Named, reading: &'r Reading<'_>, paths: &Paths) -> Option<&'r str> {
    let text = |at: usize| reading.tokens.get(at).map(|token| token.text.as_ref());
    let name = match named.end {
        End::Path if text(named.last) == Some("self") => text(paths.before(named.last)?),
        End::Path => text(named.last),
        End::Alias(alias) => text(alias),
        End::Glob => None,
    };
    name.filter(|name| *name != "_")
}

/// Add each step of `kept`'s path that `steps`, the step of each token
/// added so far, does not hold, root first; the step of its last segment.
fn add_steps(
    kept: &Kept,
    (reading, paths): (&Reading<'_>, &Paths),
    steps: &mut [Option<usize>],
    build: &mut TreeIndex<'_>,
) -> Result<usize, IndexFault> {
    let added = |token: usize, steps: &[Option<usize>]| steps.get(token).copied().flatten();
    if let Some(step) = added(kept.last, steps) {
        return Ok(step);
    }
    let mut above = None;
    let mut unadded = Vec::new();
    let mut at = paths.before(kept.last);
    while let Some(token) = at {
        if let Some(step) = added(token, steps) {
            above = Some(step);
            break;
        }
        unadded.push(token);
        at = paths.before(token);
    }
    let mut add = |token: usize, above: Option<usize>| {
        let segment = reading.tokens.get(token).map_or("", |token| &token.text);
        let before = match above {
            None => Before::Start(kept.module),
            Some(step) => Before::Step(step),
        };
        let step = build.add_step(PathStep {
            before,
            segment: segment.to_owned(),
        })?;
        if let Some(slot) = steps.get_mut(token) {
            *slot = Some(step);
        }
        Ok(step)
    };
    for token in unadded.into_iter().rev() {
        above = Some(add(token, above)?);
    }
    add(kept.last, above)
}

#[cfg(test)]
mod tests {
    use crate::code::index::{Before, Binds, FileKind, ImportKind, Scopes};
    use crate::code::testing;

    /// Each import of `text`, a Rust file: its path, its kind and what it
    /// binds, the inline module it stands in, and its role.
    fn imports(text: &str) -> Vec<String> {
        let index = testing::index(FileKind::Rust, text.as_bytes()).unwrap();
        let steps = &index.rust.steps;
        index
            .rust
            .imports
            .iter()
            .map(|import| {
                let mut segments = Vec::new();
                let mut at = Some(import.last);
                let mut module = None;
                while let Some(step) = at.and_then(|at| steps.get(at)) {
                    segments.push(step.segment.as_str());
                    at = match step.before {
                        Before::Start(scope) => {
                            module = scope;
                            None
                        }
                        Before::Step(before) => Some(before),
                    };
                }
                segments.reverse();
                let binds = |binds: &Binds| match binds {
                    Binds::Last => String::new(),
                    Binds::Alias(name) => format!(" as {name}"),
                    Binds::Nothing => " as nothing".to_owned(),
                    Binds::Glob => " glob".to_owned(),
                };
                let kind = match &import.kind {
                    ImportKind::Code => "code".to_owned(),
                    ImportKind::Use(bound) => format!("use{}", binds(bound)),
                    ImportKind::Reexport(bound) => format!("reexport{}", binds(bound)),
                    ImportKind::Unresolvable => "unresolvable".to_owned(),
                };
                let module = module.map_or(String::new(), |scope| {
                    format!(" in {}", testing::path(Scopes(&index.scopes), scope))
                });
                let path = segments.join("::");
                format!("{path} {kind}{module} {:?}", import.role)
            })
            .collect()
    }

    #[test]
    fn each_path_into_the_crate_is_an_import_with_its_kind_module_and_role() {
        let text = "mod child;\n\
             mod inline { use super::child::A; fn f() { super::super::x(); } }\n\
             use crate::store as s;\n\
             use std::fmt;\n\
             use child::{self, B};\n\
             pub use crate::model::{C as D, *};\n\
             pub(crate) use self::child::E as _;\n\
             fn g() { s::x(); child::f(); crate::a::b(); fmt::write(); x::y(); child; }\n\
             #[serde(with = \"crate::serde::with\")]\n\
             struct S;\n\
             macro_rules! m { () => { $crate::macros::inner() } }\n\
             /// crate::doc::comment\n\
             #[cfg(test)]\n\
             mod tests { use super::*; }\n\
             #[test]\n\
             fn t() { crate::only::test(); }\n\
             pub(crate) use {crate::braced::X as Y};\n\
             fn k(v: u8) { v as crate::cast::T; m!(crate::gen::x()); }\n\
             keep! { pub(crate) use crate::kept::K; }\n";

        assert_eq!(
            imports(text),
            [
                "super::child::A use in inline Code",
                "super::super::x code in inline Code",
                "crate::store use as s Code",
                "child use Code",
                "child::B use Code",
                "crate::model::C reexport as D Code",
                "crate::model reexport glob Code",
                "self::child::E reexport as nothing Code",
                "child::f code Code",
                "crate::a::b code Code",
                "crate::serde::with code Code",
                "$crate::macros::inner code Code",
                "super use glob in tests Test",
                "crate::only::test code Test",
                "crate::braced::X reexport as Y Code",
                "crate::cast::T code Code",
                "crate::gen::x code Code",
                "crate::kept::K reexport as nothing Code",
            ]
        );
    }

    #[test]
    fn a_path_led_by_a_child_modules_name_where_it_may_be_bound_anew_is_unresolvable() {
        let text = "mod x;\nmod y;\nuse y::*;\n\
             fn a<x: G>() { x::g(); }\n\
             struct S<x>(x);\nimpl<x> S<x> { fn b() { x::g(); } }\n\
             fn c() { extern crate core as x; x::g(); }\n\
             fn d() { vec![{ mod x {} x::g() }]; }\n\
             fn e() { m!(); { x::g(); } }\n\
             fn f() { use crate::y::x; x(); x::g(); }\n\
             fn h() { mod x { pub fn g() {} } x::g(); }\n\
             fn k() { mod z {} x::g(); }\n\
             fn l<'x>() { let v = m!(); x::g(); }\n\
             fn n() { { use crate::y; } x::g(); m!(); }\n";

        assert_eq!(
            imports(text),
            [
                "y use glob Code",
                "x::g unresolvable Code",
                "x::g unresolvable Code",
                "x::g unresolvable Code",
                "x::g unresolvable Code",
                "x::g unresolvable Code",
                "crate::y::x use as nothing Code",
                "x::g unresolvable Code",
                "x::g unresolvable Code",
                "x::g code Code",
                "x::g code Code",
                "crate::y use as nothing Code",
                "x::g unresolvable Code",
            ]
        );
    }

    #[test]
    fn a_path_through_a_macros_metavariable_is_an_import_of_it() {
        let text = "macro_rules! m { ($m:ident) => { crate::$m::g(); }; }\n";

        assert_eq!(imports(text), ["crate::$m::g code Code"]);
    }

    #[test]
    fn a_file_test_code_whole_by_its_inner_attribute_imports_as_test_code() {
        let text = "#![cfg(test)]\nuse crate::a::b;\nfn f() { crate::c::d(); }\n";

        assert_eq!(
            imports(text),
            ["crate::a::b use Test", "crate::c::d code Test"]
        );
    }
}
