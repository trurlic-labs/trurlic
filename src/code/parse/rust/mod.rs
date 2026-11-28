//! Rust definitions from tree-sitter-rust's tree: items, the fields of a
//! struct or union, the variants of an enum with their fields, the leaves
//! of a `pub use`, and what a trait or an `impl` holds, each named by its
//! path from the outermost container.
//!
//! A container gives what it holds a segment: a `mod`, a type, a trait,
//! an enum variant, a function (an item in its body is `outer::inner`),
//! a `const` or `static` (an item in its initializer). An inherent `impl`
//! gives its items the type's segment, its generics, path and references
//! dropped (`impl<T> a::Table<T>` and `impl &Table` are `Table`), and is no
//! definition itself; a trait impl adds the trait's segment
//! (`Table::<Display>::fmt`) and is the definition of that path. A tuple
//! field has no name and defines nothing; neither does a `use` without a
//! visibility, a glob, an item named `_`, or a `pub use` of a path the file
//! already defines (`pub(crate) use name;` after `macro_rules! name`). An
//! item's span takes in the attributes and doc comments before it. The
//! file and every definition but a re-export hold the first paragraph of
//! their doc (`docs.rs`). The file holds the paths it names into its crate
//! (`imports.rs`), read by the one reader of Rust paths (`reading.rs`,
//! `paths.rs`), which the crate's boundary tests read through too.
//!
//! A `macro_rules!` macro is a definition of kind macro. A macro
//! invocation defines nothing: what it produces is its macro's, which the
//! index does not run, whether a `macro_rules!` body or the invocation's
//! own tokens spell it.
//!
//! A segment is spelled as a symbol's is
//! ([`Segment::of_rust`](crate::code::index::Segment::of_rust)): a raw
//! identifier as its name, and a trait's arguments or a type with no name
//! from the tokens its hash holds, so a comment or a trailing comma
//! rustfmt adds moves no path.

mod docs;
mod imports;
mod items;
mod lexicon;
mod marks;
mod paths;
mod reading;
mod rebinding;
mod reexports;
mod silent;
mod walk;

#[cfg(test)]
pub(crate) use lexicon::LEXICON;
#[cfg(test)]
pub(crate) use paths::{End, PathKind};
pub(crate) use paths::{Led, lead};
#[cfg(test)]
pub(crate) use reading::{Reading, is_attribute, is_inline_module, joined};
pub(in crate::code) use walk::index;

#[cfg(test)]
mod tests;
