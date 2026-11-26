//! The code index: what each source file defines, with spans and span
//! hashes, and what an anchor names in a tree. Pure: its input is a path
//! and bytes, or the facts a shell gathered (`sources`), and its output is
//! values; at run time it reads no file and no clock.
//!
//! Its sub-modules depend one way. `index/` is what an index is: the
//! values every resolver builds and every reader reads, the bounds they
//! are built within, the scope tree that charges all an index holds to
//! its file's room, and its cache entry. The resolvers stand on it:
//! `parse/` reads a file with tree-sitter, Rust by a module of its own and
//! every other language by its row and query file through the general
//! resolver, and `lexical/` reads TOML, JSON, Makefiles and Markdown.
//! `resolver.rs` dispatches a file to its kind's resolver
//! ([`index`](fn@index)), so a file's index is a function of its kind and
//! bytes. `tree/` is the second step over a tree's indexes: what no one
//! file's index states, and what an anchor names. A macro invocation
//! defines nothing (`parse/rust/`).

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

    /// Each sub-module of the index and the modules of the index it may
    /// name: the values below every resolver, the resolvers on them, and
    /// the facts over a tree on the values and the language rows. The
    /// dispatch and the surface, at the root, name them all.
    const LAYERS: [(&str, &[&str]); 4] = [
        ("src/code/index", &["code::index"]),
        ("src/code/lexical", &["code::index", "code::lexical"]),
        ("src/code/parse", &["code::index", "code::parse"]),
        (
            "src/code/tree",
            &["code::index", "code::parse", "code::tree"],
        ),
    ];

    #[test]
    fn each_sub_module_names_no_module_above_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (dir, modules) in LAYERS {
            let found = faults(root, dir, &AllowList { modules, ..CODE });

            assert_eq!(found, Vec::<String>::new(), "{dir}");
        }
    }
}
