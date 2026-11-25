use super::*;
use crate::code::index::{Element, FileKind, Fragment, HandOff, TraitToken, index as index_file};
use crate::code::kinds::{DefinitionKind, Role, Visibility};
use crate::code::scope::{MAX_NESTING, Scopes};
use crate::code::symbol::Segment;
use crate::code::testing;

fn index(text: &str) -> FileIndex {
    index_file(FileKind::Rust, text.as_bytes()).unwrap()
}

/// Each definition of `text` as its path and the text of its span.
fn defined(text: &str) -> Vec<(String, &str)> {
    testing::defined(FileKind::Rust, text)
}

#[test]
fn an_items_span_takes_in_its_attributes_and_doc_comments() {
    let text = "// A plain comment.\n/// Doc.\n#[derive(Debug)]\n/// More.\npub struct A {\n    #[serde(default)]\n    b: u8,\n}\n";

    assert_eq!(
        defined(text),
        [
            (
                "A".to_owned(),
                "/// Doc.\n#[derive(Debug)]\n/// More.\npub struct A {\n    #[serde(default)]\n    b: u8,\n}"
            ),
            ("A::b".to_owned(), "#[serde(default)]\n    b: u8"),
        ]
    );
}

#[test]
fn a_trait_impl_is_a_definition_and_an_inherent_one_is_not() {
    let text = "impl<T> a::Table<T> { fn f() {} }\nimpl fmt::Display for &Table { fn fmt() {} }\nimpl dyn Shape { fn area() {} }\nimpl Trait for (u8, u16) {}\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert_eq!(
        paths,
        [
            "Table::f",
            "Table::<Display>",
            "Table::<Display>::fmt",
            "Shape::area",
            "(u8,u16)::<Trait>",
        ]
    );
}

#[test]
fn a_reexport_of_a_path_the_file_defines_is_no_second_definition() {
    let text = "macro_rules! m { () => {} }\npub(crate) use m;\npub use inner::Thing;\nmod inner { pub struct Thing; }\npub use other::{self as renamed, *};\nuse private::Name;\npub use x::y as _;\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert_eq!(paths, ["m", "Thing", "inner", "inner::Thing", "renamed"]);
}

#[test]
fn an_item_an_invocation_spells_spans_its_attributes_to_its_body() {
    let text = "proptest! {\n    #[test]\n    pub(crate) fn holds(x in 0..9) { assert!(x < 9) }\n    #[test]\n    fn also() {}\n}\nfn f() { thread_local! { static SLOT: u8 = 0; } }\nconst X: u8 = m!(fn not_an_item() {});\n";

    assert_eq!(
        defined(text),
        [
            (
                "holds".to_owned(),
                "#[test]\n    pub(crate) fn holds(x in 0..9) { assert!(x < 9) }"
            ),
            ("also".to_owned(), "#[test]\n    fn also() {}"),
            (
                "f".to_owned(),
                "fn f() { thread_local! { static SLOT: u8 = 0; } }"
            ),
            ("f::SLOT".to_owned(), "static SLOT: u8 = 0;"),
            ("X".to_owned(), "const X: u8 = m!(fn not_an_item() {});"),
        ]
    );
}

#[test]
fn a_type_macro_keeps_each_rules_opening_and_what_it_spells_of_each_binding() {
    let text = "#[macro_export]\nmacro_rules! id {\n    ($(#[$m:meta])* ordered $name:ident) => { pub enum $name { A } impl $name { fn rank() {} } };\n    ($name:ident, $t:ty) => { pub type $name = $t; };\n    (@inner $name:ident) => {};\n    ($vis:vis struct $name:ident) => { $vis struct $name; };\n}\nmacro_rules! call { ($x:ident) => { let _ = $x; } }\n";
    let macros = index(text).rust.macros;
    let [found] = macros.as_slice() else {
        panic!("{macros:?}");
    };
    let receives: Vec<Vec<(usize, Option<DefinitionKind>)>> = found
        .rules
        .iter()
        .map(|rule| rule.receives.iter().map(|(at, r)| (*at, r.kind)).collect())
        .collect();

    let openings: Vec<&[Element]> = found
        .rules
        .iter()
        .map(|rule| rule.opening.as_slice())
        .collect();
    let ident = Element::Binding(Fragment::Ident);

    assert!(found.exported);
    assert_eq!(
        openings,
        [
            &[Element::Word("ordered".into()), ident.clone()][..],
            &[
                ident.clone(),
                Element::Token(",".into()),
                Element::Binding(Fragment::Other)
            ],
            &[
                Element::Token("@".into()),
                Element::Word("inner".into()),
                ident.clone()
            ],
            &[
                Element::Binding(Fragment::Other),
                Element::Word("struct".into()),
                ident
            ],
        ]
    );
    assert_eq!(
        receives,
        [
            vec![(1, Some(DefinitionKind::Enum))],
            vec![(0, Some(DefinitionKind::TypeAlias))],
            vec![],
            vec![(2, Some(DefinitionKind::Struct))],
        ]
    );
}

#[test]
fn an_invocation_keeps_its_first_elements_past_its_attributes() {
    let text = "mod m {\n    ids::id! {\n        /// Doc.\n        #[allow(dead_code)]\n        ordered Rank(u8), \"a b\"\n    }\n}\nfn f() { id! { ordered Local(u8) } }\n";
    let invocations = index(text).rust.invocations;

    assert_eq!(invocations.len(), 1);
    let invocation = &invocations[0];
    assert_eq!(invocation.macro_name, "id");
    let indexed = index(text);
    let scopes = Scopes {
        file: &indexed.scopes,
        added: &[],
    };
    assert_eq!(testing::path(scopes, invocation.container.unwrap()), "m");
    assert_eq!(
        invocation.arguments,
        [
            Element::Word("ordered".into()),
            Element::Word("Rank".into()),
            Element::Group('(', Some(vec!["u8".into()])),
            Element::Token(",".into()),
            Element::Token("\"a b\"".into()),
        ]
    );
    assert!(invocation.whole);
}

#[test]
fn a_nesting_past_the_bound_has_no_index() {
    let nested = |depth: usize| format!("{}{}", "mod m {".repeat(depth), "}".repeat(depth));

    assert!(index_file(FileKind::Rust, nested(MAX_NESTING).as_bytes()).is_ok());
    assert_eq!(
        index_file(FileKind::Rust, nested(MAX_NESTING + 1).as_bytes()),
        Err(IndexFault::TooDeep)
    );
}

#[test]
fn a_file_mid_edit_still_defines_what_parses() {
    let text = "pub struct Kept;\nfn broken( {\nimpl Kept { fn method(&self) {} }\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert!(paths.contains(&"Kept".to_owned()), "{paths:?}");
}

#[test]
fn a_macro_is_exported_by_its_attribute_not_by_a_word_in_its_docs() {
    let text = "/// Not #[macro_export]: local.\nmacro_rules! local { () => {} }\n#[cfg_attr(feature = \"x\", macro_export)]\nmacro_rules! maybe { () => {} }\n#[macro_export]\nmacro_rules! shared { () => {} }\n";
    let indexed = index(text);
    let visibility: Vec<_> = indexed.definitions.iter().map(|d| d.visibility).collect();

    assert_eq!(
        visibility,
        [
            Some(Visibility::Private),
            Some(Visibility::Public),
            Some(Visibility::Public)
        ]
    );
}

#[test]
fn a_macro_of_many_rules_is_read_within_the_budget() {
    let rules: String = (0..20_000)
        .map(|n| format!("(w{n} $n:ident) => {{ pub struct $n; }};\n"))
        .collect();
    let text = format!("macro_rules! many {{\n{rules}}}\n");

    assert_eq!(index(&text).rust.macros[0].rules.len(), 20_000);
}

#[test]
fn a_spelled_module_holds_its_items_each_hashed_by_what_it_holds() {
    let depth = MAX_NESTING - 1;
    let text = format!(
        "m! {{ {}fn f() {{}}{} }}\n",
        "mod a { ".repeat(depth),
        "}".repeat(depth)
    );
    let indexed = index(&text);
    let hashes: std::collections::BTreeSet<_> =
        indexed.definitions.iter().map(|d| d.hash).collect();
    let deepest = defined(&text).pop().map(|(path, _)| path);

    assert_eq!(indexed.definitions.len(), depth + 1);
    assert_eq!(
        hashes.len(),
        depth + 1,
        "each holds one more than the one inside it"
    );
    assert_eq!(deepest, Some(format!("{}f", "a::".repeat(depth))));
}

#[test]
fn an_invocation_in_an_impl_spells_its_types_members_alone() {
    let text = "struct T;\nimpl T {\n    forward! { bool struct identifier fn made() {} }\n}\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert_eq!(paths, ["T", "T::made"]);
}

#[test]
fn a_tree_after_a_name_or_in_an_items_body_defines_nothing() {
    let text = "class!(Meters {\n    pub fn new() -> Self { Self }\n});\nm! { impl Seconds { fn new() {} } fn outer() { fn local() {} } }\ncfg_if! { if #[cfg(unix)] { fn branch() {} } }\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert_eq!(paths, ["outer", "branch"]);
}

#[test]
fn a_plain_comment_between_an_attribute_and_its_item_keeps_the_mark() {
    let text = "#[cfg(test)]\n// the tests\nmod tests {\n    fn helper() {}\n}\n#[test]\n/* why */\nfn unit() {}\npub(/* only here */ crate) fn narrow() {}\n";
    let marks: Vec<_> = index(text)
        .definitions
        .iter()
        .map(|d| (d.role, d.visibility))
        .collect();

    assert_eq!(
        marks,
        [
            (Role::Test, Some(Visibility::Private)),
            (Role::Test, Some(Visibility::Private)),
            (Role::Test, Some(Visibility::Private)),
            (Role::Code, Some(Visibility::Crate)),
        ]
    );
}

#[test]
fn an_item_in_a_discriminant_has_its_own_visibility() {
    let text = "pub enum E { C = { const K: isize = 1; K } }\n";
    let indexed = index(text);
    let constant = indexed
        .definitions
        .iter()
        .find(|d| d.kind == DefinitionKind::Constant)
        .unwrap();

    assert_eq!(constant.visibility, Some(Visibility::Private));
}

#[test]
fn a_type_macros_members_are_its_impl_blocks_items_and_the_rest_its_containers() {
    let text = "macro_rules! def {\n    ($name:ident) => {\n        pub struct $name;\n        pub fn make() -> $name { $name }\n        pub mod helpers {}\n        impl $name { pub fn get(&self) {} }\n        impl Other { fn not_its() {} }\n    };\n}\n";
    let found = &index(text).rust.macros[0];
    let named = |named: &[(std::sync::Arc<Segment>, DefinitionKind)]| -> Vec<String> {
        named
            .iter()
            .map(|(name, kind)| format!("{name} {kind:?}"))
            .collect()
    };
    let [rule] = found.rules.as_slice() else {
        panic!("{found:?}");
    };
    let impls: Vec<(Option<Vec<TraitToken>>, Vec<String>)> = rule
        .receives
        .iter()
        .flat_map(|(_, receives)| &receives.impls)
        .map(|found| (found.trait_tokens.clone(), named(&found.members)))
        .collect();

    assert_eq!(impls, [(None, vec!["get Method".to_owned()])]);
    assert_eq!(named(&rule.items), ["helpers Module", "make Function"]);
}

#[test]
fn a_rule_keeps_each_distinct_hand_off_of_a_binding_with_its_arguments() {
    let text = "macro_rules! entity {\n    (record $name:ident) => { declare!([] $name); declare!([] $name); };\n    (quiet $name:ident) => { let _ = $name; };\n    (moved $name:ident) => { $crate::path::declare! { $name } };\n}\n";
    let macros = index(text).rust.macros;
    let hand_offs: Vec<&[HandOff]> = macros[0]
        .rules
        .iter()
        .map(|rule| rule.hand_offs.as_slice())
        .collect();
    let declare = |from_crate: bool, arguments: Vec<Element>| HandOff {
        macro_name: "declare".to_owned(),
        from_crate,
        arguments,
        whole: true,
    };
    let name = Element::Forwarded(Some(1), Fragment::Ident);

    assert_eq!(
        hand_offs,
        [
            &[declare(
                false,
                vec![Element::Group('[', Some(vec![])), name.clone()]
            )][..],
            &[],
            &[declare(true, vec![name])],
        ]
    );
}
