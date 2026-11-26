//! The checks that hold a module's non-test code to what it may name,
//! read from its tokens. A pure module (the code index, `lint`) reaches no
//! file system, process, environment, thread, clock, network or console
//! ([`AllowList`], `Crate::faults`). A module names into the crate only
//! the modules and items of a list (`Crate::places_beyond`), which holds
//! a module to the arrows of the module map and the parts of a module to
//! one order. A test reads the crate once ([`Crate::read`]) and judges
//! its modules against what it read.
//!
//! Every token of every Rust file under `src` is read, macro arguments
//! and bodies included, a raw identifier as its name. A string inside an
//! attribute other than `doc` is read for each path of two segments or
//! more it spells anywhere (`#[serde(with = "crate::x::f")]`,
//! `bound = "T: Into<crate::x::T>"`), a line it continues joined; a
//! single word there (`try_from = "Raw"`) names an item in scope, which
//! its `use` or its definition accounts for. Test code is left out by the
//! rule of `test_code`. A path's first segment says where it goes, a
//! `use` list read whole. The crate is `crate`, `$crate`, or `super`s,
//! read from the module the path is named in, an inline module's
//! included.
//!
//! Every check refuses, in any file of the crate's non-test code, each
//! form that hides code or a module from all of them: `include!`, a
//! `mod name` in a macro's tokens, the name a metavariable or not, and a
//! `mod name;` inside an inline module, whose file the check places by
//! its declaring file's path alone. So are `#[macro_use]` and `extern
//! crate`, which bring a module's macros or a crate into scope under a
//! name no list holds, a module declared in the textual scope of a
//! `macro_rules!`, whose body it may invoke with no path, a `\x` or `\u`
//! escape in an attribute's string, which may spell a path, and a file the
//! grammar does not parse whole, whose bytes error recovery passes over
//! unread. An attribute that names a `path` (`#[path = ..]`,
//! `#[cfg_attr(.., path = ..)]`) is refused in test code too, since the
//! file it names is read as the module its own path spells.
//!
//! A pure module names `std`, `core` and `alloc` only through the modules
//! of `STD_MODULES` and the items of its list. It names the crate only
//! into the list's modules, each a path from the crate root (`code`,
//! `code::index`) a path must go past, and the store's items the list
//! holds, and a dependency only when the list holds it or the item named.
//! Refused outright: the names of `REFUSED_NAMES` wherever they stand, a
//! glob or an alias of such a root, an alias of any path of `std`, `core`
//! or `alloc`, a `macro_rules!` definition, and the prelude macros of
//! `MACROS`, which read files or the environment or write a stream. The
//! one include admitted is `include_str!` of a plain string literal, a
//! relative path that stays inside the module's directory: a language's
//! query file, source of the index as its Rust files are.
//!
//! No check here sees what names no path in the checked module's code.
//! That is a method call or a field access on a value of another
//! module's type, a value or a closure a caller hands in, and a trait
//! method called through a bound. It is an item or a macro body of a
//! module outside the list that a listed module re-exports or holds,
//! reached through the listed module by name or by a glob: a check sees
//! it only where the listed module is held to a list itself. It is what
//! an allowed crate's or item's own code does, a derive's included, with
//! a handle the module cannot name or a value it holds, and the runtime's
//! own messages to stderr, a panic's or a failed allocation's. Each test
//! that holds a module to a list refers here for what it cannot see.

mod layer;
mod path;
mod pure;
mod read;
mod test_code;
#[cfg(test)]
mod tests;

pub(crate) use pure::AllowList;
pub(crate) use read::Crate;
