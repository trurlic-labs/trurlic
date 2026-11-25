//! The second step of the index: the types the macro invocations of each
//! Rust file define, from the type-defining macros its crate and its
//! workspace hold (`crates.rs`). A file's own index is a function of its
//! bytes alone; this step is a function of the indexes of the whole tree.
//!
//! An invocation sees each `macro_rules!` macro of its crate, and each
//! `#[macro_export]` one of its workspace that the crate does not shadow
//! with one of the same name. It defines the name after the words of the
//! arm it opens (`expand/table.rs`), under the container it is in. Its
//! kind is what the arm's rule, or a rule the name is handed on to, spells
//! (`rust/arms.rs`). The type holds the items of the `impl`s of it in
//! every rule the name reached, a trait impl's under `Name::<Trait>`.
//!
//! An expansion is not cached, so its memory is bounded by construction.
//! A name the macro gives is shared with the macro, never copied per
//! invocation. Every added definition is charged the bytes it holds
//! against a budget of the bytes the file's invocations span
//! ([`Work::expansion`]), and every path it makes is held to
//! [`MAX_NESTING`]. Past either, the file has no index.

mod table;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::crates::Crates;
use super::index::{
    Definition, FileIndex, FileKind, IndexFault, Invocation, Receives, Rule, TypeImpl,
};
use super::kinds::DefinitionKind;
use super::rust::is_word;
use super::scope::{AddedScope, MAX_NESTING, ScopeId, Scopes};
use super::span::Span;
use super::symbol::Segment;
use super::work::Work;

pub(super) use table::{Arms, Held, Lookup};

/// Macros of other crates that define the type their argument at this
/// position names, and its kind. The facts of their crates are not in the
/// tree, so they are listed by hand: pyo3's
/// `create_exception!(module, Name, Base)` and `import_exception!(module,
/// Name)`, each a struct.
const FOREIGN_TYPE_MACROS: [(&str, usize, DefinitionKind); 2] = [
    ("create_exception", 1, DefinitionKind::Struct),
    ("import_exception", 1, DefinitionKind::Struct),
];

/// The bytes an added definition holds besides a name of its own: its
/// scope, its definition and its depth.
const ADDED_BYTES: usize = size_of::<AddedScope>() + size_of::<Definition>() + size_of::<usize>();

/// What the invocations of one file add: scopes past the file's own, and
/// the definitions in them.
#[derive(Debug, Default)]
pub(super) struct Expansion {
    pub(super) scopes: Vec<AddedScope>,
    pub(super) definitions: Vec<Definition>,
}

/// The files of a tree: a path to its kind and index.
pub(super) type Files<'i> = BTreeMap<&'i str, (FileKind, &'i FileIndex)>;

/// The definitions the invocations of each Rust file of `files`, the files
/// the walk took, add, by path, or why they cannot be added; `crates` is
/// of the same files.
pub(super) fn expand<'i>(
    files: &Files<'i>,
    crates: &Crates<'i>,
) -> BTreeMap<&'i str, Result<Expansion, IndexFault>> {
    let mut tables: BTreeMap<&str, Arms<'i>> = BTreeMap::new();
    let mut expanded = BTreeMap::new();
    for (&path, (kind, index)) in files {
        if *kind != FileKind::Rust || index.rust.invocations.is_empty() {
            continue;
        }
        let shared = tables
            .entry(crates.crate_of(path))
            .or_insert_with(|| crates.arms_of(path));
        let lookup = Lookup { own: None, shared };
        if let Some(expansion) = expand_file(index, lookup).transpose() {
            expanded.insert(path, expansion);
        }
    }
    expanded
}

/// What the invocations of `index` add with the arms `lookup` finds;
/// `None` when they add nothing.
pub(super) fn expand_file(
    index: &FileIndex,
    lookup: Lookup<'_, '_>,
) -> Result<Option<Expansion>, IndexFault> {
    if index.rust.invocations.is_empty() {
        return Ok(None);
    }
    // An invocation that spells the item it defines (`m! { struct X }`)
    // defines it once: the file's own index holds it, within the span.
    let mut own: Own<'_> = BTreeMap::new();
    for definition in &index.definitions {
        if let Some(scope) = index.scopes.get(definition.scope.at()) {
            own.entry((scope.parent, scope.name.as_slice()))
                .or_default()
                .push((definition.scope, definition.span));
        }
    }
    let spanned = index
        .rust
        .invocations
        .iter()
        .map(|invocation| invocation.span.end - invocation.span.start)
        .sum();
    let mut adding = Adding {
        scopes: &index.scopes,
        own: &own,
        expansion: Expansion::default(),
        depths: Vec::new(),
        this_invocation: BTreeMap::new(),
        work: Work::expansion(spanned),
    };
    for invocation in &index.rust.invocations {
        if let Some(defined) = lookup.defined_by(invocation) {
            adding.define(invocation, &defined)?;
        }
    }
    let expansion = adding.expansion;
    Ok((!expansion.definitions.is_empty()).then_some(expansion))
}

/// The file's own definitions by their parent and name: each one's scope
/// and span.
type Own<'i> = BTreeMap<(Option<ScopeId>, &'i [Segment]), Vec<(ScopeId, Span)>>;

/// What an invocation defines: the type's name and kind, and the rules
/// that received the name, whose `impl`s' items the type holds and whose
/// items its container does; none for a foreign macro.
struct Defined<'a, 'i> {
    name: &'a String,
    kind: DefinitionKind,
    reached: Vec<(&'i Rule, &'i Receives)>,
}

/// The types a file's invocations define, being added to its expansion.
struct Adding<'a, 'i> {
    /// The file's own scopes: an added scope's id is past them.
    scopes: &'a [super::scope::Scope],
    /// The file's own definitions, by their parent and name.
    own: &'a Own<'i>,
    expansion: Expansion,
    /// The depth of each added scope.
    depths: Vec<usize>,
    /// What the invocation being added has added, by parent and name: two
    /// rules it may reach that give one member give it once.
    this_invocation: BTreeMap<(Option<ScopeId>, Arc<Segment>), ScopeId>,
    /// The bytes the expansion may still hold.
    work: Work,
}

impl Adding<'_, '_> {
    /// Define the type `invocation` names and each member and item its
    /// macros give that the file does not define itself, over the
    /// invocation's span. The invocation states no visibility: the
    /// macro's body does.
    fn define(
        &mut self,
        invocation: &Invocation,
        defined: &Defined<'_, '_>,
    ) -> Result<(), IndexFault> {
        self.this_invocation.clear();
        let container = invocation.container;
        let items: BTreeSet<&(Arc<Segment>, DefinitionKind)> = defined
            .reached
            .iter()
            .flat_map(|(rule, _)| &rule.items)
            .collect();
        for (item, kind) in items {
            self.scope(invocation, container, Arc::clone(item), *kind)?;
        }
        let name = Arc::new(Segment::name(defined.name));
        let type_scope = self.scope(invocation, container, name, defined.kind)?;
        self.define_members(invocation, defined, type_scope)
    }

    /// Define the members the impls of `defined`'s reached rules give the
    /// type at `type_scope`, a trait impl's under the trait's segment.
    fn define_members(
        &mut self,
        invocation: &Invocation,
        defined: &Defined<'_, '_>,
        type_scope: ScopeId,
    ) -> Result<(), IndexFault> {
        let impls: BTreeSet<&TypeImpl> = defined
            .reached
            .iter()
            .flat_map(|(_, receives)| &receives.impls)
            .collect();
        let mut traits: BTreeMap<&[String], ScopeId> = BTreeMap::new();
        for found_impl in impls {
            let parent = match &found_impl.trait_tokens {
                None => type_scope,
                Some(tokens) => match traits.get(tokens.as_slice()) {
                    Some(scope) => *scope,
                    None => {
                        let segment = Segment::of_trait(&trait_text(tokens, defined.name));
                        let scope = self.scope(
                            invocation,
                            Some(type_scope),
                            Arc::new(segment),
                            DefinitionKind::Impl,
                        )?;
                        traits.insert(tokens, scope);
                        scope
                    }
                },
            };
            for (member, kind) in &found_impl.members {
                self.scope(invocation, Some(parent), Arc::clone(member), *kind)?;
            }
        }
        Ok(())
    }

    /// The scope of `name` under `parent`: the file's own definition of it
    /// within `invocation`'s span, else one added as a `kind` over
    /// `invocation`. An added one is charged what it holds: a fixed size,
    /// and the name when no other holds it. A definition of the path
    /// outside the span is a twin the set of the path takes in.
    fn scope(
        &mut self,
        invocation: &Invocation,
        parent: Option<ScopeId>,
        name: Arc<Segment>,
        kind: DefinitionKind,
    ) -> Result<ScopeId, IndexFault> {
        let key = (parent, std::slice::from_ref(&*name));
        let within =
            |span: &Span| span.start >= invocation.span.start && span.end <= invocation.span.end;
        let spelled = self.own.get(&key).into_iter().flatten();
        if let Some((own, _)) = spelled.into_iter().find(|(_, span)| within(span)) {
            return Ok(*own);
        }
        if let Some(added) = self.this_invocation.get(&(parent, Arc::clone(&name))) {
            return Ok(*added);
        }
        let held = match Arc::strong_count(&name) {
            1 => segment_bytes(&name),
            _ => 0,
        };
        self.work.spend(ADDED_BYTES + held)?;
        let depth = parent.map_or(0, |parent| self.depth(parent)) + 1;
        if depth > MAX_NESTING {
            return Err(IndexFault::TooDeep);
        }
        let id = ScopeId::of(self.scopes.len() + self.expansion.scopes.len())
            .ok_or(IndexFault::TooComplex)?;
        self.this_invocation.insert((parent, Arc::clone(&name)), id);
        self.expansion.scopes.push(AddedScope { parent, name });
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

/// The bytes of the texts `segment` holds.
fn segment_bytes(segment: &Segment) -> usize {
    match segment {
        Segment::Name(name) => name.len(),
        Segment::Impl { name, arguments } => name.len() + arguments.as_ref().map_or(0, String::len),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::crates::Crates;
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

    /// What `text`, the `src/lib.rs` of one crate, defines by its
    /// invocations, each path with its kind.
    fn one_crate(text: &str) -> Vec<String> {
        expanded_with_kinds(&[("Cargo.toml", "[package]\n"), ("src/lib.rs", text)])
            .unwrap()
            .into_iter()
            .map(|found| found.trim_start_matches("src/lib.rs: ").to_owned())
            .collect()
    }

    #[test]
    fn an_arm_is_opened_only_by_what_its_pattern_holds_around_the_name() {
        let text = "macro_rules! helper { ($name:ident) => { pub struct $name; impl $name { pub fn shown(&self) {} } }; }\nmacro_rules! front {\n    ($name:ident in $module:literal) => { helper!($name); };\n    (compared $name:ident in $module:literal) => { helper!($name); };\n    (enum $source:path as $name:ident) => { helper!($name); };\n}\nfront!(Plain in \"m\");\nfront!(compared Shown in \"m\");\nfront!(enum other::Source as Mirrored);\nfront!(enum in \"m\");\n";

        assert_eq!(
            one_crate(text),
            [
                "Plain Struct",
                "Plain::shown Method",
                "Shown Struct",
                "Shown::shown Method",
            ]
        );
    }

    #[test]
    fn an_arm_spells_its_type_or_hands_the_name_on_and_no_other_arm_decides() {
        let text = "macro_rules! record { ($name:ident) => { pub struct $name; impl $name { pub fn new() -> Self { Self } } }; }\nmacro_rules! model {\n    (choice $name:ident) => { pub enum $name { A } };\n    (record $name:ident) => { record!($name); };\n    (inner $name:ident) => { model!(@spelled $name); };\n    (@spelled $n:ident) => { pub union $n { a: u8 } impl $n { fn inner(&self) {} } };\n    (also $name:ident) => { pub struct $name; extra!($name); };\n}\nmacro_rules! extra { ($e:ident) => { impl $e { pub fn extra(&self) {} } }; }\nmodel!(record Account);\nmodel!(inner Deep);\nmodel!(also Meters);\n";

        assert_eq!(
            one_crate(text),
            [
                "Account Struct",
                "Account::new Method",
                "Deep Union",
                "Deep::inner Method",
                "Meters Struct",
                "Meters::extra Method",
            ]
        );
    }

    #[test]
    fn a_hand_off_is_received_by_the_metavariable_at_its_place() {
        let text = "macro_rules! pair { ($name:ident, $error:ident) => {\n    pub enum $error { Failed }\n    pub fn $name() -> Result<(), $error> { Ok(()) }\n}; }\nmacro_rules! operation {\n    (op $name:ident) => { pair!($name, OperationError); };\n    (error $name:ident) => { pair!(run, $name); };\n    (both $name:ident $kind:ident) => { pair!($kind, $name); };\n}\noperation!(op run);\noperation!(error Failure);\noperation!(both Failure2 run2);\n";

        assert_eq!(one_crate(text), ["Failure Enum", "Failure2 Enum"]);
    }

    #[test]
    fn a_hand_off_by_path_reaches_the_macro_of_its_last_name() {
        let text = "macro_rules! helper { ($name:ident) => { pub struct $name; }; }\nmacro_rules! front {\n    (plain $name:ident) => { self::helper! { $name } };\n    (rooted $name:ident) => { $crate::helper!($name); };\n}\nfront!(plain ByPath);\nfront!(rooted ByRoot);\n";

        assert_eq!(one_crate(text), ["ByPath Struct", "ByRoot Struct"]);
    }

    #[test]
    fn a_crate_hand_off_reaches_the_defining_crates_macro() {
        let helper = |kind: &str, member: &str| {
            format!(
                "macro_rules! helper {{ ($name:ident) => {{ pub {kind} $name {{}} impl $name {{ pub fn {member}(&self) {{}} }} }}; }}\n"
            )
        };
        let exported = format!(
            "#[macro_export]\n{}#[macro_export]\nmacro_rules! front {{ (make $name:ident) => {{ $crate::helper!($name); }}; }}\n",
            helper("struct", "from_a")
        );
        let own = format!("{}front!(make InB);\n", helper("enum", "from_b"));
        let files = [
            ("Cargo.toml", "[workspace]\n"),
            ("a/Cargo.toml", "[package]\n"),
            ("a/src/lib.rs", exported.as_str()),
            ("b/Cargo.toml", "[package]\n"),
            ("b/src/lib.rs", own.as_str()),
        ];

        assert_eq!(
            expanded_with_kinds(&files).unwrap(),
            [
                "b/src/lib.rs: InB Struct",
                "b/src/lib.rs: InB::from_a Method"
            ]
        );
    }

    #[test]
    fn a_name_goes_through_at_most_eight_hand_offs_and_a_cycle_ends() {
        let chain = |prefix: &str, hops: usize| -> String {
            let mut text = format!(
                "macro_rules! {prefix}0 {{ (go $name:ident) => {{ {prefix}1!(x $name); }}; }}\n"
            );
            for hop in 1..hops {
                text.push_str(&format!(
                    "macro_rules! {prefix}{hop} {{ (x $name:ident) => {{ {prefix}{}!(x $name); }}; }}\n",
                    hop + 1
                ));
            }
            text.push_str(&format!(
                "macro_rules! {prefix}{hops} {{ (x $name:ident) => {{ pub struct $name; }}; }}\n"
            ));
            text
        };
        let text = format!(
            "{}{}macro_rules! ping {{ (go $name:ident) => {{ pong!(go $name); }}; }}\nmacro_rules! pong {{ (go $name:ident) => {{ ping!(go $name); }}; }}\nc8_0!(go Hops8);\nc9_0!(go Hops9);\nping!(go Cycle);\n",
            chain("c8_", 8),
            chain("c9_", 9)
        );

        assert_eq!(one_crate(&text), ["Hops8 Struct"]);
    }

    #[test]
    fn an_impl_gives_members_to_the_name_it_is_of_alone() {
        let text = "macro_rules! pair { ($a:ident, $b:ident) => { pub struct $a; pub struct $b; impl $b { fn only_b() {} } impl From<$b> for $a { fn from(_: $b) -> Self { $a } } }; }\nmacro_rules! gen { ($n:ident) => { pub struct $n<T>(T); impl<T> $n<T> { fn new() {} } impl<F: Fn() -> u8> Tr2<F> for $n { fn g() {} } impl Tr<$n> for $n { fn t() {} } }; }\npair!(A, B);\ngen!(G);\n";

        assert_eq!(
            one_crate(text),
            [
                "A Struct",
                "A::<From<$b>> Impl",
                "A::<From<$b>>::from Method",
                "G Struct",
                "G::new Method",
                "G::<Tr<G>> Impl",
                "G::<Tr<G>>::t Method",
                "G::<Tr2<F>> Impl",
                "G::<Tr2<F>>::g Method",
            ]
        );
    }

    #[test]
    fn a_path_written_directly_and_by_an_invocation_is_one_set() {
        let text = "macro_rules! id { ($name:ident) => { pub struct $name(u8); }; }\n#[cfg(unix)]\npub struct Foo(u8);\n#[cfg(not(unix))]\nid!(Foo);\nm! { struct Spelled; }\nmacro_rules! m { ($k:ident $name:ident;) => { $k $name; }; }\n";

        assert_eq!(one_crate(text), ["Foo Struct"]);
    }

    #[test]
    fn an_expansion_shares_the_names_its_macro_gives() {
        let long = "a".repeat(100_000);
        let calls: String = (0..40).map(|n| format!("m!(T{n});\n")).collect();
        let text = format!(
            "macro_rules! m {{ ($n:ident) => {{ pub struct $n; impl $n {{ fn {long}() {{}} }} }}; }}\n{calls}"
        );
        let index = index(FileKind::Rust, text.as_bytes()).unwrap();
        let manifest = index_manifest();
        let tree: Files<'_> = [
            ("Cargo.toml", (FileKind::Toml, &manifest)),
            ("src/lib.rs", (FileKind::Rust, &index)),
        ]
        .into_iter()
        .collect();
        let crates = Crates::of(&tree, &Files::new());
        let expansion = expand(&tree, &crates)
            .remove("src/lib.rs")
            .unwrap()
            .unwrap();
        let members: Vec<&Arc<Segment>> = expansion
            .scopes
            .iter()
            .filter(|scope| matches!(&*scope.name, Segment::Name(name) if name.len() == long.len()))
            .map(|scope| &scope.name)
            .collect();

        assert_eq!(members.len(), 40);
        assert!(members.windows(2).all(|pair| Arc::ptr_eq(pair[0], pair[1])));
    }

    fn index_manifest() -> FileIndex {
        index(FileKind::Toml, b"[package]\n").unwrap()
    }
}
