//! How a node of the code tree is spelled, and read back: one spelling a
//! node, which a reader hands back to ask for the nodes it holds.
//!
//! A crate is `crate:<directory>`, `.` for the code root, and what lies
//! outside every crate is `outside_crates`. A module is
//! `module:<path>@<root>`. Its path's segments come first, joined by `::`,
//! none for a root module. The directory of its target's root file
//! follows, `.` for the code root. No segment holds an `@`, as none is
//! spelled outside Rust's identifiers. A directory is `directory:<path>`
//! and a file `file:<path>`.
//! Only these spellings read back, so two addresses of one node never
//! differ.

use std::ops::Bound::{Included, Unbounded};

use super::path::{ancestors, file_name};
use super::places::{Module, ModulePath};
use super::tree::{CodeTree, Node};

const CRATE: &str = "crate:";
const OUTSIDE_CRATES: &str = "outside_crates";
const MODULE: &str = "module:";
const DIRECTORY: &str = "directory:";
const FILE: &str = "file:";

/// How an address spells the code root's directory.
const CODE_ROOT: &str = ".";

impl<'i> CodeTree<'i> {
    /// The address of `node`, a node of this tree.
    pub(crate) fn address(&self, node: Node<'i>) -> String {
        match node {
            Node::Crate(Some(dir)) => format!("{CRATE}{}", spelled(dir)),
            Node::Crate(None) => OUTSIDE_CRATES.to_owned(),
            Node::Module { root, path, .. } => {
                let segments = self.module_path(path).join("::");
                format!("{MODULE}{segments}@{}", spelled(root))
            }
            Node::Directory { dir, .. } => format!("{DIRECTORY}{dir}"),
            Node::File { path, .. } => format!("{FILE}{path}"),
        }
    }

    /// The name a level lists `node` by: the last segment of its spelled
    /// path, `outside_crates` for what lies outside every crate. A module's
    /// is its own segment, a root module's its root directory's last. The
    /// address holds the whole path.
    pub(crate) fn name(&self, node: Node<'i>) -> &'i str {
        let last = |dir: &'i str| file_name(spelled(dir));
        match node {
            Node::Crate(dir) => dir.map_or(OUTSIDE_CRATES, last),
            Node::Module { root, path, .. } => self.paths.last(path).unwrap_or_else(|| last(root)),
            Node::Directory { dir, .. } => file_name(dir),
            Node::File { path, .. } => file_name(path),
        }
    }

    /// The node of this tree at `address`; `None` when it names none or
    /// is not an address.
    pub(crate) fn node(&self, address: &str) -> Option<Node<'i>> {
        let node = if address == OUTSIDE_CRATES {
            Node::Crate(None)
        } else if let Some(dir) = address.strip_prefix(CRATE) {
            let (&dir, _) = self.crates.get_key_value(unspelled(dir)?)?;
            Node::Crate(Some(dir))
        } else if let Some(module) = address.strip_prefix(MODULE) {
            self.module(module)?
        } else if let Some(dir) = address.strip_prefix(DIRECTORY) {
            self.directory(dir)?
        } else {
            let (&path, &place) = self.places.get_key_value(address.strip_prefix(FILE)?)?;
            return Some(Node::File { path, place });
        };
        self.levels.contains_key(&Some(node)).then_some(node)
    }

    /// The module `module`, an address past its `module:`, spells.
    fn module(&self, module: &str) -> Option<Node<'i>> {
        let (segments, root) = module.split_once('@')?;
        let root = unspelled(root)?;
        let (crate_dir, root) = self.places.values().find_map(|place| match place.module {
            Module::Rust { root: at, .. } => (at == root).then_some((place.crate_dir, at)),
            Module::Directory | Module::Unreached | Module::Unresolved(_) => None,
        })?;
        let path = match segments {
            "" => ModulePath::ROOT,
            segments => segments
                .split("::")
                .try_fold(ModulePath::ROOT, |path, segment| {
                    self.paths.find(path, segment)
                })?,
        };
        Some(Node::Module {
            crate_dir,
            root,
            path,
        })
    }

    /// The directory node at `dir`, in the crate of the nearest manifest
    /// at or above it, as the files in it are.
    fn directory(&self, dir: &str) -> Option<Node<'i>> {
        let below = format!("{dir}/");
        let (file, _) = self
            .places
            .range::<str, _>((Included(below.as_str()), Unbounded))
            .next()?;
        if !file.starts_with(&below) {
            return None;
        }
        let dir = file.get(..dir.len())?;
        let mut above = std::iter::once(dir).chain(ancestors(dir));
        let crate_dir = above.find_map(|at| Some(*self.crates.get_key_value(at)?.0));
        Some(Node::Directory { crate_dir, dir })
    }
}

/// `dir` as an address spells it.
fn spelled(dir: &str) -> &str {
    match dir {
        "" => CODE_ROOT,
        dir => dir,
    }
}

/// The directory `dir`, as an address spells it, is; `None` for the empty
/// text, which spells none.
fn unspelled(dir: &str) -> Option<&str> {
    match dir {
        "" => None,
        CODE_ROOT => Some(""),
        dir => Some(dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;

    /// A crate with a directory module, an inline module with a file below
    /// it and a directory of no module, a crate whose directory holds an
    /// `@`, and files outside every crate.
    const TREE: &[(&str, &str)] = &[
        ("app/Cargo.toml", "[package]\nname = 'app'\n"),
        ("app/src/lib.rs", "mod a;\nmod inline { mod deep; }\n"),
        ("app/src/a/mod.rs", "mod b;\n"),
        ("app/src/a/b.rs", ""),
        ("app/src/inline/deep.rs", ""),
        ("app/tools/run.py", ""),
        ("at@dir/Cargo.toml", "[package]\n"),
        ("at@dir/src/main.rs", "mod m;\n"),
        ("at@dir/src/m.rs", ""),
        ("scripts/gen/run.py", ""),
    ];

    #[test]
    fn every_node_is_spelled_once_and_reads_back_from_its_address() {
        let facts = facts(TREE, &[]);
        let tree = CodeTree::of(&facts);
        let nodes = tree.levels.values().flatten().copied();

        let addresses: Vec<String> = nodes
            .map(|node| {
                let address = tree.address(node);
                assert_eq!(tree.node(&address), Some(node), "{address}");
                address
            })
            .collect();

        let mut sorted = addresses.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            [
                "crate:app",
                "crate:at@dir",
                "directory:app/tools",
                "directory:scripts",
                "directory:scripts/gen",
                "file:app/Cargo.toml",
                "file:app/src/a/b.rs",
                "file:app/src/a/mod.rs",
                "file:app/src/inline/deep.rs",
                "file:app/src/lib.rs",
                "file:app/tools/run.py",
                "file:at@dir/Cargo.toml",
                "file:at@dir/src/m.rs",
                "file:at@dir/src/main.rs",
                "file:scripts/gen/run.py",
                "module:@app/src",
                "module:@at@dir/src",
                "module:a::b@app/src",
                "module:a@app/src",
                "module:inline::deep@app/src",
                "module:inline@app/src",
                "module:m@at@dir/src",
                "outside_crates",
            ]
        );
    }

    #[test]
    fn an_address_of_no_node_or_spelled_another_way_reads_back_as_none() {
        let facts = facts(TREE, &[]);
        let tree = CodeTree::of(&facts);

        for address in [
            "",
            "app",
            "crate:",
            "crate:.",
            "crate:app/",
            "crate:app/src",
            "outside_crates:",
            "module:",
            "module:@",
            "module:@.",
            "module:a",
            "module:a@",
            "module:a::@app/src",
            "module:::a@app/src",
            "module:b@app/src",
            "module:a::b@app/src/",
            "module:@app",
            "directory:",
            "directory:app/src",
            "directory:app",
            "directory:scripts/",
            "file:",
            "file:app/src",
            "file:app/src/gone.rs",
            "app/src/lib.rs",
        ] {
            assert_eq!(tree.node(address), None, "{address}");
        }
    }

    #[test]
    fn what_lies_outside_every_crate_is_named_outside_crates_and_listed_first() {
        let facts = facts(TREE, &[]);
        let tree = CodeTree::of(&facts);

        let top: Vec<&str> = tree
            .children(None)
            .iter()
            .map(|node| tree.name(*node))
            .collect();

        assert_eq!(top, ["outside_crates", "app", "at@dir"]);
    }

    #[test]
    fn the_code_root_is_spelled_as_a_dot_where_a_directory_is_named() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("build.rs", "mod gen;\n"),
            ("gen.rs", ""),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let crate_root = Node::Crate(Some(""));
        let build = tree.children(Some(crate_root))[0];

        assert_eq!(
            [tree.address(crate_root), tree.address(build)],
            ["crate:.", "module:@."]
        );
        assert_eq!([tree.name(crate_root), tree.name(build)], [".", "."]);
        assert_eq!(tree.node("crate:."), Some(crate_root));
        assert_eq!(
            tree.node("module:gen@."),
            tree.children(Some(build)).first().copied()
        );
    }
}
