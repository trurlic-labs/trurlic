//! One level of the code tree as a reader takes it in: the nodes one node
//! holds, what the walked files below each hold summed, the imports
//! between the nodes, and where an anchor's path lies. A level is read in
//! time linear in the nodes and files below its holder and in their
//! files' edges.

use std::collections::BTreeMap;
use std::ops::Bound::{Included, Unbounded};

use super::entry::Entry;
use super::tree::{CodeTree, Node};
use crate::code::index::{Doc, Role};

/// The nodes one node of a code tree holds, with the walked files below
/// them.
#[derive(Debug)]
pub(crate) struct Level<'t, 'i> {
    tree: &'t CodeTree<'i>,
    /// The node whose level this is; `None` at the top.
    holder: Option<Node<'i>>,
    /// In the order [`CodeTree::children`] states.
    pub(crate) nodes: &'t [Node<'i>],
    /// Each walked file below the level, by the place in `nodes` of the
    /// node it is below.
    files: BTreeMap<&'i str, usize>,
}

/// What the walked files below a node hold, summed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Sums {
    /// The [`line_count`](crate::code::index::line_count)s of those whose
    /// bytes were read.
    pub(crate) lines: usize,
    /// Those none of whose bytes were read.
    pub(crate) unread_files: usize,
    /// The definitions the indexes of those with one hold, test code
    /// included.
    pub(crate) definitions: usize,
    /// Those of the definitions that are no test code, by their own mark
    /// or their file's.
    pub(crate) non_test: usize,
    /// Those with no index.
    pub(crate) unindexed_files: usize,
}

/// Where the walked files at a path, or below it as a directory, lie in a
/// level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Covered {
    /// Below the node at this place in the level's nodes, all of them.
    Node(usize),
    /// Below the level's holder, under two of its nodes or more.
    Across,
    /// None is there, or one lies outside the holder.
    Outside,
}

/// The imports between the nodes of one level of the code tree
/// ([`Level::edges`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LevelEdges<'i> {
    /// In the level's order of the node they come from, then of the node
    /// they go to.
    pub(crate) edges: Vec<ImportEdge<'i>>,
    /// Each node of the level under which imports do not resolve, with
    /// how many, re-exports included, in the level's order.
    pub(crate) unresolved_imports: Vec<(Node<'i>, usize)>,
    /// Each node of the level under which dependency imports, re-exports
    /// left out, resolve to files outside the level, with how many, in the
    /// level's order.
    pub(crate) outside_level: Vec<(Node<'i>, usize)>,
}

/// The imports of the files below one node of a level into the files
/// below another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImportEdge<'i> {
    pub(crate) from: Node<'i>,
    pub(crate) to: Node<'i>,
    /// The imports behind it that are dependencies: every import but a
    /// re-export.
    pub(crate) count: usize,
    /// The re-exports behind it, `use`s a visibility opens, which are no
    /// dependency: a view of dependencies draws an edge of these alone as
    /// none.
    pub(crate) reexports: usize,
    /// Whether every import behind it, dependency or re-export, is test
    /// code.
    pub(crate) test_only: bool,
}

/// The imports of the files below a level: the edges between its nodes by
/// their places, and by the place of each node those that do not resolve,
/// re-exports included. Apart, by the place of each node, the dependency
/// imports into files outside the level.
struct Tally {
    edges: BTreeMap<(usize, usize), ImportEdgeTally>,
    unresolved_imports: Vec<usize>,
    outside_level: Vec<usize>,
}

/// What a level counts of the imports behind one edge.
struct ImportEdgeTally {
    count: usize,
    reexports: usize,
    test_only: bool,
}

impl<'i> CodeTree<'i> {
    /// The level below `holder`, the crates for `None`. A file holds no
    /// node: its level is empty.
    pub(crate) fn level(&self, holder: Option<Node<'i>>) -> Level<'_, 'i> {
        let nodes = self.children(holder);
        let mut files = BTreeMap::new();
        for (at, node) in nodes.iter().enumerate() {
            let mut pending = vec![*node];
            while let Some(node) = pending.pop() {
                match node {
                    Node::File { path, .. } => {
                        files.insert(path, at);
                    }
                    Node::Crate(_) | Node::Module { .. } | Node::Directory { .. } => {
                        pending.extend(self.children(Some(node)));
                    }
                }
            }
        }
        Level {
            tree: self,
            holder,
            nodes,
            files,
        }
    }

    /// The first doc paragraph the index holds of `node`: a file's own, a
    /// module's own file's when it has one and no other (targets whose
    /// roots share a directory share a root module), none of any other
    /// node.
    pub(crate) fn doc(&self, node: Node<'i>) -> Option<&'i Doc> {
        let own = match node {
            Node::File { path, .. } => path,
            Node::Module { .. } => {
                let mut files = self
                    .children(Some(node))
                    .iter()
                    .filter_map(|child| match child {
                        Node::File { path, .. } => Some(*path),
                        Node::Crate(_) | Node::Module { .. } | Node::Directory { .. } => None,
                    });
                let own = files.next()?;
                if files.next().is_some() {
                    return None;
                }
                own
            }
            Node::Crate(_) | Node::Directory { .. } => return None,
        };
        match self.facts.entries.get(own)? {
            Entry::File {
                index: Ok(index), ..
            } => index.doc.as_ref(),
            Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed => None,
        }
    }

    /// Add what the walked file at `path` holds to `sums`: no lines and no
    /// index for a path the facts hold no file at.
    fn add_file(&self, path: &str, sums: &mut Sums) {
        let Some(Entry::File { lines, index, .. }) = self.facts.entries.get(path) else {
            sums.unread_files += 1;
            sums.unindexed_files += 1;
            return;
        };
        match lines {
            Some(lines) => sums.lines += lines,
            None => sums.unread_files += 1,
        }
        let Ok(index) = index else {
            sums.unindexed_files += 1;
            return;
        };
        let file_role = self.facts.file_role(path);
        let non_test = index
            .definitions
            .iter()
            .filter(|definition| definition.role.or(file_role) == Role::Code);
        sums.definitions += index.definitions.len();
        sums.non_test += non_test.count();
    }
}

impl<'i> Level<'_, 'i> {
    /// Whether this is the top level, the crates', whose holder holds the
    /// whole tree.
    pub(crate) const fn is_top(&self) -> bool {
        self.holder.is_none()
    }

    /// What the walked files below each node of the level hold, in its
    /// order: the one pass that sums what lies below the nodes.
    pub(crate) fn sums(&self) -> Vec<Sums> {
        let mut sums = vec![Sums::default(); self.nodes.len()];
        for (&file, &at) in &self.files {
            if let Some(sum) = sums.get_mut(at) {
                self.tree.add_file(file, sum);
            }
        }
        sums
    }

    /// Where the walked files at `path`, or below it as a directory, lie.
    pub(crate) fn covered(&self, path: &str) -> Covered {
        let below = format!("{path}/");
        let file = self.tree.places.get_key_value(path).map(|(file, _)| *file);
        let under = self
            .tree
            .places
            .range::<str, _>((Included(below.as_str()), Unbounded));
        let under = under
            .map(|(file, _)| *file)
            .take_while(|file| file.starts_with(&below));
        let mut covered = file.into_iter().chain(under);
        let Some(&first) = covered.next().and_then(|file| self.files.get(file)) else {
            return Covered::Outside;
        };
        let mut across = false;
        for file in covered {
            match self.files.get(file) {
                None => return Covered::Outside,
                Some(&at) => across |= at != first,
            }
        }
        match across {
            true => Covered::Across,
            false => Covered::Node(first),
        }
    }

    /// The imports between the nodes of the level: an edge from one to
    /// another where files below the first import files below the second.
    /// Under each node, how many imports do not resolve, and how many
    /// dependency imports go to files outside the level. An import into a
    /// file below the node it stands under is no edge of the level.
    pub(crate) fn edges(&self) -> LevelEdges<'i> {
        let tally = self.tally();
        let node = |at: usize| self.nodes.get(at).copied();
        let edges = tally.edges.into_iter().filter_map(|((from, to), counted)| {
            Some(ImportEdge {
                from: node(from)?,
                to: node(to)?,
                count: counted.count,
                reexports: counted.reexports,
                test_only: counted.test_only,
            })
        });
        let by_node = |counts: Vec<usize>| -> Vec<(Node<'i>, usize)> {
            let counted = counts
                .into_iter()
                .enumerate()
                .filter(|(_, count)| *count > 0);
            counted
                .filter_map(|(at, count)| Some((node(at)?, count)))
                .collect()
        };
        LevelEdges {
            edges: edges.collect(),
            unresolved_imports: by_node(tally.unresolved_imports),
            outside_level: by_node(tally.outside_level),
        }
    }

    /// The imports of the files below the level ([`Tally`]).
    fn tally(&self) -> Tally {
        let mut tally = Tally {
            edges: BTreeMap::new(),
            unresolved_imports: vec![0; self.nodes.len()],
            outside_level: vec![0; self.nodes.len()],
        };
        for (&file, &from) in &self.files {
            for edge in self.tree.imports.get(file).map_or(&[][..], Vec::as_slice) {
                let (apart, count) = match edge.to.map(|to| self.files.get(to)) {
                    None => (
                        tally.unresolved_imports.get_mut(from),
                        edge.imports + edge.reexports,
                    ),
                    Some(None) => (tally.outside_level.get_mut(from), edge.imports),
                    Some(Some(&to)) if to == from => continue,
                    Some(Some(&to)) => {
                        let counted = tally.edges.entry((from, to)).or_insert(ImportEdgeTally {
                            count: 0,
                            reexports: 0,
                            test_only: true,
                        });
                        counted.count += edge.imports;
                        counted.reexports += edge.reexports;
                        counted.test_only &= edge.test;
                        continue;
                    }
                };
                if let Some(apart) = apart {
                    *apart += count;
                }
            }
        }
        tally
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::code::index::FileKind;
    use crate::code::testing::facts;
    use crate::code::{CodeFacts, FileFault, ReadFault};

    /// Where each of `paths` lies in the level below `holder`, each
    /// spelled `path -> address`, `across` or `-` outside.
    fn held(tree: &CodeTree<'_>, holder: &str, paths: &[&str]) -> Vec<String> {
        let level = tree.level(tree.node(holder));
        let held = paths.iter().map(|path| {
            let lies = match level.covered(path) {
                Covered::Node(at) => tree.address(level.nodes[at]),
                Covered::Across => "across".to_owned(),
                Covered::Outside => "-".to_owned(),
            };
            format!("{path} -> {lies}")
        });
        held.collect()
    }

    #[test]
    fn a_path_lies_in_the_node_below_which_all_its_walked_files_lie() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\n#[path = \"a/x/far.rs\"]\nmod far;\n"),
            ("src/a.rs", "mod x;\n"),
            ("src/a/x.rs", ""),
            ("src/a/x/far.rs", ""),
            ("tools/run.py", ""),
            ("tools/gen/make.py", ""),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);

        assert_eq!(
            held(
                &tree,
                "crate:.",
                &[
                    "src/a.rs",
                    "src",
                    "tools",
                    "tools/gen",
                    "Cargo.toml",
                    "",
                    "src/gone.rs"
                ]
            ),
            [
                "src/a.rs -> module:@src",
                "src -> module:@src",
                "tools -> directory:tools",
                "tools/gen -> directory:tools",
                "Cargo.toml -> file:Cargo.toml",
                " -> -",
                "src/gone.rs -> -",
            ]
        );
        assert_eq!(
            held(
                &tree,
                "module:a@src",
                &["src/a/x.rs", "src/a/x", "src/a", "src/a.rs"]
            ),
            [
                "src/a/x.rs -> module:a::x@src",
                "src/a/x -> -",
                "src/a -> -",
                "src/a.rs -> file:src/a.rs",
            ],
            "a directory whose walked files lie partly outside the level lies outside it"
        );
        assert_eq!(
            held(&tree, "module:@src", &["src/a/x", "src"]),
            ["src/a/x -> module:far@src", "src -> across"]
        );
    }

    #[test]
    fn a_module_s_doc_is_its_own_file_s_when_it_has_one_alone() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "//! The library.\nmod one;\nmod inline { mod deep; }\n",
            ),
            ("src/main.rs", "//! The binary.\n"),
            ("src/one.rs", "//! One module.\n"),
            ("src/inline/deep.rs", "//! Deep.\n"),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let doc = |address: &str| tree.doc(tree.node(address).unwrap()).map(Doc::text);

        let docs = [
            "crate:.",
            "module:@src",
            "module:one@src",
            "module:inline@src",
            "module:inline::deep@src",
            "file:src/lib.rs",
            "file:Cargo.toml",
        ]
        .map(doc);

        assert_eq!(
            docs,
            [
                None,
                None,
                Some("One module."),
                None,
                Some("Deep."),
                Some("The library."),
                None
            ]
        );
    }

    #[test]
    fn a_node_sums_what_each_walked_file_below_it_holds() {
        let deep = format!("{}{}", "mod m {".repeat(70), "}".repeat(70));
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod a;\nmod deep;\nmod big;\n#[cfg(test)]\nmod tests;\n",
            ),
            ("src/a.rs", "pub fn f() {}\n\n#[test]\nfn t() {}\n"),
            ("src/deep.rs", deep.as_str()),
            ("src/tests.rs", "fn helper() {}\n"),
        ];
        let mut entries = facts(&files, &[]).entries;
        let big = crate::code::Entry::File {
            kind: FileKind::Rust,
            lines: None,
            index: Err(FileFault::Read(ReadFault::TooLarge { bytes: 9, max: 8 })),
        };
        entries.insert("src/big.rs".to_owned(), big);
        let walked: BTreeSet<String> = entries.keys().cloned().collect();
        let facts = CodeFacts::new(entries, walked);
        let tree = CodeTree::of(&facts);
        let level = tree.level(tree.node("module:@src"));
        let sum = |lines, unread_files, definitions, non_test, unindexed_files| Sums {
            lines,
            unread_files,
            definitions,
            non_test,
            unindexed_files,
        };

        let sums = level.sums();

        let nodes: Vec<String> = level.nodes.iter().map(|node| tree.address(*node)).collect();
        assert_eq!(
            nodes,
            [
                "module:a@src",
                "module:big@src",
                "module:deep@src",
                "module:tests@src",
                "file:src/lib.rs"
            ]
        );
        assert_eq!(
            sums,
            [
                sum(4, 0, 2, 1, 0),
                sum(0, 1, 0, 0, 1),
                sum(1, 0, 0, 0, 1),
                sum(1, 0, 1, 0, 0),
                sum(5, 0, 4, 3, 0),
            ]
        );
        assert_eq!(
            tree.level(tree.node("crate:.")).sums(),
            [sum(11, 1, 7, 4, 2), sum(1, 0, 1, 1, 0)],
            "a node sums every walked file below it"
        );
    }
}
