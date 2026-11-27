//! The code tree of the walked files: crate > module > file >
//! definitions, from the code index alone and from nothing an agent
//! wrote. A file an anchor brought from outside the walk is no part of
//! it, so the tree is a function of the walk.

use std::collections::{BTreeMap, BTreeSet};

use super::code_facts::CodeFacts;
use super::crate_graph::{Crate, crates};
use super::crates::{Crates, Files};
use super::entry::Entry;
use super::path::parent;
use super::places::{Module, ModulePath, ModulePaths, Place, places};
use crate::code::index::FileIndex;

/// What stands for the index of a walked file that has none: it holds no
/// declaration to follow and no manifest to read.
static NO_INDEX: FileIndex = FileIndex::EMPTY;

/// The places of the walked files, the module paths they hold, the
/// crates of their manifests, and each level of the tree.
#[derive(Debug)]
pub(crate) struct CodeTree<'i> {
    places: BTreeMap<&'i str, Place<'i>>,
    paths: ModulePaths<'i>,
    crates: BTreeMap<&'i str, Crate<'i>>,
    /// The nodes each node holds, the top's under `None`, in the order
    /// [`CodeTree::children`] states: a level is read in time
    /// proportional to its nodes, however deep the tree runs.
    levels: BTreeMap<Option<Node<'i>>, Vec<Node<'i>>>,
}

/// A node of the code tree. The top holds the crates; a crate holds its
/// Rust root modules, its directories and the files in its own
/// directory; a module holds the modules below it and its own files; a
/// directory holds the directories below it and its files. A file holds
/// its definitions, which its index states (`CodeFacts::definitions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Node<'t> {
    /// A crate by its directory; `None` holds what is outside every crate.
    Crate(Option<&'t str>),
    /// A [`Module::Rust`] of the crate `crate_dir`, which a module path
    /// with a file of its own or a module below it makes a node; an
    /// inline `mod` with neither is a definition of its file alone.
    Module {
        crate_dir: Option<&'t str>,
        root: &'t str,
        path: ModulePath,
    },
    /// A directory below its crate's, or below the code root outside
    /// every crate, that holds a file in no Rust module.
    Directory {
        crate_dir: Option<&'t str>,
        dir: &'t str,
    },
    File(&'t str),
}

impl<'i> CodeTree<'i> {
    /// The tree of `facts`' walked files. One with no index is a file of
    /// the tree with no declaration to follow, and a manifest with none a
    /// crate whose manifest is unread.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the command that prints a level of the code tree builds it"
        )
    )]
    pub(crate) fn of(facts: &'i CodeFacts) -> Self {
        let mut files = Files::new();
        let mut unread = BTreeSet::new();
        let walked = facts
            .entries
            .iter()
            .filter(|(path, _)| facts.walked.contains(*path));
        for (path, entry) in walked {
            let path = path.as_str();
            match entry {
                Entry::File {
                    kind,
                    index: Ok(index),
                } => {
                    files.insert(path, (*kind, index));
                }
                Entry::File {
                    kind,
                    index: Err(_),
                } => {
                    files.insert(path, (*kind, &NO_INDEX));
                    unread.insert(path);
                }
                // The walk takes files of a kind with a resolver alone.
                Entry::Directory | Entry::Unindexed => {}
            }
        }
        let layout = Crates::of(files.keys().copied(), []);
        let mut paths = ModulePaths::default();
        let mut tree = Self {
            places: places(&files, &layout, &mut paths),
            paths,
            crates: crates(&files, &unread),
            levels: BTreeMap::new(),
        };
        tree.levels = tree.levels();
        tree
    }

    /// The place of the walked file at `path`; `None` for any other path.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the imports between Rust files read an importing file's module path"
        )
    )]
    pub(crate) fn place(&self, path: &str) -> Option<&Place<'i>> {
        self.places.get(path)
    }

    /// The segments of `path`, a module path of this tree, outermost
    /// first.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the command that prints a level of the code tree spells its modules"
        )
    )]
    pub(crate) fn module_path(&self, path: ModulePath) -> Vec<&'i str> {
        self.paths.segments(path)
    }

    /// The crate whose manifest is in `dir`; `None` when no walked
    /// manifest is.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the command that prints a level of the code tree prints its crates"
        )
    )]
    pub(crate) fn crate_at(&self, dir: &str) -> Option<&Crate<'i>> {
        self.crates.get(dir)
    }

    /// The nodes `holder` holds; the top's, the crates, for `None`. A
    /// file's are its definitions, which no tree node is: none. A level
    /// is in the order of the kinds of [`Node`] (crates, modules,
    /// directories, files), then of the spelled paths, which siblings of
    /// one kind order by the last segment of their path that differs: a
    /// crate's or a directory's directory, a root module's root directory,
    /// any other module's own name, a file's path.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the command that prints a level of the code tree reads it"
        )
    )]
    pub(crate) fn children(&self, holder: Option<Node<'i>>) -> &[Node<'i>] {
        self.levels.get(&holder).map_or(&[], Vec::as_slice)
    }

    /// Each level of the tree, in the order [`CodeTree::children`]
    /// states. Each node is entered once, from the first file below it.
    fn levels(&self) -> BTreeMap<Option<Node<'i>>, Vec<Node<'i>>> {
        let mut levels: BTreeMap<Option<Node<'i>>, Vec<Node<'i>>> = BTreeMap::new();
        let mut entered = BTreeSet::new();
        for path in self.places.keys() {
            let mut next = Some(Node::File(path));
            while let Some(node) = next.filter(|node| entered.insert(*node)) {
                next = self.holder(&node);
                levels.entry(next).or_default().push(node);
            }
        }
        for level in levels.values_mut() {
            level.sort_by_key(|node| self.order_of(node));
        }
        levels
    }

    /// Where `node` stands among its siblings ([`CodeTree::children`]).
    fn order_of(&self, node: &Node<'i>) -> (u8, Option<&'i str>) {
        match *node {
            Node::Crate(dir) => (0, dir),
            Node::Module { root, path, .. } => (1, Some(self.paths.last(path).unwrap_or(root))),
            Node::Directory { dir, .. } => (2, Some(dir)),
            Node::File(path) => (3, Some(path)),
        }
    }

    /// The node that holds `node`; `None` for a crate, at the top, and
    /// for a file not in the tree.
    fn holder(&self, node: &Node<'i>) -> Option<Node<'i>> {
        match *node {
            Node::Crate(_) => None,
            Node::Module {
                crate_dir,
                root,
                path,
            } => Some(match self.paths.parent(path) {
                None => Node::Crate(crate_dir),
                Some(above) => Node::Module {
                    crate_dir,
                    root,
                    path: above,
                },
            }),
            Node::Directory { crate_dir, dir } => Some(directory(crate_dir, parent(dir))),
            Node::File(path) => {
                let place = self.places.get(path)?;
                Some(match place.module {
                    Module::Rust { root, path } => Node::Module {
                        crate_dir: place.crate_dir,
                        root,
                        path,
                    },
                    Module::Directory | Module::Unreached | Module::Unresolved(_) => {
                        directory(place.crate_dir, parent(path))
                    }
                })
            }
        }
    }
}

/// The node of `dir` in the crate `crate_dir`: the crate at its own
/// directory, or at the code root outside every crate.
fn directory<'t>(crate_dir: Option<&'t str>, dir: &'t str) -> Node<'t> {
    match dir == crate_dir.unwrap_or("") {
        true => Node::Crate(crate_dir),
        false => Node::Directory { crate_dir, dir },
    }
}

/// How the tests spell a place and a node, one line each.
#[cfg(test)]
impl CodeTree<'_> {
    /// The place of the file at `path`: its crate's directory, `.` at the
    /// code root and `-` outside every crate, then its module, a Rust
    /// one's root directory and path, or how the file is in its directory.
    pub(crate) fn spelled_place(&self, path: &str) -> String {
        let place = self.places.get(path).unwrap();
        let dir = |dir: &str| if dir.is_empty() { "." } else { dir }.to_owned();
        let crate_dir = place.crate_dir.map_or_else(|| "-".to_owned(), dir);
        let module = match place.module {
            Module::Rust { root, path } => {
                format!("{} > {}", dir(root), self.module_path(path).join("::"))
            }
            Module::Directory => "directory".to_owned(),
            Module::Unreached => "unreached".to_owned(),
            Module::Unresolved(why) => format!("{why:?}"),
        };
        format!("{crate_dir} {module}")
    }

    /// The crate whose manifest is in `dir`: the directory, then its name
    /// and edges, or `unread`.
    pub(crate) fn spelled_crate(&self, dir: &str) -> String {
        match self.crates.get(dir).unwrap() {
            Crate::Read { name, edges } => {
                let edges: Vec<String> = edges
                    .iter()
                    .map(|edge| format!("{:?} {:?}", edge.kind, edge.to))
                    .collect();
                format!("{dir} {name:?}: {}", edges.join(", "))
            }
            Crate::Unread => format!("{dir} unread"),
        }
    }

    /// `node`: a crate by its directory, a module by its root and path, a
    /// directory or a file by its path.
    pub(crate) fn spelled_node(&self, node: &Node<'_>) -> String {
        match *node {
            Node::Crate(dir) => format!("crate {}", dir.unwrap_or("-")),
            Node::Module { root, path, .. } => {
                format!("module {root} > {}", self.module_path(path).join("::"))
            }
            Node::Directory { dir, .. } => format!("directory {dir}"),
            Node::File(path) => format!("file {path}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::{FileKind, IndexFault};
    use crate::code::testing::facts;

    /// A crate at `app/` with a target of each kind, and files outside
    /// every crate.
    const APP: &[(&str, &str)] = &[
        ("app/Cargo.toml", "[package]\nname = 'app'\n"),
        ("app/build.rs", ""),
        (
            "app/src/lib.rs",
            "mod one;\npub mod dir;\nmod inline { mod deep; }\nmod shared;\nfn f() { mod local; }\n",
        ),
        ("app/src/main.rs", "mod shared;\nmod cli;\n"),
        ("app/src/one.rs", ""),
        ("app/src/dir/mod.rs", "mod leaf;\n"),
        ("app/src/dir/leaf.rs", ""),
        ("app/src/inline/deep.rs", ""),
        ("app/src/local.rs", ""),
        ("app/src/shared.rs", ""),
        ("app/src/cli.rs", ""),
        ("app/src/stray.rs", ""),
        ("app/src/bin/tool.rs", "mod helper;\n"),
        ("app/src/bin/helper.rs", ""),
        ("app/src/bin/big/main.rs", "mod part;\n"),
        ("app/src/bin/big/part.rs", ""),
        ("app/tests/flow.rs", "mod common;\n"),
        ("app/tests/other.rs", "mod common;\n"),
        ("app/tests/common/mod.rs", ""),
        ("app/benches/speed.rs", ""),
        ("app/examples/demo.rs", ""),
        ("app/tools/run.py", ""),
        ("scripts/run.py", ""),
        ("scripts/gen.rs", ""),
    ];

    /// The place of each walked file of `files`.
    fn places_of(files: &[(&str, &str)]) -> Vec<String> {
        let facts = facts(files, &[]);
        let tree = CodeTree::of(&facts);
        files
            .iter()
            .map(|(path, _)| format!("{path}: {}", tree.spelled_place(path)))
            .collect()
    }

    #[test]
    fn a_rust_file_is_at_its_module_path_from_its_targets_root_directory() {
        assert_eq!(
            places_of(APP),
            [
                "app/Cargo.toml: app directory",
                "app/build.rs: app app > ",
                "app/src/lib.rs: app app/src > ",
                "app/src/main.rs: app app/src > ",
                "app/src/one.rs: app app/src > one",
                "app/src/dir/mod.rs: app app/src > dir",
                "app/src/dir/leaf.rs: app app/src > dir::leaf",
                "app/src/inline/deep.rs: app app/src > inline::deep",
                "app/src/local.rs: app InItem",
                "app/src/shared.rs: app app/src > shared",
                "app/src/cli.rs: app app/src > cli",
                "app/src/stray.rs: app unreached",
                "app/src/bin/tool.rs: app app/src/bin > ",
                "app/src/bin/helper.rs: app Several",
                "app/src/bin/big/main.rs: app app/src/bin/big > ",
                "app/src/bin/big/part.rs: app app/src/bin/big > part",
                "app/tests/flow.rs: app app/tests > ",
                "app/tests/other.rs: app app/tests > ",
                "app/tests/common/mod.rs: app app/tests > common",
                "app/benches/speed.rs: app app/benches > ",
                "app/examples/demo.rs: app app/examples > ",
                "app/tools/run.py: app directory",
                "scripts/run.py: - directory",
                "scripts/gen.rs: - unreached",
            ]
        );
    }

    #[test]
    fn a_file_reached_at_two_modules_is_at_several_and_so_is_what_it_declares() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\n#[path = \"a.rs\"]\nmod b;\n"),
            ("src/a.rs", "mod c;\n"),
            ("src/a/c.rs", ""),
        ];

        assert_eq!(
            places_of(&files),
            [
                "Cargo.toml: . directory",
                "src/lib.rs: . src > ",
                "src/a.rs: . Several",
                "src/a/c.rs: . Several",
            ]
        );
    }

    #[test]
    fn a_file_a_mod_in_a_function_declares_is_in_no_module_of_a_path() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod local;\nfn f() {\n    #[path = \"other.rs\"]\n    mod local;\n}\n",
            ),
            ("src/local.rs", ""),
            ("src/other.rs", ""),
        ];

        assert_eq!(
            places_of(&files)[2..],
            ["src/local.rs: . src > local", "src/other.rs: . InItem"]
        );
    }

    #[test]
    fn a_module_with_both_its_files_is_at_no_module_in_either() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod foo;\n"),
            ("src/foo.rs", "mod bar;\n"),
            ("src/foo/mod.rs", ""),
            ("src/foo/bar.rs", ""),
        ];

        assert_eq!(
            places_of(&files)[2..],
            [
                "src/foo.rs: . TwoFiles",
                "src/foo/mod.rs: . TwoFiles",
                "src/foo/bar.rs: . TwoFiles",
            ]
        );
    }

    #[test]
    fn a_file_reached_at_several_modules_is_followed_once_more_however_many_paths_reach_it() {
        // Each file declares the next twice: 2^n paths reach the last.
        let levels = 5_000;
        let texts: Vec<String> = (0..=levels)
            .map(|n| {
                let next = format!("m{}.rs", n + 1);
                format!("#[path = \"{next}\"]\nmod x;\n#[path = \"{next}\"]\nmod y;\n")
            })
            .collect();
        let names: Vec<String> = (0..=levels).map(|n| format!("src/m{n}.rs")).collect();
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "#[path = \"m0.rs\"]\nmod x;\n"),
        ];
        files.extend(
            names
                .iter()
                .map(String::as_str)
                .zip(texts.iter().map(String::as_str)),
        );
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);

        assert_eq!(tree.spelled_place("src/m0.rs"), ". src > x");
        for name in &names[1..] {
            assert_eq!(tree.spelled_place(name), ". Several", "{name}");
        }
    }

    #[test]
    fn a_chain_of_modules_one_in_each_file_is_placed_and_leveled_in_time_linear_in_its_length() {
        // Flat files, each declaring the next: a path one segment deeper
        // in each, which a copy of the path per file made quadratic.
        let count = 16_000;
        let texts: Vec<String> = (0..count)
            .map(|n| format!("#[path = \"m{}.rs\"]\nmod m;\n", n + 1))
            .collect();
        let names: Vec<String> = (0..count).map(|n| format!("src/m{n}.rs")).collect();
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "#[path = \"m0.rs\"]\nmod m;\n"),
        ];
        files.extend(
            names
                .iter()
                .map(String::as_str)
                .zip(texts.iter().map(String::as_str)),
        );
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let last = tree.place(&names[count - 1]).unwrap();
        let Module::Rust { root, path } = last.module else {
            panic!("{last:?}");
        };
        let holder = Node::Module {
            crate_dir: last.crate_dir,
            root,
            path,
        };

        assert_eq!(tree.module_path(path).len(), count);
        assert_eq!(tree.children(Some(holder)), [Node::File(&names[count - 1])]);
    }

    #[test]
    fn a_walked_file_with_no_index_is_at_its_place() {
        let deep = format!("{}{}", "mod m {".repeat(70), "}".repeat(70));
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod deep;\n"),
            ("src/deep.rs", deep.as_str()),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);

        assert_eq!(
            crate::code::testing::index(FileKind::Rust, deep.as_bytes()),
            Err(IndexFault::TooDeep)
        );
        assert_eq!(tree.spelled_place("src/deep.rs"), ". src > deep");
    }

    #[test]
    fn a_file_an_anchor_brought_is_in_no_place() {
        let brought = crate::code::Entry::File {
            kind: FileKind::Rust,
            index: crate::code::testing::index(FileKind::Rust, b"")
                .map_err(crate::code::FileFault::Index),
        };
        let facts = facts(
            &[("Cargo.toml", "[package]\n")],
            &[("target/gen.rs", brought)],
        );

        assert_eq!(CodeTree::of(&facts).place("target/gen.rs"), None);
    }

    #[test]
    fn an_unread_manifest_keeps_its_crate_for_the_files_below_it() {
        let files = [
            ("Cargo.toml", "[workspace]\n"),
            ("inner/Cargo.toml", "[package\n"),
            ("inner/src/lib.rs", "mod a;\n"),
            ("inner/src/a.rs", ""),
        ];

        assert_eq!(
            places_of(&files)[2..],
            [
                "inner/src/lib.rs: inner inner/src > ",
                "inner/src/a.rs: inner inner/src > a",
            ]
        );
    }

    #[test]
    fn a_node_holds_the_nodes_one_level_below_it() {
        let facts = facts(APP, &[]);
        let tree = CodeTree::of(&facts);

        assert_eq!(level(&tree, ""), ["crate -", "crate app"]);
        assert_eq!(
            level(&tree, "crate app"),
            [
                "module app > ",
                "module app/benches > ",
                "module app/examples > ",
                "module app/src > ",
                "module app/src/bin > ",
                "module app/src/bin/big > ",
                "module app/tests > ",
                "directory app/src",
                "directory app/tools",
                "file app/Cargo.toml",
            ]
        );
        assert_eq!(
            level(&tree, "module app/src > "),
            [
                "module app/src > cli",
                "module app/src > dir",
                "module app/src > inline",
                "module app/src > one",
                "module app/src > shared",
                "file app/src/lib.rs",
                "file app/src/main.rs",
            ]
        );
        assert_eq!(
            level(&tree, "module app/src > inline"),
            ["module app/src > inline::deep"],
            "an inline module with a file below it is a node with no file of its own"
        );
        assert_eq!(
            level(&tree, "module app/src > dir"),
            ["module app/src > dir::leaf", "file app/src/dir/mod.rs"]
        );
        assert_eq!(level(&tree, "crate -"), ["directory scripts"]);
        assert!(level(&tree, "file app/src/lib.rs").is_empty());
    }

    #[test]
    fn a_level_is_in_the_order_of_its_spelled_paths_whatever_the_walk_met_first() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\n"),
            ("src/main.rs", "mod b;\n"),
            ("src/a.rs", ""),
            ("src/b.rs", ""),
        ];
        let facts = facts(&files, &[]);

        assert_eq!(
            level(&CodeTree::of(&facts), "module src > "),
            [
                "module src > a",
                "module src > b",
                "file src/lib.rs",
                "file src/main.rs",
            ]
        );
    }

    /// The level below the node spelled `spelled`, the top's for `""`,
    /// each node spelled.
    fn level(tree: &CodeTree<'_>, spelled: &str) -> Vec<String> {
        let holder = (!spelled.is_empty()).then(|| {
            let mut nodes = tree.levels.values().flatten();
            *nodes
                .find(|node| tree.spelled_node(node) == spelled)
                .unwrap()
        });
        let level = tree.children(holder).iter();
        level.map(|node| tree.spelled_node(node)).collect()
    }
}
