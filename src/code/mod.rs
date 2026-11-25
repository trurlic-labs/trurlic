//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; it reads no file and no clock.
//!
//! Rust is read with tree-sitter by a module of its own (`rust.rs`, over
//! `syntax.rs` and the builder every tree-sitter resolver shares,
//! `tree.rs`), since its macros need code. Every other language is a
//! grammar and a query file, a row of `language.rs` read by the general
//! resolver (`general.rs`, under the contract of `query.rs`). TOML, JSON,
//! Makefiles and Markdown have lexical resolvers of their own (over
//! `lexical.rs`). A file's index is a function of its kind and bytes
//! ([`index`]): its paths a tree of scopes (`scope.rs`), which charges all
//! an index holds to the file's room, its work under budgets per byte
//! (`work.rs`), its cache entry a compact encoding (`encoding.rs`). What
//! needs the whole tree is a second step over the indexes: the types
//! macro invocations define (`expand.rs`) and the files that are test
//! code whole (`roles.rs`).

mod crates;
mod definitions;
mod encoding;
mod expand;
mod facts;
mod general;
mod indentation;
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
mod work;

pub(crate) use crates::MANIFEST;
pub(crate) use encoding::{
    Indexed, decode as decode_index, encode as encode_index, max_entry_bytes,
};
pub(crate) use facts::{CodeFacts, Entry, FileFault, ReadFault, Resolution};
pub(crate) use index::{BOUNDS, FileKind, INDEX_FORMAT, MAX_SOURCE_BYTES, index};
pub(crate) use span::HASH_SCHEME;
#[cfg(test)]
pub(crate) use {facts::Target, span::SpanHash};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::purity::faults;

    #[test]
    fn the_index_names_nothing_but_what_its_allow_list_holds() {
        let found = faults(Path::new(env!("CARGO_MANIFEST_DIR")), "src/code");

        assert_eq!(found, Vec::<String>::new());
    }

    /// Each form a path to the outside can take, one per file, and code
    /// that is pure or test code, which none of the faults is in.
    const SEEDED: &[(&str, &str)] = &[
        (
            "pure",
            "use std::collections::BTreeMap;\nuse std::io::ErrorKind;\nuse crate::store::Anchor;\nuse super::index::FileKind;\nuse tree_sitter::Node;\nfn f() -> core::fmt::Result { Ok(()) }\nconst Q: &str = include_str!(\"queries/q.scm\");\nconst R: &str = include_str!(\"./queries/../q.scm\");\n",
        ),
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
        let modules: String = SEEDED
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
        for (name, text) in SEEDED {
            write(&format!("src/index/{name}.rs"), text);
        }
        let found = faults(root, "src/index");
        let files: std::collections::BTreeSet<&str> = found
            .iter()
            .map(|fault| fault.split(':').next().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<String> = SEEDED
            .iter()
            .filter(|(name, _)| *name != "pure")
            .map(|(name, _)| format!("src/index/{name}.rs"))
            .collect();

        assert_eq!(
            files,
            expected.iter().map(String::as_str).collect(),
            "{found:#?}"
        );
    }
}
