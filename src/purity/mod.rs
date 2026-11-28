//! The checks that hold a module's non-test code to what it may name,
//! read from its tokens. A pure module (the code index, `lint`) reaches no
//! file system, process, environment, thread, clock, network or console
//! ([`AllowList`], `Crate::faults`). A module names into the crate only
//! the modules and items of a list (`Crate::places_beyond`), which holds
//! a module to its arrow of the module map and the parts of a module to
//! one order, and re-exports nothing of another module
//! (`Crate::re_exports_beyond`). A test reads the crate once
//! ([`Crate::read`]) and judges its modules against what it read.
//!
//! Every Rust file under `src` is read from disk and through the code
//! index's one reader of Rust paths (`crate::code::testing::Reading`),
//! which the index's imports read through too. It reads every token,
//! macro arguments and bodies included, a raw identifier as its name, and
//! a string inside an attribute other than `doc` for each path of two
//! segments or more it spells anywhere (`#[serde(with = "crate::x::f")]`,
//! `bound = "T: Into<crate::x::T>"`), a line it continues joined; a
//! single word there (`try_from = "Raw"`) names an item in scope, which
//! its `use` or its definition accounts for, as does a word alone in code.
//! It reads each path, a `use` list's entries each one, a cast's type and
//! a qualified path's trait among them, and a `use` a visibility opens as
//! a re-export, among a macro's tokens too. Test code is left out here,
//! after the reading, by the rule of `test_code`. A path's first segment
//! says where it goes. The crate is `crate`, `$crate`, or `super`s, read
//! from the module the path is named in. A module is a file's or one
//! inline in a file, the crate root's included.
//!
//! Every check refuses, in any file of the crate's non-test code, each
//! form that hides code or a module from all of them. That is `include!`,
//! a `mod name` in a macro's tokens, the name a metavariable or not, and a
//! `mod name;` inside an inline module, whose file the check places by
//! its declaring file's path alone. So are `#[macro_use]` and `extern
//! crate`, which bring a module's macros or a crate into scope under a
//! name no list holds. So is a `\x` or `\u` escape in an attribute's
//! string, which may spell a path, and a file the grammar does not parse
//! whole, whose bytes error recovery passes over unread. So is a
//! `macro_rules!`, among a macro's tokens or not, but those
//! `DEFINED_MACROS` admits, each with its reason (the console's `out!`
//! and `diag!`). A metavariable makes a path of whatever tokens a caller
//! hands in (`$root::x`, `use crate::{$m}`, an attribute's `$s`), which no
//! reading follows. An admitted one is refused too where a module is
//! declared in its textual scope, whose code may invoke it with no path.
//! An attribute that names a `path` (`#[path = ..]`, `#[cfg_attr(..,
//! path = ..)]`) is refused in test code too, since the file it names is
//! read as the module its own path spells.
//!
//! A pure module names `std`, `core` and `alloc` only through the modules
//! of `STD_MODULES` and the items of its list. It names the crate only
//! into the list's modules, each a path from the crate root (`code`,
//! `code::index`) a path must go past, and the store's items the list
//! holds, and a dependency only when the list holds it or the item named.
//! Refused outright: the names of `REFUSED_NAMES` wherever they stand, a
//! glob or an alias of such a root, an alias of any path of `std`, `core`
//! or `alloc`, and the prelude macros of `MACROS`, which read files or the
//! environment or write a stream. The one include admitted is
//! `include_str!` of a plain string literal, a relative path that stays
//! inside the module's directory: a language's query file, source of the
//! index as its Rust files are.
//!
//! What no check here sees is stated here alone; each test that holds a
//! module to a list refers here for it. No check sees what names no path
//! in the checked module's code. That is a method call or a field access
//! on a value of another module's type, a value or a closure a caller
//! hands in, and a trait method called through a bound. An admitted macro
//! is judged where it is defined, against that module's list, so what its
//! body names, `$crate::` paths included, reaches a module that invokes it
//! unseen. A re-export of a name a private `use` brought into scope
//! (`use crate::x; pub(crate) use x::Item;`) is read as a path from that
//! name, never from the crate, so the re-export check passes it. Nor does
//! a check see what an allowed crate's or item's own code does, a macro's
//! or a derive's included, with a handle the module cannot name or a
//! value it holds. Nor does it see the runtime's own messages to stderr,
//! a panic's or a failed allocation's.

mod layer;
mod pure;
mod read;
mod test_code;
#[cfg(test)]
mod tests;

pub(crate) use pure::AllowList;
pub(crate) use read::Crate;
