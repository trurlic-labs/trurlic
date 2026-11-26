//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; at run time it reads no file and no clock.
//!
//! Rust is read with tree-sitter by a module of its own (`parse/rust/`,
//! over `parse/syntax.rs` and the builder every tree-sitter resolver
//! shares, `parse/tree_index.rs`), since its attributes, impls, re-exports
//! and module files need code. Every other language is a grammar and a
//! query file, a row of `parse/language/` read by the general resolver
//! (`parse/general/`, under the contract of `parse/query/`). TOML, JSON,
//! Makefiles and Markdown have lexical resolvers of their own (over
//! `lexical/`). A file's index is a function of its kind and bytes
//! ([`index`]): its paths a tree of scopes (`index/scope_tree.rs`), which
//! charges all an index holds to the file's room, its work under budgets
//! per byte (`index/work.rs`), its cache entry a compact encoding
//! (`index/encoding/`). What needs the whole tree is a second step over
//! the indexes: the files that are test code whole (`tree/`). A macro
//! invocation defines nothing (`parse/rust/`).

mod index;
mod lexical;
mod parse;
mod resolver;
#[cfg(test)]
pub(crate) mod testing;
mod tree;

#[cfg(test)]
pub(crate) use index::SpanHash;
pub(crate) use index::{
    DefinitionKind, FileKind, HASH_SCHEME, Indexed, MAX_SOURCE_BYTES, decode as decode_index,
    encode as encode_index, max_entry_bytes,
};
pub(crate) use parse::{
    BlankMacros, Defines, FileMacros, Lexed, decode_lexed, encode_lexed, is_header, lexed,
    max_lexed_bytes,
};
pub(crate) use resolver::{BOUNDS, INDEX_FORMAT, index};
#[cfg(test)]
pub(crate) use tree::Target;
pub(crate) use tree::{Code, CodeFacts, Entry, FileFault, MANIFEST, ReadFault, Resolution};

/// Whether the query of the language named `name` (`c`, `cpp`, `python`,
/// `typescript`, `tsx`) compiles within its contract, compiled afresh:
/// what a benchmark times.
#[cfg(feature = "bench")]
pub(crate) fn query_compiles(name: &str) -> bool {
    [
        &parse::C,
        &parse::CPP,
        &parse::PYTHON,
        &parse::TYPESCRIPT,
        &parse::TSX,
    ]
    .into_iter()
    .find(|language| language.name == name)
    .is_some_and(|language| language.compile().is_ok())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::purity::{AllowList, faults};

    /// What the code index may name: the store's value type an anchor is,
    /// a few items of `std` that hold no path, its parser, its grammars, its
    /// hash and the TOML reader.
    const CODE: AllowList = AllowList {
        modules: &["code"],
        store_items: &["Anchor"],
        std_items: &[
            ("io", "ErrorKind"),
            ("sync", "Arc"),
            ("sync", "LazyLock"),
            ("sync", "OnceLock"),
        ],
        crates: &[
            "tree_sitter",
            "tree_sitter_rust",
            "tree_sitter_python",
            "tree_sitter_typescript",
            "tree_sitter_c",
            "tree_sitter_cpp",
            "blake3",
            "toml",
        ],
        crate_items: &[],
    };

    #[test]
    fn the_index_names_nothing_but_what_its_allow_list_holds() {
        let found = faults(Path::new(env!("CARGO_MANIFEST_DIR")), "src/code", &CODE);

        assert_eq!(found, Vec::<String>::new());
    }
}
