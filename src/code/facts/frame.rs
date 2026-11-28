//! A walked file as its frame shows it: what the shell found of it, and
//! each definition its index holds with its path, kind, visibility, role,
//! lines and doc. A type of a Rust file carries the impls that other
//! walked Rust files hold of its name.
//!
//! The index holds no type an `impl` resolves to, only the segment its
//! path is given: its self type's name, its own path dropped
//! (`impl a::Table` is `Table`). So an impl is matched to a type by that
//! name alone, an approximation. A name that two holders of the tree share
//! (structs, enums, unions, traits, type aliases, at two paths or in two
//! files) has its impls attributed to none: `impl Tr for dyn Same` beside
//! a trait `Same`. A trait impl whose self type is written as a path of
//! more segments is listed under no type, as it may name another crate's
//! (`impl From<Error> for std::io::Error`). A generic parameter named as
//! a type is taken for it (`impl<T> Tr for T` beside a struct `T`), and
//! so is another type a `use` renames to its name; an impl through a type
//! alias of another name is not found; an inherent impl with no member is
//! not listed, as the index holds its members alone.

use std::collections::{BTreeMap, BTreeSet};

use super::entry::Entry;
use super::tree::{CodeTree, Node};
use crate::code::index::{
    Definition, DefinitionKind, Doc, FileIndex, FileKind, Role, Scope, ScopeId, Scopes, Segment,
    SelfType, Visibility,
};

/// A definition of a frame's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileDefinition<'i> {
    /// Its path's segments as the index spells them, outermost first.
    pub(crate) path: Vec<String>,
    pub(crate) kind: DefinitionKind,
    /// `None` where the source states none.
    pub(crate) visibility: Option<Visibility>,
    /// Test code by its own mark or by its file's.
    pub(crate) role: Role,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    pub(crate) doc: Option<&'i Doc>,
    /// A struct's, an enum's or a union's of a Rust file alone.
    pub(crate) impls: Option<Impls<'i>>,
}

/// The impls that the other walked Rust files hold of a type's name, as
/// far as the name tells (the module's doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Impls<'i> {
    /// One type of the tree has the name: its impls in the other files,
    /// by file, then by line.
    ByName(Vec<ImplBlock<'i>>),
    /// This many holders of the tree, two or more, have the name: structs,
    /// enums, unions, traits and type aliases. No impl is attributed.
    Ambiguous(usize),
}

/// An impl of a type's name in another file: a trait impl, the
/// definition `Type::<Trait>`, or an inherent `impl Type`, which the index
/// holds as its members alone, so its lines run from its first member's
/// first line to its last member's last line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImplBlock<'i> {
    /// The node of its file.
    pub(crate) file: Node<'i>,
    /// Its path's segments, outermost first: those of what holds it, the
    /// type's name, then a trait impl's `<Trait>`.
    pub(crate) path: Vec<String>,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
}

/// What the tree holds of the type names a frame's file defines.
#[derive(Default)]
struct Named<'i> {
    /// The places that define a holder of each name ([`holder_name`]): a
    /// file and a path, so the cfg twins of one holder are one place.
    holders: BTreeMap<&'i str, BTreeSet<(&'i str, Vec<&'i Segment>)>>,
    blocks: BTreeMap<&'i str, Vec<ImplBlock<'i>>>,
}

impl<'i> CodeTree<'i> {
    /// What the shell found of the walked file at `path`; `None` for a
    /// path no walked file is at.
    pub(crate) fn entry(&self, path: &str) -> Option<&'i Entry> {
        self.places.get(path)?;
        self.facts.entries.get(path)
    }

    /// Each definition of the walked file at `path`, in its index's order
    /// (by span, then scope); none when it has no index.
    pub(crate) fn file_definitions(&self, path: &str) -> Vec<FileDefinition<'i>> {
        let Some((kind, index)) = self.walked_index(path) else {
            return Vec::new();
        };
        let scopes = Scopes(&index.scopes);
        let file_role = self.facts.file_role(path);
        let impls = match kind == FileKind::Rust {
            true => self.impls_of(path, index),
            false => BTreeMap::new(),
        };
        let definitions = index.definitions.iter().map(|definition| FileDefinition {
            path: spelled(scopes, definition.scope),
            kind: definition.kind,
            visibility: definition.visibility,
            role: definition.role.or(file_role),
            first_line: definition.span.first_line,
            last_line: definition.span.last_line,
            doc: definition.doc.as_ref(),
            // clone: the cfg twins of a type, one name, each list its impls.
            impls: type_name(scopes, definition).and_then(|name| impls.get(name).cloned()),
        });
        definitions.collect()
    }

    /// The kind and index of the walked file at `path`, when it has one.
    pub(super) fn walked_index(&self, path: &str) -> Option<(FileKind, &'i FileIndex)> {
        match self.entry(path)? {
            Entry::File {
                kind,
                index: Ok(index),
                ..
            } => Some((*kind, index)),
            Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed => None,
        }
    }

    /// The impls of each type name that `index`, the Rust file at `file`,
    /// defines, from one pass over the definitions of the walked Rust
    /// files; none is read when it defines no type.
    fn impls_of(&self, file: &str, index: &'i FileIndex) -> BTreeMap<&'i str, Impls<'i>> {
        let scopes = Scopes(&index.scopes);
        let names: BTreeSet<&'i str> = index
            .definitions
            .iter()
            .filter_map(|definition| type_name(scopes, definition))
            .collect();
        if names.is_empty() {
            return BTreeMap::new();
        }
        let mut named = Named::default();
        for (&other, &place) in &self.places {
            let Some((FileKind::Rust, index)) = self.walked_index(other) else {
                continue;
            };
            named.read_types(other, index, &names);
            if other != file {
                let node = Node::File { path: other, place };
                named.read_trait_impls(node, index, &names);
                named.read_inherent(node, index, &names);
            }
        }
        named.impls(names)
    }
}

impl<'i> Named<'i> {
    /// The impls of each of `names`, as what was read tells them.
    fn impls(self, names: BTreeSet<&'i str>) -> BTreeMap<&'i str, Impls<'i>> {
        let Self {
            holders,
            mut blocks,
        } = self;
        let impls = names.into_iter().map(|name| {
            let holders = holders.get(name).map_or(0, BTreeSet::len);
            if holders > 1 {
                return (name, Impls::Ambiguous(holders));
            }
            let mut found = blocks.remove(name).unwrap_or_default();
            found.sort_by(|a, b| {
                let key = |block: &ImplBlock<'i>| (block.file, block.first_line, block.last_line);
                key(a).cmp(&key(b)).then_with(|| a.path.cmp(&b.path))
            });
            (name, Impls::ByName(found))
        });
        impls.collect()
    }

    /// Add the holders of `names` that `index`, the Rust file at `file`,
    /// defines.
    fn read_types(&mut self, file: &'i str, index: &'i FileIndex, names: &BTreeSet<&'i str>) {
        let scopes = Scopes(&index.scopes);
        for definition in &index.definitions {
            let held = holder_name(scopes, definition).filter(|name| names.contains(name));
            if let Some(name) = held {
                let mut path: Vec<&Segment> = scopes.path_from_end(definition.scope).collect();
                path.reverse();
                self.holders.entry(name).or_default().insert((file, path));
            }
        }
    }

    /// Add the trait impls of `names` that `index`, the Rust file of the
    /// node `file`, holds.
    fn read_trait_impls(
        &mut self,
        file: Node<'i>,
        index: &'i FileIndex,
        names: &BTreeSet<&'i str>,
    ) {
        let scopes = Scopes(&index.scopes);
        for definition in &index.definitions {
            if let Some(name) = impl_name(scopes, definition).filter(|name| names.contains(name)) {
                self.blocks.entry(name).or_default().push(ImplBlock {
                    file,
                    path: spelled(scopes, definition.scope),
                    first_line: definition.span.first_line,
                    last_line: definition.span.last_line,
                });
            }
        }
    }

    /// Add the inherent impls of `names` that `index`, the Rust file of the
    /// node `file`, holds, each with the lines of its members.
    fn read_inherent(&mut self, file: Node<'i>, index: &'i FileIndex, names: &BTreeSet<&'i str>) {
        let mut defined = vec![false; index.scopes.len()];
        for definition in &index.definitions {
            if let Some(slot) = defined.get_mut(definition.scope.at()) {
                *slot = true;
            }
        }
        let mut blocks: BTreeMap<ScopeId, (&'i str, usize, usize)> = BTreeMap::new();
        for definition in &index.definitions {
            let held = inherent_block(&index.scopes, &defined, definition);
            let Some((block, name)) = held.filter(|(_, name)| names.contains(name)) else {
                continue;
            };
            let span = definition.span;
            let lines = blocks
                .entry(block)
                .or_insert((name, span.first_line, span.last_line));
            lines.1 = lines.1.min(span.first_line);
            lines.2 = lines.2.max(span.last_line);
        }
        let scopes = Scopes(&index.scopes);
        for (block, (name, first_line, last_line)) in blocks {
            self.blocks.entry(name).or_default().push(ImplBlock {
                file,
                path: spelled(scopes, block),
                first_line,
                last_line,
            });
        }
    }
}

/// The name of the type `definition` is, a struct, an enum or a union:
/// its path's last segment.
fn type_name<'i>(scopes: Scopes<'i>, definition: &Definition) -> Option<&'i str> {
    match definition.kind {
        DefinitionKind::Struct | DefinitionKind::Enum | DefinitionKind::Union => {
            last_name(scopes, definition)
        }
        // Every other kind is no type a frame lists impls of.
        _ => None,
    }
}

/// The name `definition` holds that an impl's self type may name: a
/// type's, a trait's or a type alias's, its path's last segment.
fn holder_name<'i>(scopes: Scopes<'i>, definition: &Definition) -> Option<&'i str> {
    match definition.kind {
        DefinitionKind::Struct
        | DefinitionKind::Enum
        | DefinitionKind::Union
        | DefinitionKind::Trait
        | DefinitionKind::TypeAlias => last_name(scopes, definition),
        // Every other kind holds no name a self type spells.
        _ => None,
    }
}

/// The last segment of `definition`'s path, when it is a name.
fn last_name<'i>(scopes: Scopes<'i>, definition: &Definition) -> Option<&'i str> {
    match scopes.path_from_end(definition.scope).next()? {
        Segment::Name(name) => Some(name),
        Segment::Impl { .. } => None,
    }
}

/// The name of the type whose trait impl `definition` is, the segment
/// before its `<Trait>`, when its self type is written as that name alone;
/// `None` for one written as a path of more segments, which may name a
/// type of another crate (`impl From<Error> for std::io::Error`).
fn impl_name<'i>(scopes: Scopes<'i>, definition: &Definition) -> Option<&'i str> {
    if definition.self_type != Some(SelfType::Bare) {
        return None;
    }
    match scopes.path_from_end(definition.scope).nth(1)? {
        Segment::Name(name) => Some(name),
        Segment::Impl { .. } => None,
    }
}

/// The inherent impl `definition` is a member of, and its type's name:
/// the scope that holds it when no definition has that scope, which an
/// inherent `impl` alone gives (`parse/rust/`).
fn inherent_block<'i>(
    scopes: &'i [Scope],
    defined: &[bool],
    definition: &Definition,
) -> Option<(ScopeId, &'i str)> {
    let parent = scopes.get(definition.scope.at())?.parent?;
    if defined.get(parent.at()).copied().unwrap_or(true) {
        return None;
    }
    match scopes.get(parent.at())?.name.last()? {
        Segment::Name(name) => Some((parent, name)),
        Segment::Impl { .. } => None,
    }
}

/// The segments of the path of `scope`, each as the index spells it,
/// outermost first.
pub(super) fn spelled(scopes: Scopes<'_>, scope: ScopeId) -> Vec<String> {
    let mut path: Vec<String> = scopes
        .path_from_end(scope)
        .map(ToString::to_string)
        .collect();
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;

    /// The impls of each type of the file at `path` of `files`, one a
    /// line: the type, then each impl's file, path and lines, or the count
    /// of types of its name.
    fn impls(files: &[(&str, &str)], path: &str) -> Vec<String> {
        let facts = facts(files, &[]);
        let tree = CodeTree::of(&facts);
        let types = tree.file_definitions(path).into_iter();
        let types = types.filter_map(|definition| {
            let spelled = match definition.impls? {
                Impls::ByName(blocks) => blocks
                    .iter()
                    .map(|block| {
                        let path = block.path.join("::");
                        format!(
                            "{} {path} {}-{}",
                            tree.address(block.file),
                            block.first_line,
                            block.last_line
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                Impls::Ambiguous(types) => format!("{types} types"),
            };
            Some(format!("{}: {spelled}", definition.path.join("::")))
        });
        types.collect()
    }

    /// A crate of `a.rs`, which defines `a`, and `b.rs`, which defines
    /// `b`, each a file's text.
    fn crate_of<'t>(a: &'t str, b: &'t str) -> [(&'t str, &'t str); 4] {
        [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\nmod b;\n"),
            ("src/a.rs", a),
            ("src/b.rs", b),
        ]
    }

    #[test]
    fn the_cfg_twins_of_a_type_are_one_holder_of_its_name() {
        let a = "#[cfg(unix)]\npub struct T;\n#[cfg(not(unix))]\npub struct T;\n";
        let files = crate_of(a, "impl Clone for T {}\n");

        assert_eq!(
            impls(&files, "src/a.rs"),
            [
                "T: file:src/b.rs T::<Clone> 1-1",
                "T: file:src/b.rs T::<Clone> 1-1"
            ]
        );
    }

    #[test]
    fn a_type_s_impls_in_its_own_file_are_not_listed_as_elsewhere() {
        let a = "pub struct T;\nimpl T {\n    fn here() {}\n}\nimpl Clone for T {}\n";
        let files = crate_of(a, "");

        assert_eq!(impls(&files, "src/a.rs"), ["T: "]);
    }

    #[test]
    fn an_inherent_impl_in_a_function_is_matched_by_its_self_type_s_name() {
        let b = "fn f() {\n    impl crate::a::T {\n        fn inner() {}\n    }\n}\n";
        let files = crate_of("pub struct T;\n", b);

        assert_eq!(impls(&files, "src/a.rs"), ["T: file:src/b.rs f::T 3-3"]);
    }

    #[test]
    fn a_type_name_two_paths_define_lists_no_impl() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\nmod inner { pub struct T; }\n"),
            ("src/a.rs", "pub enum T {}\nimpl Clone for T {}\n"),
        ];

        assert_eq!(impls(&files, "src/a.rs"), ["T: 2 types"]);
    }

    #[test]
    fn a_trait_impl_whose_self_type_is_written_as_a_path_is_listed_under_no_type() {
        let b = "impl From<std::io::Error> for Error {\n    fn from(_: std::io::Error) -> Self { Error }\n}\n\
                 impl From<Error> for std::io::Error {\n    fn from(_: Error) -> Self { todo() }\n}\n\
                 impl Display for Error {}\n";
        let files = crate_of("pub enum Error {}\n", b);

        assert_eq!(
            impls(&files, "src/a.rs"),
            [
                "Error: file:src/b.rs Error::<From<std::io::Error>> 1-3, file:src/b.rs Error::<Display> 7-7"
            ]
        );
    }

    #[test]
    fn a_type_named_as_a_trait_or_a_type_alias_lists_no_impl() {
        let b = "pub trait Same {}\npub type T = u8;\nimpl Tr for dyn Same {}\n";
        let files = crate_of("pub struct Same;\npub struct T;\n", b);

        assert_eq!(impls(&files, "src/a.rs"), ["Same: 2 types", "T: 2 types"]);
    }
}
