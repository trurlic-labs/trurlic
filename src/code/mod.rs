//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; at run time it reads no file and no clock.
//!
//! Rust is read with tree-sitter by a module of its own (`rust.rs`, over
//! `syntax.rs` and the builder every tree-sitter resolver shares,
//! `tree.rs`), since its attributes, impls, re-exports and module files
//! need code. Every other language is a grammar and a query file, a row
//! of `language.rs` read by the general resolver (`general.rs`, under the
//! contract of `query.rs`). TOML, JSON, Makefiles and Markdown have
//! lexical resolvers of their own (over `lexical.rs`). A file's index is a
//! function of its kind and bytes ([`index`]): its paths a tree of scopes
//! (`scope.rs`), which charges all an index holds to the file's room, its
//! work under budgets per byte (`work.rs`), its cache entry a compact
//! encoding (`encoding.rs`). What needs the whole tree is a second step
//! over the indexes: the files that are test code whole (`roles.rs`). A
//! macro invocation defines nothing (`rust/invocations.rs`).

mod crates;
mod definitions;
mod encoding;
mod facts;
mod general;
mod index;
mod json;
mod kinds;
mod language;
mod lexical;
mod make;
mod markdown;
#[cfg(test)]
mod purity;
mod query;
mod roles;
mod rust;

mod scope;
mod span;
mod symbol;
mod syntax;
#[cfg(test)]
pub(crate) mod testing;
mod toml;
mod tree;
mod units;
mod work;

pub(crate) use crates::MANIFEST;
pub(crate) use encoding::{
    Indexed, decode as decode_index, encode as encode_index, max_entry_bytes,
};
#[cfg(test)]
pub(crate) use facts::Target;
pub(crate) use facts::{CodeFacts, Entry, FileFault, Namesakes, ReadFault, Resolution};
pub(crate) use index::{
    BOUNDS, FileKind, INDEX_FORMAT, MAX_BYTES_INDEXED_AT_ONCE, MAX_BYTES_INDEXED_BY_A_WORKER,
    MAX_SOURCE_BYTES, PARSE_STACK_BYTES, WORKER_STACK_BYTES, index,
};
pub(crate) use kinds::{DefinitionKind, Role};
pub(crate) use language::c_family::{
    BlankMacros, Defines, FileMacros, Lexed, decode_lexed, encode_lexed, is_header, lexed,
    max_lexed_bytes,
};

/// Whether the query of the language named `name` (`c`, `cpp`, `python`,
/// `typescript`, `tsx`) compiles within its contract, compiled afresh:
/// what a benchmark times.
#[cfg(feature = "bench")]
pub(crate) fn query_compiles(name: &str) -> bool {
    [
        &language::C,
        &language::CPP,
        &language::PYTHON,
        &language::TYPESCRIPT,
        &language::TSX,
    ]
    .into_iter()
    .find(|language| language.name == name)
    .is_some_and(|language| language.compile().is_ok())
}
#[cfg(test)]
pub(crate) use purity::{AllowList, faults};
pub(crate) use span::HASH_SCHEME;
#[cfg(test)]
pub(crate) use span::SpanHash;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::purity::{AllowList, INDEX, faults};

    #[test]
    fn the_index_names_nothing_but_what_its_allow_list_holds() {
        let found = faults(Path::new(env!("CARGO_MANIFEST_DIR")), "src/code", &INDEX);

        assert_eq!(found, Vec::<String>::new());
    }

    /// Each form a path to the outside can take, one per file, and code
    /// that is pure or test code, which none of the faults is in.
    const SEEDED: &[(&str, &str)] = &[
        (
            "pure",
            "use std::collections::BTreeMap;\nuse std::io::ErrorKind;\nuse crate::store::Anchor;\nuse super::index::FileKind;\nuse tree_sitter::Node;\nfn f() -> core::fmt::Result { Ok(()) }\nconst Q: &str = include_str!(\"queries/q.scm\");\nconst R: &str = include_str!(\"./queries/../q.scm\");\nuse std::sync::{Arc, LazyLock};\nuse rayon::prelude::*;\nmod checks {\n    #![cfg(test)]\n    fn probe() { std::fs::read(\"x\"); }\n}\n",
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
        ("crate_alias", "use crate as c;\n"),
        (
            "super_reach",
            "fn f() { super::super::console::write(); }\n",
        ),
        (
            "dollar_crate",
            "macro_rules! m { () => { $crate::sources::gather() }; }\n",
        ),
        ("extern", "extern crate std as s;\n"),
        (
            "path_attribute",
            "#[path = \"../../outside.rs\"]\nmod outside;\n",
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
            "conditional_path",
            "#[cfg_attr(all(), path = \"../outside.rs\")]\nmod outside;\n",
        ),
        (
            "test_file_shipped",
            "#[cfg(test)]\nmod hidden;\n#[cfg_attr(all(), path = \"test_file_shipped/hidden.rs\")]\nmod shipped;\n",
        ),
        (
            "inner_test_module",
            "mod hidden {\n    #![cfg(test)]\n}\npub(crate) fn leak() { let _ = std::fs::read(\"/x\"); }\n",
        ),
        (
            "macro_module",
            "macro_rules! ship { () => { mod shipped; }; }\n",
        ),
    ];

    #[test]
    fn each_path_to_the_outside_is_found_however_it_is_spelled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("Cargo.toml", "[package]\n[dependencies]\nrayon = \"1\"\n");
        write("src/lib.rs", "mod index;\n");
        // Each refused name, reached through an allowed module's own name.
        let refused: Vec<(String, String)> = super::purity::REFUSED_NAMES
            .iter()
            .map(|name| {
                let text = format!(
                    "use std::collections;\nfn f() {{ let _ = collections::{name}::default(); }}\n"
                );
                (format!("refused_{}", name.to_lowercase()), text)
            })
            .collect();
        let files: Vec<(&str, &str)> = SEEDED
            .iter()
            .copied()
            .chain(
                refused
                    .iter()
                    .map(|(name, text)| (name.as_str(), text.as_str())),
            )
            .collect();
        let modules: String = files
            .iter()
            .map(|(name, _)| format!("mod {name};\n"))
            .collect();
        write(
            "src/index/mod.rs",
            &format!(
                "{modules}#[cfg(test)]\nmod tests;\n#[cfg(test)]\nfn probe() {{ std::fs::read(\"x\"); }}\n"
            ),
        );
        write("src/index/tests.rs", "use std::fs;\n");
        write("src/index/test_file_shipped/hidden.rs", "use std::fs;\n");
        for (name, text) in &files {
            write(&format!("src/index/{name}.rs"), text);
        }
        // The index's list, with the thread pool's prelude.
        let listed = AllowList {
            crate_items: &[("rayon", "prelude")],
            ..INDEX
        };
        let found = faults(root, "src/index", &listed);
        let faulted: std::collections::BTreeSet<&str> = found
            .iter()
            .map(|fault| fault.split(':').next().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<String> = files
            .iter()
            .filter(|(name, _)| *name != "pure")
            .map(|(name, _)| format!("src/index/{name}.rs"))
            .collect();

        assert_eq!(
            faulted,
            expected.iter().map(String::as_str).collect(),
            "{found:#?}"
        );
    }
}
