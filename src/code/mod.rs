//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; it reads no file and no clock.
//!
//! Rust is read with tree-sitter (`rust.rs`, over `syntax.rs`); TOML,
//! JSON, Makefiles and Markdown with lexical resolvers of their own. A
//! file's index is a function of its kind and bytes ([`index`]); the types
//! macro invocations define need the macros of the whole crate, a second
//! step over the tree's indexes (`expand.rs`). A tree-sitter grammar is
//! one file beside `rust.rs` and one row of [`FileKind`].

mod expand;
mod facts;
mod index;
mod json;
mod make;
mod markdown;
mod rust;
mod span;
mod symbol;
mod syntax;
mod toml;

pub(crate) use expand::MANIFEST;
pub(crate) use facts::{CodeFacts, Entry, FileFault, ReadFault, Resolution};
pub(crate) use index::{FileIndex, FileKind, INDEX_FORMAT, IndexFault, index};
pub(crate) use span::HASH_SCHEME;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// What only a shell may name: the file system, processes, the
    /// environment, the clock, the terminal, and the modules that hold them.
    const SHELL_PATHS: [&str; 13] = [
        "std::fs",
        "std::process",
        "std::env",
        "std::time",
        "std::net",
        "io::Read",
        "io::Write",
        "Timestamp::now",
        "crate::sources",
        "crate::git",
        "crate::console",
        "crate::discovery",
        "crate::commands",
    ];

    #[test]
    fn the_index_names_no_file_no_process_and_no_clock() {
        let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src/code")];
        let mut read = 0;
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let source = fs::read_to_string(&path).unwrap();
                let code = source.split("#[cfg(test)]").next().unwrap();
                for shell in SHELL_PATHS {
                    assert!(!code.contains(shell), "{} names {shell}", path.display());
                }
                read += 1;
            }
        }
        assert!(read > 10, "{read} files read");
    }
}
