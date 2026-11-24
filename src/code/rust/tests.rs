use super::*;
use crate::code::index::{FileKind, TypeArm, TypeMacro, index as index_file};
use crate::code::kinds::DefinitionKind;

fn index(text: &str) -> FileIndex {
    index_file(FileKind::Rust, text.as_bytes()).unwrap()
}

/// Each definition of `text` as its path and the text of its span.
fn defined(text: &str) -> Vec<(String, &str)> {
    index(text)
        .definitions
        .iter()
        .map(|d| {
            let path: Vec<String> = d.path.iter().map(ToString::to_string).collect();
            (path.join("::"), &text[d.span.start..d.span.end])
        })
        .collect()
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
fn a_type_macro_keeps_the_words_of_each_arm_that_defines_a_type() {
    let text = "#[macro_export]\nmacro_rules! id {\n    ($(#[$m:meta])* ordered $name:ident) => { pub enum $name { A } impl $name { fn rank() {} } };\n    ($name:ident, $t:ty) => { pub type $name = $t; };\n    (@inner $name:ident) => {};\n    ($vis:vis struct $name:ident) => { $vis struct $name; };\n}\nmacro_rules! call { ($x:ident) => { let _ = $x; } }\n";

    assert_eq!(
        index(text).macros,
        [TypeMacro {
            name: "id".into(),
            exported: true,
            arms: vec![
                TypeArm {
                    words: vec!["ordered".into()],
                    kind: DefinitionKind::Enum,
                },
                TypeArm {
                    words: vec![],
                    kind: DefinitionKind::TypeAlias,
                },
            ],
            members: vec![("rank".into(), DefinitionKind::Method)],
        }]
    );
}

#[test]
fn an_invocation_keeps_its_first_tokens_past_its_attributes() {
    let text = "mod m {\n    ids::id! {\n        /// Doc.\n        #[allow(dead_code)]\n        ordered Rank(u8), \"a b\"\n    }\n}\nfn f() { id! { ordered Local(u8) } }\n";
    let invocations = index(text).invocations;

    assert_eq!(invocations.len(), 1);
    let invocation = &invocations[0];
    assert_eq!(invocation.macro_name, "id");
    assert_eq!(invocation.container, [Segment::name("m")]);
    assert_eq!(
        invocation.tokens,
        ["ordered", "Rank", "(", "u8", ")", ",", "\"a b\""]
    );
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
