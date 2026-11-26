use super::*;
use crate::code::index::{FileKind, index as index_file};
use crate::code::kinds::{DefinitionKind, Role, Visibility};
use crate::code::scope::{MAX_NESTING, Scopes};
use crate::code::testing;

fn index(text: &str) -> FileIndex {
    index_file(
        FileKind::Rust,
        text.as_bytes(),
        &crate::code::BlankMacros::default(),
    )
    .unwrap()
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
fn a_macro_rules_macro_is_a_definition_and_no_invocation_defines_anything() {
    let text = "macro_rules! made { ($n:ident) => { pub struct $n; }; }\nmade!(Made);\nproptest! {\n    #[test]\n    fn holds(x in 0..9) { assert!(x < 9) }\n}\nview! { pub struct View { open: u8 } }\nthread_local! { static SLOT: u8 = 0; }\ncfg_if! { if #[cfg(unix)] { fn branch() {} } }\nimpl T { forward! { fn made() {} } }\nfn f() { local! { struct Local; } }\n";
    let defined: Vec<(String, DefinitionKind)> = {
        let indexed = index(text);
        let scopes = Scopes(&indexed.scopes);
        indexed
            .definitions
            .iter()
            .map(|d| (testing::path(scopes, d.scope), d.kind))
            .collect()
    };

    assert_eq!(
        defined,
        [
            ("made".to_owned(), DefinitionKind::Macro),
            ("f".to_owned(), DefinitionKind::Function),
        ]
    );
}

#[test]
fn a_nesting_past_the_bound_has_no_index() {
    let nested = |depth: usize| format!("{}{}", "mod m {".repeat(depth), "}".repeat(depth));

    assert!(
        index_file(
            FileKind::Rust,
            nested(MAX_NESTING).as_bytes(),
            &crate::code::BlankMacros::default()
        )
        .is_ok()
    );
    assert_eq!(
        index_file(
            FileKind::Rust,
            nested(MAX_NESTING + 1).as_bytes(),
            &crate::code::BlankMacros::default()
        ),
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
fn hostile_rust_is_refused_or_read_in_linear_work() {
    let mib = 1024 * 1024;
    let body = |unit: &str| format!("fn f() {{ let x = {}", unit.repeat((mib - 20) / unit.len()));
    let cases = [
        ("unclosed brackets", body("(")),
        ("closures", body("|a| ")),
        ("generic calls", body("a<b>(c)<")),
        ("comparisons", body("a < b > ")),
        ("references", body("&")),
        (
            "macro repetitions",
            format!("m!{{{}", "$($".repeat((mib - 4) / 3)),
        ),
    ];
    for (case, text) in cases {
        assert!(text.len() <= mib, "{case}");
        match testing::on_parse_stack(|| {
            index_file(
                FileKind::Rust,
                text.as_bytes(),
                &crate::code::BlankMacros::default(),
            )
        }) {
            Ok(_) | Err(IndexFault::TooComplex) => {}
            Err(other) => panic!("{case}: {other}"),
        }
    }
}
