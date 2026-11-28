//! One level of the code tree as `map` shows it. Each node one node holds
//! is listed by its address, with the sums of the walked files below it,
//! the decisions anchored in it, what its kind alone has, and its imports.
//! The level also counts the decisions anchored in its holder. It is read
//! from the walk of the calling tree alone: a file outside the walk lies in
//! no node, and no git runs.
//!
//! A decision is anchored in a node when one of its anchors' paths covers
//! a walked file and every walked file it covers is below the node. A file
//! anchor covers its file, a directory anchor the walked files below it.
//! Its symbol is not resolved, as a symbol names a part of its file and
//! never another file. A decision counts once in a node however many of
//! its anchors lie there.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{
    self, CodeFacts, CodeTree, Covered, Crate, DependencyKind, Edge, Endpoint, Module, Node,
    Unresolved,
};
use crate::store::{Decision, Status, Store};

use super::reads::{Reads, Scope, read_tree};

/// One level: the nodes one node holds, and the decisions anchored in
/// that node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Level {
    pub(crate) children: Vec<Child>,
    /// The decisions anchored in the holder; at the top, every decision of
    /// the graph.
    pub(crate) decisions: DecisionCounts,
    /// Those of them anchored in no one child: through directories that
    /// span children, and at the top through no walked file.
    pub(crate) decisions_in_no_child: DecisionCounts,
}

/// A node of a level, as `map` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Child {
    /// Its address ([`CodeTree::address`]).
    pub(crate) address: String,
    pub(crate) kind: ChildKind,
    /// The last segment of its spelled path ([`CodeTree::name`]).
    pub(crate) name: String,
    pub(crate) lines: Lines,
    pub(crate) definitions: Definitions,
    pub(crate) decisions: DecisionCounts,
    /// The imports of the files below it that resolve to no file,
    /// re-exports included.
    pub(crate) unresolved_imports: usize,
    /// A crate's alone.
    pub(crate) crate_facts: Option<CrateFacts>,
    /// A module's alone.
    pub(crate) module: Option<ModuleFacts>,
    /// A file's alone.
    pub(crate) file: Option<FileFacts>,
}

/// What a node of the code tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildKind {
    Crate,
    /// What lies outside every crate, from the code root.
    OutsideCrates,
    Module,
    Directory,
    File,
}

impl ChildKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Crate,
        Self::OutsideCrates,
        Self::Module,
        Self::Directory,
        Self::File,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Crate => "crate",
            Self::OutsideCrates => "outside_crates",
            Self::Module => "module",
            Self::Directory => "directory",
            Self::File => "file",
        }
    }
}

/// What a crate's manifest states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrateFacts {
    pub(crate) manifest: ManifestState,
    /// `None` when its manifest is unread, a virtual workspace's, or names
    /// no package.
    pub(crate) package: Option<String>,
    /// `None` when its manifest is unread.
    pub(crate) dependencies: Option<Dependencies>,
}

/// Whether a crate's manifest has an index, so what it states is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestState {
    Read,
    Unread,
}

impl ManifestState {
    pub(crate) const ALL: [Self; 2] = [Self::Read, Self::Unread];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Unread => "unread",
        }
    }
}

/// The dependencies of a crate on other crates and paths, by the table of
/// its manifest that states them, each in the order of what it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Dependencies {
    pub(crate) normal: Vec<Dependency>,
    pub(crate) dev: Vec<Dependency>,
    pub(crate) build: Vec<Dependency>,
    /// Those a page left out of the tables, every one, when the crate
    /// passes a page with them.
    pub(crate) omitted: usize,
}

/// What a dependency names: its endpoint, and the text that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dependency {
    pub(crate) endpoint: EndpointKind,
    /// A crate's address, an unwalked path as the manifest writes it, or a
    /// workspace dependency's name, as `endpoint` states.
    pub(crate) value: String,
}

/// What a dependency's endpoint is ([`Endpoint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointKind {
    Crate,
    Unwalked,
    NoWorkspaceRoot,
    NotInWorkspace,
}

impl EndpointKind {
    pub(crate) const ALL: [Self; 4] = [
        Self::Crate,
        Self::Unwalked,
        Self::NoWorkspaceRoot,
        Self::NotInWorkspace,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Crate => "crate",
            Self::Unwalked => "unwalked",
            Self::NoWorkspaceRoot => "no_workspace_root",
            Self::NotInWorkspace => "not_in_workspace",
        }
    }
}

/// What a module has of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleFacts {
    pub(crate) doc: Option<Doc>,
    pub(crate) imports: Imports,
}

/// What a file has of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileFacts {
    pub(crate) place: FilePlace,
    pub(crate) doc: Option<Doc>,
    pub(crate) imports: Imports,
}

/// Where a file stands in the tree ([`Module`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePlace {
    InModule,
    NoModule,
    Unreached,
    UnresolvedSeveral,
    UnresolvedInItem,
    UnresolvedTwoFiles,
}

impl FilePlace {
    pub(crate) const ALL: [Self; 6] = [
        Self::InModule,
        Self::NoModule,
        Self::Unreached,
        Self::UnresolvedSeveral,
        Self::UnresolvedInItem,
        Self::UnresolvedTwoFiles,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InModule => "in_module",
            Self::NoModule => "no_module",
            Self::Unreached => "unreached",
            Self::UnresolvedSeveral => "unresolved_several",
            Self::UnresolvedInItem => "unresolved_in_item",
            Self::UnresolvedTwoFiles => "unresolved_two_files",
        }
    }

    const fn of(module: Module<'_>) -> Self {
        match module {
            Module::Rust { .. } => Self::InModule,
            Module::Directory => Self::NoModule,
            Module::Unreached => Self::Unreached,
            Module::Unresolved(Unresolved::Several) => Self::UnresolvedSeveral,
            Module::Unresolved(Unresolved::InItem) => Self::UnresolvedInItem,
            Module::Unresolved(Unresolved::TwoFiles) => Self::UnresolvedTwoFiles,
        }
    }
}

/// The first doc paragraph the index holds of a node ([`CodeTree::doc`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Doc {
    pub(crate) text: String,
    /// Whether the paragraph ran past the bytes the index holds of it.
    pub(crate) cut: bool,
}

/// The imports of the files below a node that resolve to a file: those
/// into the other nodes of the level, listed, and the dependency imports
/// into files outside the level, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Imports {
    /// Into the files below each other node of the level, in the level's
    /// order of the node they go to.
    pub(crate) siblings: Vec<ImportEdge>,
    /// The dependency imports into files outside the level, counted and
    /// not listed; re-exports are left out.
    pub(crate) outside_level: usize,
    /// The edges of `siblings` a page left out, every one, when the node
    /// passes a page with them.
    pub(crate) omitted: usize,
}

/// The imports of the files below a node into the files below another of
/// its level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportEdge {
    /// The other node's address.
    pub(crate) to: String,
    /// The dependency imports: every import but a re-export.
    pub(crate) count: usize,
    /// The re-exports, `use`s a visibility opens, which are no dependency.
    pub(crate) reexports: usize,
    /// Whether every one of them, dependency or re-export, is test code.
    pub(crate) test_only: bool,
}

/// The lines of the walked files below a node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Lines {
    /// Of the files whose bytes were read.
    pub(crate) counted: usize,
    /// The files none of whose bytes were read, whose lines are unknown.
    pub(crate) unread_files: usize,
}

/// The definitions of the walked files below a node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Definitions {
    /// Test code included.
    pub(crate) all: usize,
    pub(crate) non_test: usize,
    /// The files with no index, whose definitions are unknown.
    pub(crate) unindexed_files: usize,
}

/// The decisions anchored in a node, by status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DecisionCounts {
    pub(crate) proposed: usize,
    pub(crate) accepted: usize,
}

impl DecisionCounts {
    fn add(&mut self, status: &Status) {
        match status {
            Status::Proposed { .. } => self.proposed += 1,
            Status::Accepted { .. } => self.accepted += 1,
        }
    }
}

/// An address that names no node of the code tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoNode<'a> {
    pub(crate) address: &'a str,
}

/// The level below the node at the address `node` in the tree at
/// `store`'s code root, the crates for `None`, with the decisions of
/// `decisions`. It reads every source file the walk takes, and fills the
/// index's cache.
pub(crate) fn level<'d, 'a>(
    store: &Store,
    decisions: impl IntoIterator<Item = &'d Decision>,
    node: Option<&'a str>,
) -> Result<Level, NoNode<'a>> {
    let facts = walked(store);
    level_in(&CodeTree::of(&facts), decisions, node)
}

/// The facts of every source file the walk of `store`'s code root takes,
/// the misses of the index's cache written.
pub(crate) fn walked(store: &Store) -> CodeFacts {
    let mut reads = Reads::of_tree(store);
    let root = store.code_root();
    let (entries, walked) = read_tree(store, root, Scope::Tree, std::iter::empty(), &mut reads);
    reads.write(store);
    CodeFacts::new(entries, walked)
}

/// [`level`] in `tree`.
pub(crate) fn level_in<'d, 'a>(
    tree: &CodeTree<'_>,
    decisions: impl IntoIterator<Item = &'d Decision>,
    node: Option<&'a str>,
) -> Result<Level, NoNode<'a>> {
    let holder = match node {
        None => None,
        Some(address) => Some(tree.node(address).ok_or(NoNode { address })?),
    };
    let level = tree.level(holder);
    let mut tallied = Tallied::of(&level, decisions);
    let mut imports = imports(tree, &level);
    let rows = level.nodes.iter().zip(level.sums()).enumerate();
    let children = rows.map(|(at, (&node, sums))| {
        let NodeImports {
            siblings,
            unresolved_imports,
            outside_level,
        } = imports.remove(&node).unwrap_or_default();
        let imports = Imports {
            siblings,
            outside_level,
            omitted: 0,
        };
        Child {
            lines: Lines {
                counted: sums.lines,
                unread_files: sums.unread_files,
            },
            definitions: Definitions {
                all: sums.definitions,
                non_test: sums.non_test,
                unindexed_files: sums.unindexed_files,
            },
            decisions: tallied.by_node.remove(&at).unwrap_or_default(),
            unresolved_imports,
            ..child(tree, node, imports)
        }
    });
    Ok(Level {
        children: children.collect(),
        decisions: tallied.holder,
        decisions_in_no_child: tallied.in_no_child,
    })
}

/// `node` as a child of its level, `imports` its own, with nothing below
/// it counted.
fn child<'i>(tree: &CodeTree<'i>, node: Node<'i>, imports: Imports) -> Child {
    let doc = || {
        tree.doc(node).map(|doc| Doc {
            text: doc.text().to_owned(),
            cut: doc.is_cut(),
        })
    };
    let (kind, crate_facts, module, file) = match node {
        Node::Crate(Some(dir)) => (ChildKind::Crate, Some(crate_facts(tree, dir)), None, None),
        Node::Crate(None) => (ChildKind::OutsideCrates, None, None, None),
        Node::Module { .. } => {
            let module = ModuleFacts {
                doc: doc(),
                imports,
            };
            (ChildKind::Module, None, Some(module), None)
        }
        Node::Directory { .. } => (ChildKind::Directory, None, None, None),
        Node::File { place, .. } => {
            let file = FileFacts {
                place: FilePlace::of(place.module),
                doc: doc(),
                imports,
            };
            (ChildKind::File, None, None, Some(file))
        }
    };
    Child {
        address: tree.address(node),
        kind,
        name: tree.name(node).to_owned(),
        lines: Lines::default(),
        definitions: Definitions::default(),
        decisions: DecisionCounts::default(),
        unresolved_imports: 0,
        crate_facts,
        module,
        file,
    }
}

/// What the manifest of the crate at `dir` states.
fn crate_facts(tree: &CodeTree<'_>, dir: &str) -> CrateFacts {
    match tree.crate_at(dir) {
        Some(Crate::Read { name, edges }) => CrateFacts {
            manifest: ManifestState::Read,
            package: name.map(str::to_owned),
            dependencies: Some(dependencies(tree, edges)),
        },
        // A crate node's directory holds a walked manifest; one with no
        // index states nothing known.
        Some(Crate::Unread) | None => CrateFacts {
            manifest: ManifestState::Unread,
            package: None,
            dependencies: None,
        },
    }
}

/// The dependencies `edges` state, each in its table.
fn dependencies(tree: &CodeTree<'_>, edges: &BTreeSet<Edge<'_>>) -> Dependencies {
    let mut dependencies = Dependencies::default();
    for edge in edges {
        let table = match edge.kind {
            DependencyKind::Normal => &mut dependencies.normal,
            DependencyKind::Dev => &mut dependencies.dev,
            DependencyKind::Build => &mut dependencies.build,
        };
        let (endpoint, value) = match edge.to {
            Endpoint::Crate(dir) => (EndpointKind::Crate, tree.address(Node::Crate(Some(dir)))),
            Endpoint::Unwalked(path) => (EndpointKind::Unwalked, path.to_owned()),
            Endpoint::NoWorkspaceRoot(name) => (EndpointKind::NoWorkspaceRoot, name.to_owned()),
            Endpoint::NotInWorkspace(name) => (EndpointKind::NotInWorkspace, name.to_owned()),
        };
        table.push(Dependency { endpoint, value });
    }
    dependencies
}

/// The decisions of a level by the place of each node they are anchored
/// in, and those anchored in its holder.
struct Tallied {
    by_node: BTreeMap<usize, DecisionCounts>,
    holder: DecisionCounts,
    in_no_child: DecisionCounts,
}

impl Tallied {
    /// The decisions of `decisions` in `level`. At the top every decision
    /// is anchored in the holder.
    fn of<'d>(
        level: &code::Level<'_, '_>,
        decisions: impl IntoIterator<Item = &'d Decision>,
    ) -> Self {
        let mut covered: BTreeMap<&str, Covered> = BTreeMap::new();
        let mut tallied = Self {
            by_node: BTreeMap::new(),
            holder: DecisionCounts::default(),
            in_no_child: DecisionCounts::default(),
        };
        for decision in decisions {
            let (mut nodes, mut in_holder) = (BTreeSet::new(), level.is_top());
            for anchor in &decision.anchors {
                let path = anchor.file();
                match *covered.entry(path).or_insert_with(|| level.covered(path)) {
                    Covered::Node(at) => {
                        nodes.insert(at);
                        in_holder = true;
                    }
                    Covered::Across => in_holder = true,
                    Covered::Outside => {}
                }
            }
            if in_holder {
                tallied.holder.add(&decision.status);
            }
            if in_holder && nodes.is_empty() {
                tallied.in_no_child.add(&decision.status);
            }
            for at in nodes {
                tallied.by_node.entry(at).or_default().add(&decision.status);
            }
        }
        tallied
    }
}

/// The imports of the files below a node of a level.
#[derive(Default)]
struct NodeImports {
    siblings: Vec<ImportEdge>,
    unresolved_imports: usize,
    outside_level: usize,
}

/// The imports of each node of `level`; a node with none is not held.
fn imports<'i>(
    tree: &CodeTree<'i>,
    level: &code::Level<'_, 'i>,
) -> BTreeMap<Node<'i>, NodeImports> {
    let edges = level.edges();
    let mut imports: BTreeMap<Node<'i>, NodeImports> = BTreeMap::new();
    for edge in edges.edges {
        imports
            .entry(edge.from)
            .or_default()
            .siblings
            .push(ImportEdge {
                to: tree.address(edge.to),
                count: edge.count,
                reexports: edge.reexports,
                test_only: edge.test_only,
            });
    }
    for (node, count) in edges.unresolved_imports {
        imports.entry(node).or_default().unresolved_imports = count;
    }
    for (node, count) in edges.outside_level {
        imports.entry(node).or_default().outside_level = count;
    }
    imports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;
    use crate::store::Anchor;
    use crate::store::testing::sample_decision;

    /// A decision of `status` anchored at each of `anchors`, `file` or
    /// `file::symbol`.
    fn decision(status: Status, anchors: &[&str]) -> Decision {
        let mut decision = sample_decision("project");
        decision.status = status;
        decision.anchors = anchors
            .iter()
            .map(|anchor| match anchor.split_once("::") {
                Some((file, symbol)) => Anchor::new(file.into(), Some(symbol.into())),
                None => Anchor::new((*anchor).into(), None),
            })
            .map(Result::unwrap)
            .collect();
        decision
    }

    /// The decisions of the level below `node`, proposed and accepted: its
    /// holder's, then those in no child, then each child's by address.
    fn counted(tree: &CodeTree<'_>, decisions: &[Decision], node: Option<&str>) -> Vec<String> {
        let level = level_in(tree, decisions, node).unwrap();
        let spell = |counts: DecisionCounts| format!("{} {}", counts.proposed, counts.accepted);
        let holder = [
            format!("holder {}", spell(level.decisions)),
            format!("no child {}", spell(level.decisions_in_no_child)),
        ];
        let children = level
            .children
            .iter()
            .map(|child| format!("{} {}", child.address, spell(child.decisions)));
        holder.into_iter().chain(children).collect()
    }

    #[test]
    fn a_decision_counts_once_in_each_node_and_in_the_holder_one_of_its_anchors_lies_in() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\nmod b;\n"),
            ("src/a.rs", "pub fn f() {}\n"),
            ("src/b.rs", ""),
            ("tools/run.py", ""),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let accepted = || Status::Accepted { via: None };
        let proposed = || Status::Proposed {
            accepted_text: None,
        };
        let decisions = [
            decision(accepted(), &["src/a.rs", "src/a.rs::f", "src/b.rs"]),
            decision(proposed(), &["src/a.rs::gone"]),
            decision(proposed(), &["src"]),
            decision(accepted(), &["tools/gone.py", "README.md"]),
            decision(accepted(), &["src/a.rs", "tools/run.py"]),
        ];

        assert_eq!(
            counted(&tree, &decisions, None),
            ["holder 2 3", "no child 0 1", "crate:. 2 2"],
            "at the top every decision is the holder's, one anchored nowhere in no child"
        );
        assert_eq!(
            counted(&tree, &decisions, Some("crate:.")),
            [
                "holder 2 2",
                "no child 0 0",
                "module:@src 2 2",
                "directory:tools 0 1",
                "file:Cargo.toml 0 0"
            ]
        );
        assert_eq!(
            counted(&tree, &decisions, Some("module:@src")),
            [
                "holder 2 2",
                "no child 1 0",
                "module:a@src 1 2",
                "module:b@src 0 1",
                "file:src/lib.rs 0 0"
            ],
            "a symbol that names nothing still names its file; a directory over two nodes is \
             in their holder alone"
        );
    }

    #[test]
    fn an_import_into_a_file_outside_the_level_is_counted_and_a_re_export_apart() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
            ("src/a/mod.rs", "mod x;\n"),
            (
                "src/a/x.rs",
                "pub use crate::b::g;\nfn f() { crate::b::g(); crate::b::g(); }\n",
            ),
            ("src/b.rs", "pub fn g() {}\n"),
            ("src/c.rs", "pub use crate::b::g;\n"),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let imports = |node: &str| -> Vec<(String, Imports)> {
            let level = level_in(&tree, std::iter::empty(), Some(node)).unwrap();
            let modules = level
                .children
                .into_iter()
                .filter_map(|child| Some((child.address, child.module?.imports)));
            modules.collect()
        };
        let to_b = |count, reexports| Imports {
            siblings: vec![ImportEdge {
                to: "module:b@src".to_owned(),
                count,
                reexports,
                test_only: false,
            }],
            ..Imports::default()
        };

        assert_eq!(
            imports("module:@src"),
            [
                ("module:a@src".to_owned(), to_b(2, 1)),
                ("module:b@src".to_owned(), Imports::default()),
                ("module:c@src".to_owned(), to_b(0, 1)),
            ],
            "an edge of re-exports alone is listed with no dependency import"
        );
        assert_eq!(
            imports("module:a@src"),
            [(
                "module:a::x@src".to_owned(),
                Imports {
                    outside_level: 2,
                    ..Imports::default()
                }
            )],
            "a re-export out of the level is no dependency import counted there"
        );
    }

    #[test]
    fn a_child_holds_what_its_kind_alone_has() {
        let files = [
            (
                "Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'x' }\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = 'app'\n[dependencies]\nx = { workspace = true }\n\
                 gone = { path = '../gone' }\n[dev-dependencies]\nnope = { workspace = true }\n",
            ),
            ("app/src/lib.rs", "mod a;\nfn f() { mod local; }\n"),
            ("app/src/a.rs", "//! A.\n"),
            ("app/src/local.rs", ""),
            ("app/src/stray.rs", ""),
            ("x/Cargo.toml", "[package\n"),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let spelled = |node: Option<&str>| -> Vec<String> {
            let level = level_in(&tree, std::iter::empty(), node).unwrap();
            let spelled = level.children.iter().map(|child| {
                let own = match (&child.crate_facts, &child.module, &child.file) {
                    (Some(facts), None, None) => {
                        let tables = facts.dependencies.as_ref().map(|tables| {
                            [&tables.normal, &tables.dev, &tables.build].map(|table| {
                                let named = table.iter().map(|dependency| {
                                    format!("{} {}", dependency.endpoint.as_str(), dependency.value)
                                });
                                named.collect::<Vec<_>>().join(", ")
                            })
                        });
                        format!("{} {:?} {tables:?}", facts.manifest.as_str(), facts.package)
                    }
                    (None, Some(module), None) => format!("doc {:?}", module.doc),
                    (None, None, Some(file)) => file.place.as_str().to_owned(),
                    (None, None, None) => String::new(),
                    own => panic!("{own:?}"),
                };
                format!("{} {}: {own}", child.kind.as_str(), child.name)
            });
            spelled.collect()
        };

        assert_eq!(
            spelled(None),
            [
                "crate .: read None Some([\"\", \"\", \"\"])",
                "crate app: read Some(\"app\") Some([\"crate crate:x, unwalked ../gone\", \
                 \"not_in_workspace nope\", \"\"])",
                "crate x: unread None None",
            ]
        );
        assert_eq!(
            spelled(Some("crate:app")),
            [
                "module src: doc None",
                "directory src: ",
                "file Cargo.toml: no_module"
            ]
        );
        assert_eq!(
            spelled(Some("directory:app/src")),
            [
                "file local.rs: unresolved_in_item",
                "file stray.rs: unreached"
            ]
        );
        assert_eq!(
            spelled(Some("module:@app/src")),
            [
                "module a: doc Some(Doc { text: \"A.\", cut: false })",
                "file lib.rs: in_module"
            ]
        );
    }

    /// What lies below a node that its level's children share out.
    fn shared(children: &[Child]) -> (Lines, Definitions, usize) {
        let mut sum = (Lines::default(), Definitions::default(), 0);
        for child in children {
            sum.0.counted += child.lines.counted;
            sum.0.unread_files += child.lines.unread_files;
            sum.1.all += child.definitions.all;
            sum.1.non_test += child.definitions.non_test;
            sum.1.unindexed_files += child.definitions.unindexed_files;
            sum.2 += child.unresolved_imports;
        }
        sum
    }

    /// Whatever files this repository gains, its root lists the crate
    /// `trurlic`, and each node's lines, definitions and unresolved imports
    /// are those its children share out.
    #[test]
    fn on_this_repository_each_node_holds_what_its_children_share_out() {
        let (facts, _) = crate::sources::testing::this_repository();
        let tree = CodeTree::of(facts);
        let mut pending = level_in(&tree, std::iter::empty(), None).unwrap().children;
        let packages: Vec<Option<&str>> = pending
            .iter()
            .map(|child| {
                let facts = child.crate_facts.as_ref();
                facts.and_then(|facts| facts.package.as_deref())
            })
            .collect();
        assert!(packages.contains(&Some("trurlic")), "{packages:?}");

        let mut levels = 0;
        while let Some(holder) = pending.pop() {
            let level = level_in(&tree, std::iter::empty(), Some(&holder.address)).unwrap();
            if holder.kind == ChildKind::File {
                assert_eq!(level.children, [], "{}", holder.address);
                continue;
            }
            let held = (holder.lines, holder.definitions, holder.unresolved_imports);
            assert_eq!(shared(&level.children), held, "{}", holder.address);
            levels += 1;
            pending.extend(level.children);
        }
        assert!(levels > 100, "{levels}");
    }
}
