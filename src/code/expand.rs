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
//! that type holds the members the macro's `impl`s of it give, a trait
//! impl's under `Name::<Trait>`.
//!
//! The arms a crate sees are a table by macro name and words, built once
//! per crate, so an invocation finds its arm in a few lookups whatever
//! the number of arms. What a file's invocations add is spent from a
//! budget of the bytes they span ([`Work::expansion`]), and every path
//! they make is held to [`MAX_NESTING`]: past either, the file has no
//! index.

use std::collections::{BTreeMap, BTreeSet};

use super::index::{Definition, FileIndex, FileKind, IndexFault, Invocation, TypeMacro};
use super::kinds::DefinitionKind;
use super::rust::{MAX_ARM_WORDS, is_word};
use super::scope::{MAX_NESTING, Scope, ScopeId, Scopes};
use super::symbol::Segment;
use super::work::Work;

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

/// The files of a tree: a path to its kind and index.
pub(super) type Files<'i> = BTreeMap<&'i str, (FileKind, &'i FileIndex)>;

/// The definitions the invocations of each Rust file of `files` add, by
/// path, or why they cannot be added; `crates` is of the same files.
pub(super) fn expand<'i>(
    files: &Files<'i>,
    crates: &Crates<'i>,
) -> BTreeMap<&'i str, Result<Expansion, IndexFault>> {
    let mut tables: BTreeMap<&str, Arms<'i>> = BTreeMap::new();
    let mut expanded = BTreeMap::new();
    for (&path, (kind, index)) in files {
        if *kind != FileKind::Rust || index.invocations.is_empty() {
            continue;
        }
        let owner = crates.crate_of(path);
        let arms = tables
            .entry(owner)
            .or_insert_with(|| crates.arms_of(owner, None));
        if let Some(expansion) = expand_file(index, arms).transpose() {
            expanded.insert(path, expansion);
        }
    }
    expanded
}

/// What the invocations of `index` add with the arms of `arms`; `None`
/// when they add nothing.
pub(super) fn expand_file(
    index: &FileIndex,
    arms: &Arms<'_>,
) -> Result<Option<Expansion>, IndexFault> {
    if index.invocations.is_empty() {
        return Ok(None);
    }
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
    let spanned = index
        .invocations
        .iter()
        .map(|invocation| invocation.span.end - invocation.span.start)
        .sum();
    let mut adding = Adding {
        scopes: &index.scopes,
        own: &own,
        expansion: Expansion::default(),
        depths: Vec::new(),
        work: Work::expansion(spanned),
    };
    for invocation in &index.invocations {
        if let Some(defined) = arms.defined_by(invocation) {
            adding.define(invocation, &defined)?;
        }
    }
    let expansion = adding.expansion;
    Ok((!expansion.definitions.is_empty()).then_some(expansion))
}

/// The crates of the tree, which directories are workspaces, and the
/// type-defining macros of each crate.
pub(super) struct Crates<'i> {
    /// Each crate's directory (`""` for the code root) to its macros, and
    /// each lone file outside a crate to its own.
    macros: BTreeMap<&'i str, Vec<&'i TypeMacro>>,
    crate_roots: Vec<&'i str>,
    workspaces: Vec<&'i str>,
}

impl<'i> Crates<'i> {
    /// The crates of `files`, the macros of its Rust files, and the
    /// crates and workspaces of `manifests` besides, which bring in no
    /// macros: the manifests above an anchored file the walk does not
    /// take, which govern only the files below them.
    pub(super) fn of(files: &Files<'i>, manifests: &Files<'i>) -> Self {
        let all: Vec<(&str, &FileIndex)> = files
            .iter()
            .chain(manifests)
            .filter(|(path, _)| file_name(path) == MANIFEST)
            .map(|(path, (_, index))| (*path, *index))
            .collect();
        let mut crate_roots: Vec<&str> = all.iter().map(|(path, _)| parent(path)).collect();
        let mut workspaces: Vec<&str> = all
            .iter()
            .filter(|(_, index)| has_workspace_table(index))
            .map(|(path, _)| parent(path))
            .collect();
        // A path sorts before its siblings that extend its name (`a` before
        // `a-b`), but `a/b` after `a-b`: the parents need their own sort.
        crate_roots.sort_unstable();
        crate_roots.dedup();
        workspaces.sort_unstable();
        workspaces.dedup();
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

    /// The arms an invocation in the crate `owner` (a lone file's own
    /// path outside every crate) sees: `own`'s macros first, a file's
    /// own that no crate holds, then its crate's, then its workspace's
    /// exported ones that neither shadows by name.
    pub(super) fn arms_of(&self, owner: &str, own: Option<&'i [TypeMacro]>) -> Arms<'i> {
        let mut arms = Arms::default();
        let crate_macros = self.macros.get(owner).map_or(&[][..], Vec::as_slice);
        for found in own.into_iter().flatten() {
            arms.add(found);
        }
        for found in crate_macros {
            arms.add(found);
        }
        // A file outside every crate is in no workspace.
        let workspace = std::iter::once(owner)
            .chain(ancestors(owner))
            .filter(|_| self.crate_roots.binary_search(&owner).is_ok())
            .find(|dir| self.workspaces.binary_search(dir).is_ok());
        let shadowed = arms.names();
        let exported = self
            .macros
            .iter()
            .filter(|(dir, _)| workspace.is_some_and(|root| is_within(dir, root)))
            .flat_map(|(_, found)| found)
            .filter(|found| found.exported && !shadowed.contains(&found.name.as_str()));
        for found in exported {
            arms.add(found);
        }
        arms
    }
}

/// Whether a manifest's index defines a top-level `workspace` table.
fn has_workspace_table(index: &FileIndex) -> bool {
    let workspace = [Segment::name("workspace")];
    index.definitions.iter().any(|d| {
        index
            .scopes
            .get(d.scope.at())
            .is_some_and(|scope| scope.parent.is_none() && scope.name == workspace)
    })
}

/// The type one arm defines: its kind, and the members and container
/// items of its macro.
#[derive(Clone, Copy)]
struct Arm<'i> {
    kind: DefinitionKind,
    found: &'i TypeMacro,
}

/// The arms invocations of one crate see, by macro name and words, each
/// with its place in the order a scan would meet it: the first arm an
/// invocation opens with is the one of least place.
#[derive(Default)]
pub(super) struct Arms<'i> {
    by_words: BTreeMap<(&'i str, &'i [String]), (usize, Arm<'i>)>,
    added: usize,
}

impl<'i> Arms<'i> {
    fn add(&mut self, found: &'i TypeMacro) {
        for arm in &found.arms {
            let place = self.added;
            self.added += 1;
            self.by_words
                .entry((found.name.as_str(), arm.words.as_slice()))
                .or_insert((
                    place,
                    Arm {
                        kind: arm.kind,
                        found,
                    },
                ));
        }
    }

    /// The names of the macros held.
    fn names(&self) -> BTreeSet<&'i str> {
        self.by_words.keys().map(|(name, _)| *name).collect()
    }

    /// The name `invocation` defines with an arm held here, or with a
    /// foreign macro: the name, and the arm.
    fn defined_by<'a>(&self, invocation: &'a Invocation) -> Option<Defined<'a, 'i>> {
        let tokens = &invocation.tokens;
        let opened = (0..=MAX_ARM_WORDS.min(tokens.len()))
            .filter_map(|words| {
                let name = tokens.get(words).filter(|name| is_word(name))?;
                let key = (invocation.macro_name.as_str(), tokens.get(..words)?);
                let (place, arm) = self.by_words.get(&key)?;
                Some((*place, name, *arm))
            })
            .min_by_key(|(place, ..)| *place);
        if let Some((_, name, arm)) = opened {
            return Some(Defined {
                name,
                kind: arm.kind,
                found: Some(arm.found),
            });
        }
        let (_, position, kind) = FOREIGN_TYPE_MACROS
            .iter()
            .find(|(name, ..)| *name == invocation.macro_name)?;
        argument(tokens, *position).map(|name| Defined {
            name,
            kind: *kind,
            found: None,
        })
    }
}

/// What an invocation defines: the type's name and kind, and the macro
/// whose members and items it holds; `None` for a foreign macro.
struct Defined<'a, 'i> {
    name: &'a String,
    kind: DefinitionKind,
    found: Option<&'i TypeMacro>,
}

/// The types a file's invocations define, being added to its expansion.
struct Adding<'a, 'i> {
    /// The file's own scopes: an added scope's id is past them.
    scopes: &'a [Scope],
    /// The file's own definitions, by their parent and name.
    own: &'a BTreeMap<(Option<ScopeId>, &'i [Segment]), ScopeId>,
    expansion: Expansion,
    /// The depth of each added scope.
    depths: Vec<usize>,
    work: Work,
}

impl Adding<'_, '_> {
    /// Define the type `invocation` names and each member and item its
    /// macro gives that the file does not define itself, over the
    /// invocation's span. The invocation states no visibility: the
    /// macro's body does.
    fn define(
        &mut self,
        invocation: &Invocation,
        defined: &Defined<'_, '_>,
    ) -> Result<(), IndexFault> {
        let container = invocation.container;
        let (members, items) = defined.found.map_or((&[][..], &[][..]), |found| {
            (&found.members[..], &found.items[..])
        });
        for (item, kind) in items {
            self.scope(invocation, container, Segment::name(item), *kind)?;
        }
        let type_scope = self.scope(
            invocation,
            container,
            Segment::name(defined.name),
            defined.kind,
        )?;
        let mut traits: BTreeMap<&[String], ScopeId> = BTreeMap::new();
        for member in members {
            let parent = match &member.trait_tokens {
                None => type_scope,
                Some(tokens) => match traits.get(tokens.as_slice()) {
                    Some(scope) => *scope,
                    None => {
                        let segment = Segment::of_trait(&trait_text(tokens, defined.name));
                        let scope = self.scope(
                            invocation,
                            Some(type_scope),
                            segment,
                            DefinitionKind::Impl,
                        )?;
                        traits.insert(tokens, scope);
                        scope
                    }
                },
            };
            self.scope(
                invocation,
                Some(parent),
                Segment::name(&member.name),
                member.kind,
            )?;
        }
        Ok(())
    }

    /// The scope of `name` under `parent`: the file's own definition of
    /// it, or one added and defined as a `kind` over `invocation`.
    fn scope(
        &mut self,
        invocation: &Invocation,
        parent: Option<ScopeId>,
        name: Segment,
        kind: DefinitionKind,
    ) -> Result<ScopeId, IndexFault> {
        let key = (parent, std::slice::from_ref(&name));
        if let Some(own) = self.own.get(&key) {
            return Ok(*own);
        }
        self.work.spend(1)?;
        let depth = parent.map_or(0, |parent| self.depth(parent)) + 1;
        if depth > MAX_NESTING {
            return Err(IndexFault::TooDeep);
        }
        let id = ScopeId::of(self.scopes.len() + self.expansion.scopes.len())
            .ok_or(IndexFault::TooComplex)?;
        self.expansion.scopes.push(Scope {
            parent,
            name: vec![name],
        });
        self.depths.push(depth);
        self.expansion.definitions.push(Definition {
            scope: id,
            kind,
            span: invocation.span,
            hash: invocation.hash,
            role: invocation.role,
            visibility: None,
        });
        Ok(id)
    }

    /// The segments of the path of `id`.
    fn depth(&self, id: ScopeId) -> usize {
        match id.at().checked_sub(self.scopes.len()) {
            Some(added) => self.depths.get(added).copied().unwrap_or(0),
            None => {
                let file = Scopes {
                    file: self.scopes,
                    added: &[],
                };
                file.path_from_end(id).count()
            }
        }
    }
}

/// The text of a trait a macro's `impl` names, its tokens joined, each
/// `$` the name the invocation defines.
fn trait_text(tokens: &[String], name: &str) -> String {
    tokens
        .iter()
        .map(|token| match token.as_str() {
            "$" => name,
            token => token,
        })
        .collect()
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

    /// What each file's invocations define in the tree of `files`: the
    /// paths, each with its kind, or the fault.
    fn expanded_with_kinds(files: &[(&str, &str)]) -> Result<Vec<String>, IndexFault> {
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
        let crates = Crates::of(&tree, &Files::new());
        let mut found = Vec::new();
        for (path, expansion) in expand(&tree, &crates) {
            let expansion = expansion?;
            let (_, index) = tree[path];
            let scopes = crate::code::scope::Scopes {
                file: &index.scopes,
                added: &expansion.scopes,
            };
            for definition in &expansion.definitions {
                let named = crate::code::testing::path(scopes, definition.scope);
                found.push(format!("{path}: {named} {:?}", definition.kind));
            }
        }
        Ok(found)
    }

    /// The paths each file's invocations define in the tree of `files`.
    fn expanded(files: &[(&str, &str)]) -> Vec<String> {
        expanded_with_kinds(files)
            .unwrap()
            .into_iter()
            .map(|found| found.rsplit_once(' ').unwrap().0.to_owned())
            .collect()
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

    #[test]
    fn a_type_holds_the_items_of_impls_of_its_name_and_a_trait_impls_under_the_trait() {
        let files = [(
            "src/lib.rs",
            "macro_rules! def {\n    ($name:ident) => {\n        pub struct $name;\n        impl $name { fn new() {} }\n        impl<T> From<T> for $name { fn from(t: T) -> Self { $name } }\n        impl From<$name> for String { fn from(x: $name) -> String { String::new() } }\n        impl<$t> Other<$t> { fn other() {} }\n        def!(@more $name);\n    };\n    (@more $t:ident) => { impl $t where $t: Copy { fn more() {} } };\n}\ndef!(Thing);\n",
        )];

        assert_eq!(
            expanded_with_kinds(&files).unwrap(),
            [
                "src/lib.rs: Thing Struct",
                "src/lib.rs: Thing::more Method",
                "src/lib.rs: Thing::new Method",
                "src/lib.rs: Thing::<From<T>> Impl",
                "src/lib.rs: Thing::<From<T>>::from Method",
            ]
        );
    }

    #[test]
    fn an_invocation_finds_its_arm_among_many_by_its_words() {
        let arms: String = (0..10_000)
            .map(|n| format!("(w{n} $n:ident) => {{ struct $n; }};"))
            .collect();
        let calls: String = (0..10_000).map(|n| format!("m!(w{n} T{n});\n")).collect();
        let text = format!("macro_rules! m {{ {arms} }}\n{calls}m!(w9999 Last);\nm!(nowhere X);\n");
        let found = expanded(&[("src/lib.rs", &text)]);

        assert_eq!(found.len(), 10_001);
        assert_eq!(found.last().map(String::as_str), Some("src/lib.rs: Last"));
    }

    #[test]
    fn what_invocations_add_is_bounded_by_the_bytes_they_span() {
        let members: String = (0..1_000).map(|n| format!("fn a{n}() {{}}")).collect();
        let template = format!(
            "macro_rules! m {{ ($n:ident) => {{ pub struct $n; impl $n {{ {members} }} }}; }}\n"
        );
        let calls =
            |count: usize| -> String { (0..count).map(|n| format!("m!(T{n});\n")).collect() };

        let few = format!("{template}{}", calls(10));
        assert_eq!(expanded(&[("src/lib.rs", &few)]).len(), 10_010);
        let many = format!("{template}{}", calls(1_000));
        assert_eq!(
            expanded_with_kinds(&[("src/lib.rs", &many)]),
            Err(IndexFault::TooComplex)
        );
    }

    #[test]
    fn a_type_an_invocation_defines_is_held_to_the_bound_of_segments() {
        let template = "macro_rules! m { ($n:ident) => { pub struct $n; }; }\n";
        let nested = |depth: usize| {
            format!(
                "{template}{}m!(Deep);{}",
                "mod a { ".repeat(depth),
                " }".repeat(depth)
            )
        };

        assert_eq!(
            expanded(&[("src/lib.rs", &nested(MAX_NESTING - 1))]).len(),
            1
        );
        assert_eq!(
            expanded_with_kinds(&[("src/lib.rs", &nested(MAX_NESTING))]),
            Err(IndexFault::TooDeep)
        );
    }
}
