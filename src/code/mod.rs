//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; it reads no file and no clock.
//!
//! Rust is read with tree-sitter (`rust.rs`, over `syntax.rs` and the
//! builder every tree-sitter resolver shares, `tree.rs`); TOML, JSON,
//! Makefiles and Markdown with lexical resolvers of their own (over
//! `lexical.rs`). A file's index is a function of its kind and bytes
//! ([`index`]): its paths a tree of scopes (`scope.rs`), which charges all
//! an index holds to the file's room, its work under budgets per byte
//! (`work.rs`), its cache entry a compact encoding (`encoding.rs`). What needs the whole tree is a second step over the
//! indexes: the types macro invocations define (`expand.rs`) and the files
//! that are test code whole (`roles.rs`). A tree-sitter grammar is one
//! file beside `rust.rs` and one row of [`FileKind`].

mod encoding;
mod expand;
mod facts;
mod index;
mod json;
mod kinds;
mod lexical;
mod make;
mod markdown;
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

pub(crate) use encoding::{
    Indexed, decode as decode_index, encode as encode_index, max_entry_bytes,
};
pub(crate) use expand::MANIFEST;
pub(crate) use facts::{CodeFacts, Entry, FileFault, ReadFault, Resolution};
pub(crate) use index::{FileKind, INDEX_FORMAT, MAX_SOURCE_BYTES, index};
pub(crate) use span::HASH_SCHEME;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::testing::{assert_names_no_shell, shell_names_of};

    #[test]
    fn the_index_names_no_shell_module_of_the_crate() {
        assert_names_no_shell(Path::new(env!("CARGO_MANIFEST_DIR")), "src/code");
    }

    #[test]
    fn a_shell_module_is_found_however_it_is_spelled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("Cargo.toml", "[package]\n");
        write("src/lib.rs", "mod git;\nmod index;\n");
        write(
            "src/index/mod.rs",
            "mod renamed;\nmod called;\nmod expanded;\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\nmod twice;\n#[cfg(not(test))]\n#[path = \"twice.rs\"]\nmod shipped;\nfn ok() { let _ = core::fmt::Error; }\n#[cfg(test)]\nfn probe() { crate::git::run(); }\n",
        );
        write("src/index/renamed.rs", "use crate::{git as g};\n");
        write(
            "src/index/called.rs",
            "fn f() { super::super::console::write(); }\n",
        );
        write(
            "src/index/expanded.rs",
            "fn g() { let _ = format!(\"{:?}\", crate::sources::gather()); }\nmacro_rules! m { () => { crate::mcp::serve() }; }\n",
        );
        write("src/index/tests.rs", "use crate::git;\n");
        write("src/index/twice.rs", "use crate::commands;\n");

        assert_eq!(
            shell_names_of(root, "src/index"),
            [
                "src/index/called.rs:1: console",
                "src/index/expanded.rs:1: sources",
                "src/index/expanded.rs:2: mcp",
                "src/index/renamed.rs:1: git",
                "src/index/twice.rs:1: commands",
            ]
        );
    }
}
