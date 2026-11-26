//! The check of a pure module: an allow list ([`AllowList`]), closed by
//! construction, of what its non-test code may name, and the names and
//! macros it never spells.

use super::path::{Named, from_crate, paths};
use super::read::{Crate, Read, is_within, text_at};

/// The modules of `std`, `core` and `alloc` a pure module may name: none
/// reads or writes outside the process's memory, but for the hashed
/// collections of [`REFUSED_NAMES`].
const STD_MODULES: [&str; 17] = [
    "borrow",
    "char",
    "cmp",
    "collections",
    "convert",
    "fmt",
    "iter",
    "marker",
    "mem",
    "num",
    "ops",
    "option",
    "result",
    "slice",
    "str",
    "string",
    "vec",
];

/// The names a pure module never spells, wherever they stand, so no
/// alias or import reaches them: the hashed collections, whose default
/// hasher's keys are read from the operating system's entropy.
pub(super) const REFUSED_NAMES: [&str; 5] =
    ["HashMap", "HashSet", "RandomState", "hash_map", "hash_set"];

/// The prelude's macros that read a file or the environment, or write a
/// standard stream. `include!` every check refuses.
const MACROS: [&str; 9] = [
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "print",
    "println",
    "eprint",
    "eprintln",
    "dbg",
];

/// What a pure module's non-test code may name beyond the modules of
/// [`STD_MODULES`].
pub(crate) struct AllowList {
    /// The crate's modules, each a path from its root (`code`,
    /// `code::index`): the checked one and the pure ones it reads.
    pub(crate) modules: &'static [&'static str],
    /// The store's items: its value types.
    pub(crate) store_items: &'static [&'static str],
    /// Items of `std`, `core` and `alloc` outside those modules, as
    /// (module, item).
    pub(crate) std_items: &'static [(&'static str, &'static str)],
    /// The dependencies it may name whole.
    pub(crate) crates: &'static [&'static str],
    /// The items of a dependency it may name, as (crate, item).
    pub(crate) crate_items: &'static [(&'static str, &'static str)],
}

impl Crate {
    /// Each fault of the non-test code of the module `module`, a path from
    /// the crate root (`code`, `code::index`), against `allowed`, and
    /// each of the crate's [`refusals`](Crate::refusals), as
    /// `file:line: what`, files sorted.
    pub(crate) fn faults(&self, module: &str, allowed: &AllowList) -> Vec<String> {
        let mut found = self.refusals();
        let dir = format!("src/{}", module.replace("::", "/"));
        for (path, file) in self.files_of(module) {
            let place = Place { path, dir: &dir };
            found.extend(
                file_faults(file, &self.dependencies, place, allowed)
                    .into_iter()
                    .map(|(line, what)| format!("{path}:{line}: {what}")),
            );
        }
        found
    }
}

/// Where a checked file is: its path from the crate's directory, and the
/// checked module's directory, which holds it.
#[derive(Clone, Copy)]
struct Place<'a> {
    path: &'a str,
    dir: &'a str,
}

/// The faults of `file`, at `place`, in a crate whose dependencies are
/// `dependencies`, against `allowed`.
fn file_faults(
    file: &Read,
    dependencies: &[String],
    place: Place<'_>,
    allowed: &AllowList,
) -> Vec<(usize, String)> {
    let tokens = &file.tokens;
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let next = text_at(tokens, at + 1);
        match (token.text.as_str(), next) {
            (name, _) if REFUSED_NAMES.contains(&name) => {
                found.push((token.line, format!("names `{name}`")));
            }
            ("macro_rules!", _) | ("macro_rules", "!") => {
                found.push((token.line, "macro_rules!".to_owned()));
            }
            ("include_str", "!") if is_admitted_include(file, at, place) => {}
            (name, "!") if MACROS.contains(&name) => {
                found.push((token.line, format!("{name}!")));
            }
            _ => {}
        }
    }
    for named in paths(file) {
        if let Some(fault) = path_fault(&named, dependencies, allowed) {
            found.push((named.line, fault));
        }
    }
    found
}

/// Whether the `include_str!` at `at` of `file` reads a file `place`'s
/// directory holds: its argument one plain string literal, a relative
/// path with no escape, which stays inside the directory once each `..`
/// is taken.
fn is_admitted_include(file: &Read, at: usize, place: Place<'_>) -> bool {
    let spelled = |n: usize| text_at(&file.tokens, n);
    let Some(relative) = spelled(at + 3)
        .strip_prefix('"')
        .and_then(|inside| inside.strip_suffix('"'))
        .filter(|inside| !inside.is_empty() && !inside.contains(['\\', '"']))
    else {
        return false;
    };
    if spelled(at + 2) != "(" || spelled(at + 4) != ")" || relative.starts_with('/') {
        return false;
    }
    let mut segments: Vec<&str> = place.path.split('/').collect();
    segments.pop();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return false;
                }
            }
            segment => segments.push(segment),
        }
    }
    let dir_depth = place.dir.split('/').count();
    segments.len() > dir_depth && segments.join("/").starts_with(&format!("{}/", place.dir))
}

/// Why `named`, in a crate whose dependencies are `dependencies`, reaches
/// outside what `allowed` holds; `None` when it does not. A path of
/// `super`s is read from the module it is named in.
fn path_fault(named: &Named<'_>, dependencies: &[String], allowed: &AllowList) -> Option<String> {
    let path = &named.segments;
    let refuse = || Some(format!("names `{}`", path.join("::")));
    if let Some(rest) = from_crate(path, &named.module) {
        return match rest.as_slice() {
            rest if allowed.modules.iter().any(|module| goes_into(rest, module)) => None,
            ["store", item, ..] if allowed.store_items.contains(item) => None,
            _ => refuse(),
        };
    }
    let segments: Vec<&str> = path
        .iter()
        .map(String::as_str)
        .filter(|segment| *segment != "self")
        .collect();
    let (root, rest) = segments.split_first()?;
    match *root {
        "std" | "core" | "alloc" => match rest {
            [.., "as"] => refuse(),
            [module, ..] if STD_MODULES.contains(module) => None,
            [module, item, ..] if allowed.std_items.contains(&(*module, *item)) => None,
            _ => refuse(),
        },
        name if allowed.crates.contains(&name) => None,
        name if rest
            .first()
            .is_some_and(|item| allowed.crate_items.contains(&(name, *item))) =>
        {
            None
        }
        name if dependencies.iter().any(|dependency| dependency == name) => refuse(),
        _ => None,
    }
}

/// Whether `path`, segments from the crate root, goes into `module`, a
/// module path such as `code::index`: the module itself is no item of
/// it, so an item of its parent spelled as the module (the function
/// `code::index` beside the module `code::index`) is outside.
fn goes_into(path: &[&str], module: &str) -> bool {
    path.len() > module.split("::").count() && is_within(path, module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_of_supers_is_read_from_the_module_it_is_named_in() {
        let below = AllowList {
            modules: &["code::index"],
            store_items: &[],
            std_items: &[],
            crates: &[],
            crate_items: &[],
        };
        let named = |text: &str, module: &[&'static str]| Named {
            line: 1,
            segments: text.split("::").map(str::to_owned).collect(),
            module: module.to_vec(),
        };
        let file = ["code", "index", "scope"];
        let inline = ["code", "parse", "rust", "inner"];

        assert_eq!(
            path_fault(&named("super::span::Span", &file), &[], &below),
            None
        );
        assert_eq!(
            path_fault(&named("super::super::parse::Token", &file), &[], &below),
            Some("names `super::super::parse::Token`".to_owned())
        );
        assert_eq!(
            path_fault(
                &named("super::super::super::index::Span", &inline),
                &[],
                &below
            ),
            None
        );
    }
}
