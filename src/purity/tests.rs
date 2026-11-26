//! The checks over seeded crates: one file per form a path to the outside
//! or above a part can take, and per form the reading refuses, each
//! asserted found.

use std::collections::BTreeSet;
use std::fs;

use tempfile::TempDir;

use super::pure::REFUSED_NAMES;
use super::{AllowList, Crate};

/// A crate in a temporary directory holding `files`, each (path from
/// the crate's directory, text).
fn seeded(files: &[(String, String)]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for (path, text) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    tmp
}

/// `cases` as files of the module directory `dir`, one per case, and a
/// `mod.rs` that declares them after `head`.
fn case_files(dir: &str, head: &str, cases: &[(&str, &str)]) -> Vec<(String, String)> {
    let declared: String = cases
        .iter()
        .map(|(name, _)| format!("mod {name};\n"))
        .collect();
    let mut files = vec![(format!("{dir}/mod.rs"), format!("{declared}{head}"))];
    files.extend(
        cases
            .iter()
            .map(|(name, text)| (format!("{dir}/{name}.rs"), (*text).to_owned())),
    );
    files
}

/// The files `found` names, each fault's text before its first colon.
fn faulted(found: &[String]) -> BTreeSet<&str> {
    found
        .iter()
        .map(|fault| fault.split(':').next().unwrap())
        .collect()
}

/// The files of `dir` that hold `cases` but those named `clean`.
fn expected(dir: &str, cases: &[(&str, &str)], clean: &str) -> BTreeSet<String> {
    cases
        .iter()
        .filter(|(name, _)| *name != clean)
        .map(|(name, _)| format!("{dir}/{name}.rs"))
        .collect()
}

/// The list the seeded module is checked against: a store value type,
/// the items of `std` its pure file names, a parser crate and the thread
/// pool's prelude.
const SEEDED_LIST: AllowList = AllowList {
    modules: &["index"],
    store_items: &["Anchor"],
    std_items: &[("io", "ErrorKind"), ("sync", "Arc"), ("sync", "LazyLock")],
    crates: &["tree_sitter"],
    crate_items: &[("rayon", "prelude")],
};

/// Each form a path to the outside can take, one per file, and code that
/// is pure or test code, which none of the faults is in.
const SEEDED: &[(&str, &str)] = &[
    (
        "pure",
        "use std::collections::BTreeMap;\nuse std::io::ErrorKind;\nuse crate::store::Anchor;\nuse super::index::FileKind;\nuse tree_sitter::Node;\nfn f() -> core::fmt::Result { Ok(()) }\nconst Q: &str = include_str!(\"queries/q.scm\");\nconst R: &str = include_str!(\"./queries/../q.scm\");\nuse std::sync::{Arc, LazyLock};\nuse rayon::prelude::*;\nmod checks {\n    #![cfg(test)]\n    fn probe() { std::fs::read(\"x\"); }\n}\n#[derive(Deserialize)]\n#[serde(rename = \"type\", try_from = \"String\")]\nstruct Field;\n#[error(\"names `{0}` twice\")]\nstruct Twice;\nmod inner {\n    use super::super::Kind;\n}\n",
    ),
    ("path_type", "fn f(path: &std::path::PathBuf) {}\n"),
    (
        "module_alias",
        "use std::collections as c;\nfn f() { let _ = c::BTreeMap::<u8, u8>::new(); }\n",
    ),
    ("item_alias", "use std::collections::BTreeMap as Map;\n"),
    ("sync_clock", "fn f(wait: &std::sync::Condvar) {}\n"),
    (
        "macro_definition",
        "macro_rules! call { ($v:expr, $m:ident) => { $v.$m() }; }\n",
    ),
    (
        "crate_item",
        "fn f() -> usize { rayon::current_num_threads() }\n",
    ),
    ("crate_item_glob", "use rayon::*;\n"),
    ("fs", "fn f() { let _ = std::fs::read(\"/x\"); }\n"),
    ("listed", "use std::{fs, collections};\n"),
    ("aliased", "use std::fs as disk;\n"),
    ("self_alias", "use std::{self as s};\n"),
    ("glob", "use std::*;\n"),
    ("root_alias", "use std as s;\n"),
    (
        "absolute",
        "fn f() { let _ = ::std::env::var(\"HOME\"); }\n",
    ),
    ("time", "fn f() { let _ = std::time::Instant::now(); }\n"),
    (
        "thread",
        "fn f() { let _ = std::thread::available_parallelism(); }\n",
    ),
    ("io", "fn f() { let _ = std::io::stdin(); }\n"),
    (
        "os",
        "fn f() { let _ = std::os::unix::process::parent_id(); }\n",
    ),
    ("path", "fn f() { let _ = std::path::absolute(\"x\"); }\n"),
    (
        "backtrace",
        "fn f() { let _ = std::backtrace::Backtrace::force_capture(); }\n",
    ),
    (
        "macro_argument",
        "fn f() -> String { format!(\"{:?}\", std::fs::read(\"/x\")) }\n",
    ),
    (
        "macro_body",
        "macro_rules! read { () => { std::net::TcpStream::connect(\"x\") }; }\n",
    ),
    ("dependency", "fn f() { rayon::spawn(|| ()); }\n"),
    ("crate_shell", "fn f() { crate::git::toplevel(); }\n"),
    ("crate_store", "fn f(store: &crate::store::Store) {}\n"),
    ("crate_glob", "use crate::*;\n"),
    (
        "parent_item_named_as_module",
        "fn f() { crate::index(b\"x\"); }\n",
    ),
    ("crate_alias", "use crate as c;\n"),
    (
        "super_reach",
        "fn f() { super::super::console::write(); }\n",
    ),
    (
        "inline_super_reach",
        "mod inner {\n    fn f() { super::super::super::console::write(); }\n}\n",
    ),
    (
        "dollar_crate",
        "macro_rules! m { () => { $crate::sources::gather() }; }\n",
    ),
    (
        "attribute_crate",
        "#[derive(Deserialize)]\nstruct S {\n    #[serde(default = \"crate::git::toplevel\")]\n    root: String,\n}\n",
    ),
    (
        "attribute_std",
        "#[derive(Deserialize)]\n#[serde(try_from = \"std::path::PathBuf\")]\nstruct S;\n",
    ),
    (
        "attribute_raw",
        "#[derive(Deserialize)]\n#[serde(with = r#\"super::super::console\"#)]\nstruct S;\n",
    ),
    ("include", "const X: &str = include_str!(\"../lib.rs\");\n"),
    (
        "include_absolute",
        "const X: &str = include_str!(\"/etc/hosts\");\n",
    ),
    (
        "include_escaping",
        "const X: &str = include_str!(\"queries/../../lib.rs\");\n",
    ),
    (
        "include_computed",
        "const X: &str = include_str!(concat!(\"q\", \".scm\"));\n",
    ),
    ("include_raw", "const X: &str = include_str!(r\"q.scm\");\n"),
    (
        "include_bytes",
        "const X: &[u8] = include_bytes!(\"q.scm\");\n",
    ),
    (
        "environment",
        "const X: Option<&str> = option_env!(\"HOME\");\n",
    ),
    ("print", "fn f() { println!(\"x\"); }\n"),
    ("raw_root", "fn f() { let _ = r#std::fs::read(\"/x\"); }\n"),
    (
        "tool_attribute",
        "#[rustfmt::test]\npub(crate) fn tool() { let _ = std::fs::read(\"/x\"); }\n",
    ),
    (
        "inner_test_module",
        "mod hidden {\n    #![cfg(test)]\n}\npub(crate) fn leak() { let _ = std::fs::read(\"/x\"); }\n",
    ),
];

#[test]
fn each_path_to_the_outside_is_found_however_it_is_spelled() {
    // Each refused name, reached through an allowed module's own name.
    let refused: Vec<(String, String)> = REFUSED_NAMES
        .iter()
        .map(|name| {
            let text = format!(
                "use std::collections;\nfn f() {{ let _ = collections::{name}::default(); }}\n"
            );
            (format!("refused_{}", name.to_lowercase()), text)
        })
        .collect();
    let cases: Vec<(&str, &str)> = SEEDED
        .iter()
        .copied()
        .chain(
            refused
                .iter()
                .map(|(name, text)| (name.as_str(), text.as_str())),
        )
        .collect();
    let mut files = case_files(
        "src/index",
        "#[cfg(test)]\nmod tests;\n#[cfg(test)]\nfn probe() { std::fs::read(\"x\"); }\n",
        &cases,
    );
    files.extend([
        (
            "Cargo.toml".to_owned(),
            "[package]\n[dependencies]\nrayon = \"1\"\n".to_owned(),
        ),
        ("src/lib.rs".to_owned(), "mod index;\n".to_owned()),
        ("src/index/tests.rs".to_owned(), "use std::fs;\n".to_owned()),
    ]);
    let tmp = seeded(&files);

    let found = Crate::read(tmp.path()).faults("index", &SEEDED_LIST);

    let expected = expected("src/index", &cases, "pure");
    assert_eq!(
        faulted(&found),
        expected.iter().map(String::as_str).collect(),
        "{found:#?}"
    );
}

/// Each form a path into the crate above a part can take, one per file
/// of the part `index::high`, which may name `index::low` and `Error`.
const ABOVE: &[(&str, &str)] = &[
    (
        "below",
        "use crate::index::low::Item;\nuse super::super::low;\nuse crate::Error;\nfn f() -> low::Item { super::super::low::Item }\nmod checks {\n    #![cfg(test)]\n    use crate::index::Root;\n}\nmod inner {\n    use super::super::super::low::Item;\n    use super::super::below::f;\n}\n#[serde(with = \"crate::index::low\", try_from = \"Root\")]\nstruct S;\n",
    ),
    ("supers", "fn f() { super::super::Root; }\n"),
    ("supers_listed", "use super::super::{low, Root};\n"),
    ("root_reexport", "use super::super::super::Root;\n"),
    ("crate_root_reexport", "use crate::Root;\n"),
    ("root_glob", "use crate::*;\nfn f() { Root; }\n"),
    ("group", "use crate::{Error, index::Root};\n"),
    (
        "alias",
        "use crate::index as above;\nfn f() { above::Root; }\n",
    ),
    ("self_alias", "use crate::index::{self as above};\n"),
    ("super_alias", "use super::super as above;\n"),
    (
        "dollar_crate",
        "macro_rules! m { () => { $crate::index::Root }; }\n",
    ),
    ("sibling", "fn f() { crate::other::g(); }\n"),
    (
        "inline_supers",
        "mod inner {\n    use super::super::super::Root;\n}\n",
    ),
    (
        "attribute",
        "#[derive(Deserialize)]\n#[serde(try_from = \"crate::other::Raw\")]\nstruct S;\n",
    ),
    (
        "attribute_supers",
        "struct S {\n    #[serde(default = \"super::super::Root::new\")]\n    root: u8,\n}\n",
    ),
];

#[test]
fn each_path_above_a_part_is_found_however_it_is_spelled() {
    let mut files = case_files("src/index/high", "#[cfg(test)]\nmod tests;\n", ABOVE);
    files.extend(
        [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod index;\nmod other;\npub use index::Root;\n",
            ),
            ("src/other.rs", "pub fn g() {}\n"),
            ("src/index/low.rs", "pub struct Item;\n"),
            ("src/index/high/tests.rs", "use crate::other::g;\n"),
            (
                "src/index/mod.rs",
                "pub struct Root;\nmod low;\nmod high;\n",
            ),
        ]
        .map(|(path, text)| (path.to_owned(), text.to_owned())),
    );
    let tmp = seeded(&files);

    let found = Crate::read(tmp.path())
        .places_beyond("index::high", &["index::low", "index::high", "Error"]);

    let expected = expected("src/index/high", ABOVE, "below");
    assert_eq!(
        faulted(&found),
        expected.iter().map(String::as_str).collect(),
        "{found:#?}"
    );
}

/// Each form the reading cannot follow, one per file of the module
/// `index`, and a file that holds none, its macro's textual scope holding
/// only a test module.
const UNREADABLE: &[(&str, &str)] = &[
    (
        "clean",
        "mod before {}\nmacro_rules! m { () => {} }\n#[cfg(test)]\nmod tests {}\n#[error(\"a\\nb \\\"{0}\\\"\")]\nstruct S;\n",
    ),
    (
        "path_attribute",
        "#[path = \"../../outside.rs\"]\nmod outside;\n",
    ),
    (
        "conditional_path",
        "#[cfg_attr(all(), path = \"../outside.rs\")]\nmod outside;\n",
    ),
    (
        "test_file_shipped",
        "#[cfg(test)]\nmod hidden;\n#[cfg_attr(all(), path = \"test_file_shipped/hidden.rs\")]\nmod shipped;\n",
    ),
    ("include", "include!(\"../body.in\");\n"),
    ("macros_used", "#[macro_use]\nmod shipped {}\n"),
    (
        "macros_used_conditional",
        "#[cfg_attr(all(), macro_use)]\nmod shipped {}\n",
    ),
    ("macros_used_inner", "#![macro_use]\n"),
    ("extern", "extern crate std as s;\n"),
    (
        "extern_self",
        "extern crate self as x;\nfn f() { x::other::g(); }\n",
    ),
    (
        "macro_scope",
        "macro_rules! m { () => { $crate::other::g() }; }\nmod inner {\n    fn f() { m!(); }\n}\n",
    ),
    (
        "macro_module",
        "macro_rules! ship { () => { mod shipped; }; }\n",
    ),
    (
        "nested_module",
        "#[cfg(test)]\nmod twin;\n#[cfg(not(test))]\nmod twin {\n    mod leak;\n}\n",
    ),
    ("unparsed", "fn f() { crate::other::g() @@ }\n"),
    (
        "attribute_escape",
        "#[derive(Deserialize)]\n#[serde(with = \"crate\\x3a\\x3aother\")]\nstruct S;\n",
    ),
];

#[test]
fn each_form_the_reading_cannot_follow_is_refused_by_every_check() {
    let mut files = case_files("src/index", "", UNREADABLE);
    files.extend(
        [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod index;\nmod other;\n"),
            ("src/other.rs", "pub fn g() {}\n"),
            ("src/index/test_file_shipped/hidden.rs", "use std::fs;\n"),
            ("src/index/nested_module/twin.rs", ""),
            ("src/index/nested_module/twin/leak.rs", "use std::fs;\n"),
        ]
        .map(|(path, text)| (path.to_owned(), text.to_owned())),
    );
    let tmp = seeded(&files);
    let source = Crate::read(tmp.path());
    let nothing = AllowList {
        modules: &["other"],
        store_items: &[],
        std_items: &[],
        crates: &[],
        crate_items: &[],
    };
    let expected = expected("src/index", UNREADABLE, "clean");

    for found in [
        source.faults("other", &nothing),
        source.places_beyond("other", &["other"]),
    ] {
        assert_eq!(
            faulted(&found),
            expected.iter().map(String::as_str).collect(),
            "{found:#?}"
        );
    }
}
