//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; it reads no file and no clock.
//!
//! Rust is read with tree-sitter (`rust.rs`, over `syntax.rs`); TOML,
//! JSON, Makefiles and Markdown with lexical resolvers of their own
//! (over `lexical.rs`). A file's index is a function of its kind and bytes
//! ([`index`]): its paths a tree of scopes (`scope.rs`), its work after the
//! parse under a budget (`work.rs`), its cache entry a compact encoding
//! (`encoding.rs`). What needs the whole tree is a second step over the
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
mod work;

pub(crate) use encoding::{
    ENTRY_OVERHEAD_BYTES, Indexed, MAX_ENTRY_BYTES_PER_SOURCE_BYTE, decode as decode_index,
    encode as encode_index,
};
pub(crate) use expand::MANIFEST;
pub(crate) use facts::{CodeFacts, Entry, FileFault, ReadFault, Resolution};
pub(crate) use index::{FileKind, INDEX_FORMAT, index};
pub(crate) use span::HASH_SCHEME;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::testing::{assert_names_no_shell, shell_paths_of};

    #[test]
    fn the_index_names_no_file_no_process_and_no_clock() {
        assert_names_no_shell(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/code"));
    }

    #[test]
    fn a_shell_path_is_found_however_it_is_spelled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("mod.rs"),
            "mod renamed;\nmod called;\nmod listed;\nmod aliased;\n#[cfg(test)]\nmod tests;\nfn ok() { let _ = core::fmt::Error; }\n",
        )
        .unwrap();
        fs::write(dir.join("renamed.rs"), "use std::{fs as disk};\n").unwrap();
        fs::write(
            dir.join("called.rs"),
            "fn f() { let _ = ::std::process::id(); }\n",
        )
        .unwrap();
        fs::write(
            dir.join("listed.rs"),
            "use std::io::{self, Read};\nuse crate::{git, store};\n",
        )
        .unwrap();
        fs::write(
            dir.join("aliased.rs"),
            "use std as s;\nfn g() { Timestamp::now(); }\n",
        )
        .unwrap();
        fs::write(dir.join("tests.rs"), "use std::fs;\n").unwrap();

        assert_eq!(
            shell_paths_of(dir),
            [
                "aliased.rs: std",
                "aliased.rs: Timestamp::now",
                "called.rs: ::std::process::id",
                "listed.rs: std::io::Read",
                "listed.rs: crate::git",
                "renamed.rs: std::fs",
            ]
        );
    }
}
