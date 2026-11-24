//! The second step of the index: the types the macro invocations of each
//! Rust file define, from the type-defining macros its crate and its
//! workspace hold. A file's own index is a function of its bytes alone;
//! this step is a function of the indexes of the whole tree.
//!
//! A file's crate is the nearest directory above it with a `Cargo.toml`,
//! or the file alone when there is none; its workspace is the nearest
//! directory from the crate's up whose `Cargo.toml` has a `workspace`
//! table. An invocation sees each `macro_rules!` macro of its crate, and
//! each `#[macro_export]` one of its workspace that the crate does not
//! shadow with one of the same name. It defines the name after the words
//! of the first arm it opens with, under the container it is in, and
//! that type holds the items the macro's body names.

use std::collections::BTreeMap;

use super::index::{Definition, FileIndex, FileKind, Invocation, TypeMacro};
use super::kinds::DefinitionKind;
use super::rust::is_word;
use super::scope::{Scope, ScopeId};
use super::symbol::Segment;

/// Macros of other crates that define the type their argument at this
/// position names, and its kind. The facts of their crates are not in the
/// tree, so they are listed by hand: pyo3's
/// `create_exception!(module, Name, Base)` and `import_exception!(module,
/// Name)`, each a struct.
const FOREIGN_TYPE_MACROS: [(&str, usize, DefinitionKind); 2] = [
    ("create_exception", 1, DefinitionKind::Struct),
    ("import_exception", 1, DefinitionKind::Struct),
];

/// The manifest that makes a directory a crate.
pub(crate) const MANIFEST: &str = "Cargo.toml";

/// What the invocations of one file add: scopes past the file's own, and
/// the definitions in them.
#[derive(Debug, Default)]
pub(super) struct Expansion {
    pub(super) scopes: Vec<Scope>,
    pub(super) definitions: Vec<Definition>,
}

/// The definitions the invocations of each Rust file of `files` add, by
/// path; `files` maps a path to its kind and index, and `crates` is of
/// the same files.
pub(super) fn expand<'i>(
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
    crates: &Crates<'i>,
) -> BTreeMap<&'i str, Expansion> {
    files
        .iter()
        .filter(|(_, (kind, _))| *kind == FileKind::Rust)
        .filter_map(|(&path, (_, index))| Some((path, expand_file(path, index, crates)?)))
        .collect()
}

/// What the invocations of `index`, the Rust file at `path`, add with the
/// macros `crates` shows it; `None` when they add nothing.
pub(super) fn expand_file(path: &str, index: &FileIndex, crates: &Crates<'_>) -> Option<Expansion> {
    if index.invocations.is_empty() {
        return None;
    }
    {
        let visible = crates.visible_from(path);
        // An invocation that spells the item it defines (`m! { struct X }`)
        // defines it once: the file's own index holds it.
        let own: BTreeMap<(Option<ScopeId>, &[Segment]), ScopeId> = index
            .definitions
            .iter()
            .filter_map(|d| {
                let scope = index.scopes.get(d.scope.at())?;
                Some(((scope.parent, scope.name.as_slice()), d.scope))
            })
            .collect();
        let mut expansion = Expansion::default();
        for invocation in &index.invocations {
            let Some(defined) = defined_type(invocation, &visible) else {
                continue;
            };
            let mut adding = Adding {
                base: index.scopes.len(),
                own: &own,
                invocation,
                expansion: &mut expansion,
            };
            adding.define(defined);
        }
        (!expansion.definitions.is_empty()).then_some(expansion)
    }
}

/// The type-defining macros of the tree, by the crate that holds them,
/// and which directories are workspaces.
pub(super) struct Crates<'i> {
    /// Each crate's directory (`""` for the code root) to its macros, and
    /// each lone file outside a crate to its own.
    macros: BTreeMap<&'i str, Vec<&'i TypeMacro>>,
    crate_roots: Vec<&'i str>,
    workspaces: Vec<&'i str>,
}

impl<'i> Crates<'i> {
    pub(super) fn of(files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>) -> Self {
        let manifests: Vec<_> = files
            .iter()
            .filter(|(path, _)| file_name(path) == MANIFEST)
            .collect();
        let mut crate_roots: Vec<&str> = manifests.iter().map(|(path, _)| parent(path)).collect();
        let mut workspaces: Vec<&str> = manifests
            .iter()
            .filter(|(_, (_, index))| {
                let workspace = [Segment::name("workspace")];
                index.definitions.iter().any(|d| {
                    index
                        .scopes
                        .get(d.scope.at())
                        .is_some_and(|scope| scope.parent.is_none() && scope.name == workspace)
                })
            })
            .map(|(path, _)| parent(path))
            .collect();
        // A path sorts before its siblings that extend its name (`a` before
        // `a-b`), but `a/b` after `a-b`: the parents need their own sort.
        crate_roots.sort_unstable();
        workspaces.sort_unstable();
        let mut crates = Self {
            macros: BTreeMap::new(),
            crate_roots,
            workspaces,
        };
        for (&path, (_, index)) in files {
            if !index.macros.is_empty() {
                let owner = crates.crate_of(path);
                crates
                    .macros
                    .entry(owner)
                    .or_default()
                    .extend(&index.macros);
            }
        }
        crates
    }

    /// The crate directory of `path`, or `path` itself outside every crate.
    pub(super) fn crate_of<'p>(&self, path: &'p str) -> &'p str {
        ancestors(path)
            .find(|dir| self.crate_roots.binary_search(dir).is_ok())
            .unwrap_or(path)
    }

    /// The macros an invocation in `path` sees, by name: its crate's, and
    /// its workspace's exported ones its crate does not shadow.
    fn visible_from(&self, path: &str) -> BTreeMap<&'i str, Vec<&'i TypeMacro>> {
        let own = self.crate_of(path);
        let mut visible: BTreeMap<&str, Vec<&TypeMacro>> = BTreeMap::new();
        // A file outside every crate is in no workspace.
        let workspace = std::iter::once(own)
            .chain(ancestors(own))
            .filter(|_| own != path)
            .find(|dir| self.workspaces.binary_search(dir).is_ok());
        if let Some(workspace) = workspace {
            let members = self
                .macros
                .iter()
                .filter(|(dir, _)| is_within(dir, workspace));
            for found in members.flat_map(|(_, found)| found).filter(|m| m.exported) {
                visible.entry(&found.name).or_default().push(found);
            }
        }
        for found in self.macros.get(own).into_iter().flatten() {
            visible.remove(found.name.as_str());
        }
        for found in self.macros.get(own).into_iter().flatten() {
            visible.entry(&found.name).or_default().push(found);
        }
        visible
    }
}

/// What `invocation` defines: the type's name and kind, and its members.
type Defined<'a> = (
    &'a String,
    DefinitionKind,
    &'a [(String, DefinitionKind)],
    &'a [(String, DefinitionKind)],
);

/// The type `invocation` defines with a macro of `visible` or a foreign
/// one, and its members.
fn defined_type<'a>(
    invocation: &'a Invocation,
    visible: &BTreeMap<&str, Vec<&'a TypeMacro>>,
) -> Option<Defined<'a>> {
    let tokens = &invocation.tokens;
    let named = visible
        .get(invocation.macro_name.as_str())
        .into_iter()
        .flatten()
        .find_map(|found| {
            found.arms.iter().find_map(|arm| {
                let words = arm.words.as_slice();
                let name = tokens.get(words.len()).filter(|name| is_word(name))?;
                (tokens.get(..words.len()) == Some(words)).then_some((
                    name,
                    arm.kind,
                    found.members.as_slice(),
                    found.items.as_slice(),
                ))
            })
        });
    let foreign = || {
        let (_, position, kind) = FOREIGN_TYPE_MACROS
            .iter()
            .find(|(name, ..)| *name == invocation.macro_name)?;
        argument(tokens, *position).map(|name| (name, *kind, &[][..], &[][..]))
    };
    named.or_else(foreign)
}

/// The type one invocation defines being added to its file's expansion.
struct Adding<'a, 'i> {
    /// The number of the file's own scopes: an added scope's id is past
    /// them.
    base: usize,
    /// The file's own definitions, by their parent and name.
    own: &'a BTreeMap<(Option<ScopeId>, &'i [Segment]), ScopeId>,
    invocation: &'a Invocation,
    expansion: &'a mut Expansion,
}

impl Adding<'_, '_> {
    /// Define the type and each member it holds that the file does not
    /// define itself, over the invocation's span. The invocation states no
    /// visibility: the macro's body does.
    fn define(&mut self, (name, kind, members, items): Defined<'_>) {
        let container = self.invocation.container;
        for (item, kind) in items {
            self.scope(container, Segment::name(item), *kind);
        }
        let Some(type_scope) = self.scope(container, Segment::name(name), kind) else {
            return;
        };
        for (member, kind) in members {
            self.scope(Some(type_scope), Segment::name(member), *kind);
        }
    }

    /// The scope of `name` under `parent`: the file's own definition of
    /// it, or one added and defined as a `kind`.
    fn scope(
        &mut self,
        parent: Option<ScopeId>,
        name: Segment,
        kind: DefinitionKind,
    ) -> Option<ScopeId> {
        let key = (parent, std::slice::from_ref(&name));
        if let Some(own) = self.own.get(&key) {
            return Some(*own);
        }
        let id = ScopeId::of(self.base + self.expansion.scopes.len())?;
        self.expansion.scopes.push(Scope {
            parent,
            name: vec![name],
        });
        self.expansion.definitions.push(Definition {
            scope: id,
            kind,
            span: self.invocation.span,
            hash: self.invocation.hash,
            role: self.invocation.role,
            visibility: None,
        });
        Some(id)
    }
}

/// The argument at `position` of comma-separated `tokens`, when it is one
/// word.
fn argument(tokens: &[String], position: usize) -> Option<&String> {
    let mut depth = 0_usize;
    let mut arguments = vec![Vec::new()];
    for token in tokens {
        match token.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                arguments.push(Vec::new());
                continue;
            }
            _ => {}
        }
        if let Some(last) = arguments.last_mut() {
            last.push(token);
        }
    }
    match arguments.get(position)?.as_slice() {
        [word] if is_word(word) => Some(word),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::index;

    /// The paths each file's invocations define in the tree of `files`.
    fn expanded(files: &[(&str, &str)]) -> Vec<String> {
        let indexes: Vec<(&str, FileKind, FileIndex)> = files
            .iter()
            .map(|(path, text)| {
                let kind = FileKind::of(path).unwrap();
                (*path, kind, index(kind, text.as_bytes()).unwrap())
            })
            .collect();
        let tree = indexes
            .iter()
            .map(|(path, kind, index)| (*path, (*kind, index)))
            .collect();
        let crates = Crates::of(&tree);
        let mut found = Vec::new();
        for (path, expansion) in expand(&tree, &crates) {
            let (_, index) = tree[path];
            let scopes = crate::code::scope::Scopes {
                file: &index.scopes,
                added: &expansion.scopes,
            };
            for definition in &expansion.definitions {
                let named = crate::code::testing::path(scopes, definition.scope);
                found.push(format!("{path}: {named}"));
            }
        }
        found
    }

    const EXPORTED: &str =
        "#[macro_export]\nmacro_rules! id { (x $n:ident) => { pub struct $n; }; }\n";
    const LOCAL: &str = "macro_rules! id { (y $n:ident) => { pub struct $n; }; }\n";

    #[test]
    fn a_crate_sees_its_workspaces_exported_macros_unless_it_shadows_their_name() {
        let files = [
            ("Cargo.toml", "[workspace]\n"),
            ("a/Cargo.toml", "[package]\n"),
            ("a/src/lib.rs", EXPORTED),
            ("b/Cargo.toml", "[package]\n"),
            ("b/src/lib.rs", "id! { x Seen }\n"),
            ("c/Cargo.toml", "[package]\n"),
            ("c/src/ids.rs", LOCAL),
            ("c/src/lib.rs", "id! { x Shadowed }\nid! { y Own }\n"),
        ];

        assert_eq!(
            expanded(&files),
            ["b/src/lib.rs: Seen", "c/src/lib.rs: Own"]
        );
    }

    #[test]
    fn no_crate_sees_another_workspaces_exports_nor_another_crates_own_macros() {
        let files = [
            ("one/Cargo.toml", "[workspace]\n[package]\n"),
            ("one/src/lib.rs", EXPORTED),
            ("two/Cargo.toml", "[workspace]\n[package]\n"),
            ("two/src/lib.rs", "id! { x Unseen }\n"),
            ("three/Cargo.toml", "[package]\n"),
            ("three/src/lib.rs", LOCAL),
            ("four/Cargo.toml", "[package]\n"),
            ("four/src/lib.rs", "id! { y Unseen }\n"),
        ];

        assert!(expanded(&files).is_empty());
    }

    #[test]
    fn a_file_outside_every_crate_sees_its_own_macros_alone() {
        let files = [
            ("loose/a.rs", &*format!("{LOCAL}id! {{ y Mine }}\n")),
            ("loose/b.rs", "id! { y Theirs }\n"),
        ];

        assert_eq!(expanded(&files), ["loose/a.rs: Mine"]);
    }

    #[test]
    fn a_foreign_macro_defines_the_name_at_its_position() {
        let files = [(
            "src/lib.rs",
            "create_exception!(m, Full, Base, \"doc\");\nimport_exception!(m, Imported);\ncreate_exception!(m, path::Name, Base);\n",
        )];

        assert_eq!(
            expanded(&files),
            ["src/lib.rs: Full", "src/lib.rs: Imported"]
        );
    }
}
