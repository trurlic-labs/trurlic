use crate::code::index::{
    DefinitionKind, FileIndex, FileKind, IndexFault, MAX_NESTING, Role, Scopes, Visibility,
};
use crate::code::testing;

fn index(text: &str) -> FileIndex {
    crate::code::testing::index(FileKind::Rust, text.as_bytes()).unwrap()
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

/// Trait impls with a comment in their arguments, and as rustfmt 1.95's
/// defaults format them: a trailing comma added to the tuple and to the
/// trait's arguments it breaks.
const IMPLS: &str = "pub struct Table;\nimpl Shape for (TypeNameNumberOne, TypeNameNumberTwo, TypeNameNumberThree, TypeNameNumberFour) { fn area() {} }\nimpl Convert<TypeNameNumberOne, TypeNameNumberTwo, TypeNameNumberThree, TypeNameNumberFour, TypeNameNumberFive> for Table { fn go() {} }\nimpl Convert</* why */ u8> for Table {}\n";

const IMPLS_FORMATTED: &str = "pub struct Table;\nimpl Shape\n    for (\n        TypeNameNumberOne,\n        TypeNameNumberTwo,\n        TypeNameNumberThree,\n        TypeNameNumberFour,\n    )\n{\n    fn area() {}\n}\nimpl\n    Convert<\n        TypeNameNumberOne,\n        TypeNameNumberTwo,\n        TypeNameNumberThree,\n        TypeNameNumberFour,\n        TypeNameNumberFive,\n    > for Table\n{\n    fn go() {}\n}\nimpl Convert</* why */ u8> for Table {}\n";

#[test]
fn an_impls_path_is_spelled_from_its_tokens_so_rustfmt_and_a_comment_move_none() {
    let paths = |text| -> Vec<String> { defined(text).into_iter().map(|(path, _)| path).collect() };
    let tuple = "(TypeNameNumberOne,TypeNameNumberTwo,TypeNameNumberThree,TypeNameNumberFour)";
    let arguments = "<TypeNameNumberOne,TypeNameNumberTwo,TypeNameNumberThree,TypeNameNumberFour,TypeNameNumberFive>";

    assert_eq!(paths(IMPLS_FORMATTED), paths(IMPLS));
    assert_eq!(
        paths(IMPLS),
        [
            "Table".to_owned(),
            format!("{tuple}::<Shape>"),
            format!("{tuple}::<Shape>::area"),
            format!("Table::<Convert{arguments}>"),
            format!("Table::<Convert{arguments}>::go"),
            "Table::<Convert<u8>>".to_owned(),
        ]
    );
}

#[test]
fn an_anchor_names_an_impl_whatever_its_spaces_and_trailing_commas() {
    for symbol in [
        "(TypeNameNumberOne, TypeNameNumberTwo, TypeNameNumberThree, TypeNameNumberFour,)::<Shape>::area",
        "(TypeNameNumberOne,TypeNameNumberTwo,TypeNameNumberThree,TypeNameNumberFour)::area",
        "Table::<Convert<TypeNameNumberOne, TypeNameNumberTwo, TypeNameNumberThree, TypeNameNumberFour, TypeNameNumberFive,>>::go",
        "Table::<Convert<u8>>",
    ] {
        assert_eq!(
            testing::named(FileKind::Rust, IMPLS_FORMATTED, symbol).len(),
            1,
            "{symbol}"
        );
    }
    let tuples = "impl Shape for (u8,) {}\nimpl Shape for (u8, u16,) {}\n";
    let named = |symbol| testing::named(FileKind::Rust, tuples, symbol);
    assert_eq!(named("(u8,)::<Shape>"), ["impl Shape for (u8,) {}"]);
    assert_eq!(named("(u8)::<Shape>"), Vec::<&str>::new());
    assert_eq!(
        named("(u8, u16)::<Shape>"),
        ["impl Shape for (u8, u16,) {}"]
    );
}

#[test]
fn a_raw_identifier_is_defined_and_named_as_its_name() {
    let text = "pub struct r#Raw;\nimpl r#Raw { pub fn r#match() {} }\nimpl Raw { pub fn plain() {} }\nimpl From<r#Raw> for (r#Raw, u8) {}\npub use inner::r#Moved;\nmod inner { pub struct Moved; }\n";
    let paths: Vec<String> = defined(text).into_iter().map(|(path, _)| path).collect();

    assert_eq!(
        paths,
        [
            "Raw",
            "Raw::match",
            "Raw::plain",
            "(Raw,u8)::<From<Raw>>",
            "Moved",
            "inner",
            "inner::Moved"
        ]
    );
    for (symbol, spelled) in [("Raw", "r#Raw"), ("Raw::match", "r#Raw::r#match")] {
        assert_eq!(
            testing::named(FileKind::Rust, text, spelled),
            testing::named(FileKind::Rust, text, symbol)
        );
        assert_eq!(
            testing::named(FileKind::Rust, text, symbol).len(),
            1,
            "{symbol}"
        );
    }
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

    assert!(crate::code::testing::index(FileKind::Rust, nested(MAX_NESTING).as_bytes()).is_ok());
    assert_eq!(
        crate::code::testing::index(FileKind::Rust, nested(MAX_NESTING + 1).as_bytes()),
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
        (
            "nested blocks of uses",
            format!("fn f() {}", "{ use a::b; ".repeat((mib - 8) / 12)),
        ),
    ];
    for (case, text) in cases {
        assert!(text.len() <= mib, "{case}");
        match crate::code::testing::index(FileKind::Rust, text.as_bytes()) {
            Ok(_) | Err(IndexFault::TooComplex) => {}
            Err(other) => panic!("{case}: {other}"),
        }
    }
}
