//! The crates of a tree and their edges, read from the walked manifests
//! (`Cargo.toml`) alone: no other language's manifest, and none an anchor
//! brought from outside the walk.
//!
//! A crate is the directory of each walked manifest, read or not, named
//! by its `package.name`. Its edges are its path and workspace
//! dependencies ([`Manifest`]), each once a kind. A path is read from the
//! manifest's directory. A `workspace = true` dependency is read through
//! its workspace root, found as cargo finds it. It is the directory the
//! crate's `package.workspace` names, taken as named. Else the search goes
//! up from the crate's directory to the first walked manifest that has a
//! `[workspace]` whose `exclude` does not cover the crate, which is the
//! root, or that is a package whose `package.workspace` names one, taken
//! as named. An `exclude` entry, a path from the root with no glob, covers
//! its directory and every directory below it; one with a `..` segment
//! covers nothing, as cargo compares the joined path as written. `members`
//! is not read, so a crate both name is skipped where cargo keeps it. A
//! manifest that cannot be read ends the search, as what it states is
//! unknown. The root's
//! `[workspace.dependencies]` entry of the dependency's name gives a path
//! from the root; an entry with no path (a version, a git repository) is
//! no edge, as a registry dependency is none. What a path names is a crate
//! when a walked manifest is in its directory, and stated as unwalked when
//! none is.

use std::collections::{BTreeMap, BTreeSet};

use super::crates::{Files, MANIFEST};
use super::path::{ancestors, file_name, parent, resolve};
use crate::code::index::{DependencyKind, DependencySource, Manifest, Package};

/// A crate of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Crate<'i> {
    /// Its manifest has an index.
    Read {
        /// `None` for a virtual workspace, a package whose name is missing
        /// or no string, or a manifest with neither a `[package]` nor a
        /// `[workspace]`.
        name: Option<&'i str>,
        edges: BTreeSet<Edge<'i>>,
    },
    /// Its manifest has no index (a syntax error mid-edit, a file past
    /// its size): its name and edges are unknown.
    Unread,
}

/// A dependency of a crate on another: one per kind and endpoint, a
/// dev-dependency and a normal one on one crate being two.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Edge<'i> {
    pub(crate) kind: DependencyKind,
    pub(crate) to: Endpoint<'i>,
}

/// What a dependency names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Endpoint<'i> {
    /// The crate of the walked manifest in this directory.
    Crate(&'i str),
    /// A path, as a manifest writes it, whose directory holds no walked
    /// manifest: it leaves the tree, or the walk passed over it.
    Unwalked(&'i str),
    /// A `workspace = true` dependency, by its name, whose crate has no
    /// workspace root the walked manifests show.
    NoWorkspaceRoot(&'i str),
    /// A `workspace = true` dependency, by its name, that its root's
    /// `[workspace.dependencies]` does not name, which cargo refuses.
    NotInWorkspace(&'i str),
}

/// What the walk found of a crate's manifest.
#[derive(Debug, Clone, Copy)]
enum Found<'i> {
    /// Its index, and what it states as a manifest.
    Read(Option<&'i Manifest>),
    Unread,
}

/// The crates of `files`, the walked files, those of `unread` having no
/// index, by directory.
pub(super) fn crates<'i>(
    files: &Files<'i>,
    unread: &BTreeSet<&str>,
) -> BTreeMap<&'i str, Crate<'i>> {
    let manifests: BTreeMap<&str, Found<'_>> = files
        .iter()
        .filter(|(path, _)| file_name(path) == MANIFEST)
        .map(|(&path, (_, index))| {
            let found = match unread.contains(path) {
                true => Found::Unread,
                false => Found::Read(index.manifest.as_deref()),
            };
            (parent(path), found)
        })
        .collect();
    let graph = Graph::of(&manifests);
    manifests
        .iter()
        .map(|(&dir, found)| {
            let crate_node = match found {
                Found::Read(manifest) => graph.crate_at(dir, *manifest),
                Found::Unread => Crate::Unread,
            };
            (dir, crate_node)
        })
        .collect()
}

/// The walked manifests by directory, each workspace root's entries by
/// its directory and their name, and the directories each root excludes.
struct Graph<'m, 'i> {
    manifests: &'m BTreeMap<&'i str, Found<'i>>,
    inherited: BTreeMap<(&'i str, &'i str), Option<&'i str>>,
    excluded: BTreeMap<&'i str, BTreeSet<String>>,
}

impl<'m, 'i> Graph<'m, 'i> {
    fn of(manifests: &'m BTreeMap<&'i str, Found<'i>>) -> Self {
        let roots = manifests.iter().filter_map(|(&dir, found)| match found {
            Found::Read(manifest) => Some((dir, manifest.as_ref()?.workspace.as_ref()?)),
            Found::Unread => None,
        });
        let mut inherited = BTreeMap::new();
        let mut excluded = BTreeMap::new();
        for (dir, workspace) in roots {
            for entry in &workspace.dependencies {
                inherited.insert((dir, entry.name.as_str()), entry.path.as_deref());
            }
            let paths = workspace.exclude.iter();
            let plain = paths.filter(|path| !path.split('/').any(|segment| segment == ".."));
            excluded.insert(dir, plain.filter_map(|path| resolve(dir, path)).collect());
        }
        Self {
            manifests,
            inherited,
            excluded,
        }
    }

    /// The crate whose read manifest at `dir` states `manifest`.
    fn crate_at(&self, dir: &str, manifest: Option<&'i Manifest>) -> Crate<'i> {
        let Some(package) = manifest.and_then(|manifest| manifest.package.as_ref()) else {
            return Crate::Read {
                name: None,
                edges: BTreeSet::new(),
            };
        };
        Crate::Read {
            name: package.name.as_deref(),
            edges: self.edges(dir, package),
        }
    }

    /// The edges of `package`, whose manifest is at `dir`.
    fn edges(&self, dir: &str, package: &'i Package) -> BTreeSet<Edge<'i>> {
        let root = self.workspace_root(dir, package.workspace.as_deref());
        let endpoint = |source: &'i DependencySource| match source {
            DependencySource::Path(path) => Some(self.at(dir, path)),
            DependencySource::Workspace(name) => {
                let Some(root) = root else {
                    return Some(Endpoint::NoWorkspaceRoot(name));
                };
                match self.inherited.get(&(root, name.as_str())) {
                    None => Some(Endpoint::NotInWorkspace(name)),
                    Some(path) => path.map(|path| self.at(root, path)),
                }
            }
        };
        let edges = package.dependencies.iter().filter_map(|dependency| {
            Some(Edge {
                kind: dependency.kind,
                to: endpoint(&dependency.source)?,
            })
        });
        edges.collect()
    }

    /// The crate `path`, written in the manifest at `dir`, names.
    fn at(&self, dir: &str, path: &'i str) -> Endpoint<'i> {
        let found =
            resolve(dir, path).and_then(|target| self.manifests.get_key_value(target.as_str()));
        match found {
            Some((&target, _)) => Endpoint::Crate(target),
            None => Endpoint::Unwalked(path),
        }
    }

    /// The directory of the workspace root of the crate at `dir`, as the
    /// module's doc states it: `named_root` from `dir` when it names one,
    /// or else the nearest. `None` when the named one is no root, when
    /// none is found, or when an unread manifest ends the search.
    fn workspace_root(&self, dir: &str, named_root: Option<&str>) -> Option<&'i str> {
        if let Some(named) = named_root {
            return self.named_root(dir, named);
        }
        for candidate in std::iter::once(dir).chain(ancestors(dir)) {
            let Some((&root, found)) = self.manifests.get_key_value(candidate) else {
                continue;
            };
            match found {
                Found::Unread => return None,
                Found::Read(_) if is_root(*found) => {
                    if !self.excludes(root, dir) {
                        return Some(root);
                    }
                }
                Found::Read(manifest) => {
                    let package = manifest.and_then(|manifest| manifest.package.as_ref());
                    if let Some(named) = package.and_then(|package| package.workspace.as_deref()) {
                        return self.named_root(candidate, named);
                    }
                }
            }
        }
        None
    }

    /// The root `named`, a `package.workspace` of the manifest at `dir`,
    /// names; `None` when its directory's walked manifest has no
    /// `[workspace]`.
    fn named_root(&self, dir: &str, named: &str) -> Option<&'i str> {
        let (&root, found) = self
            .manifests
            .get_key_value(resolve(dir, named)?.as_str())?;
        is_root(*found).then_some(root)
    }

    /// Whether an `exclude` path of the root at `root` covers the crate at
    /// `dir`: names it or a directory above it.
    fn excludes(&self, root: &str, dir: &str) -> bool {
        let Some(excluded) = self.excluded.get(root) else {
            return false;
        };
        std::iter::once(dir)
            .chain(ancestors(dir))
            .any(|above| excluded.contains(above))
    }
}

/// Whether a manifest the walk found is read and has a `[workspace]`.
fn is_root(found: Found<'_>) -> bool {
    match found {
        Found::Read(manifest) => manifest.is_some_and(|manifest| manifest.workspace.is_some()),
        Found::Unread => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tree::CodeTree;
    use super::*;
    use crate::code::testing::facts;

    /// A workspace at `ws/`, a crate outside every workspace, and one
    /// whose `package.workspace` names its root.
    const WORKSPACE: &[(&str, &str)] = &[
        (
            "ws/Cargo.toml",
            "[workspace]\n[workspace.dependencies]\ncore = { path = 'core' }\nserde = '1'\nfar = { path = '../../far' }\n",
        ),
        (
            "ws/app/Cargo.toml",
            "[package]\nname = 'app'\n\
             [dependencies]\ncore = { workspace = true }\nutil = { path = '../util', package = 'util-lib' }\n\
             serde = { workspace = true }\nfar = { workspace = true }\nmissing = { path = '../missing' }\n\
             unknown = { workspace = true }\nregex = '1'\n\
             [dev-dependencies]\ncore = { workspace = true }\n\
             [build-dependencies]\ngen = { path = '../gen' }\n\
             [target.'cfg(unix)'.dependencies]\nutil = { path = '../util' }\n",
        ),
        ("ws/core/Cargo.toml", "[package]\nname = 'core'\n"),
        ("ws/util/Cargo.toml", "[package]\nname = 'util-lib'\n"),
        ("ws/gen/Cargo.toml", "[package]\nname.workspace = true\n"),
        (
            "lone/Cargo.toml",
            "[package]\nname = 'lone'\n[dependencies]\ncore = { workspace = true }\n",
        ),
        (
            "pointed/Cargo.toml",
            "[package]\nname = 'pointed'\nworkspace = '../ws'\n[dependencies]\ncore = { workspace = true }\n",
        ),
    ];

    /// Each crate of `files`: its directory, its name and its edges.
    fn crates_of(files: &[(&str, &str)]) -> Vec<String> {
        let facts = facts(files, &[]);
        let tree = CodeTree::of(&facts);
        files
            .iter()
            .map(|(path, _)| tree.spelled_crate(parent(path)))
            .collect()
    }

    #[test]
    fn a_crates_edges_are_its_path_and_workspace_dependencies_once_a_kind() {
        assert_eq!(
            crates_of(WORKSPACE),
            [
                "ws None: ",
                "ws/app Some(\"app\"): \
                 Normal Crate(\"ws/core\"), Normal Crate(\"ws/util\"), \
                 Normal Unwalked(\"../../far\"), Normal Unwalked(\"../missing\"), \
                 Normal NotInWorkspace(\"unknown\"), \
                 Dev Crate(\"ws/core\"), Build Crate(\"ws/gen\")",
                "ws/core Some(\"core\"): ",
                "ws/util Some(\"util-lib\"): ",
                "ws/gen None: ",
                "lone Some(\"lone\"): Normal NoWorkspaceRoot(\"core\")",
                "pointed Some(\"pointed\"): Normal Crate(\"ws/core\")",
            ]
        );
    }

    #[test]
    fn a_root_whose_exclude_covers_a_crate_is_passed_over_for_the_next_one_above() {
        let files = [
            (
                "Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'outer' }\n",
            ),
            (
                "inner/Cargo.toml",
                "[workspace]\nexclude = ['c']\n[workspace.dependencies]\nx = { path = 'x' }\n",
            ),
            (
                "inner/c/d/Cargo.toml",
                "[package]\n[dependencies]\nx = { workspace = true }\n",
            ),
            (
                "inner/e/Cargo.toml",
                "[package]\n[dependencies]\nx = { workspace = true }\n",
            ),
            ("outer/Cargo.toml", "[package]\n"),
            ("inner/x/Cargo.toml", "[package]\n"),
        ];

        assert_eq!(
            crates_of(&files)[2..4],
            [
                "inner/c/d None: Normal Crate(\"outer\")",
                "inner/e None: Normal Crate(\"inner/x\")",
            ]
        );
    }

    #[test]
    fn a_package_on_the_way_up_that_names_its_root_ends_the_search_there() {
        let files = [
            (
                "a/Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'xa' }\n",
            ),
            (
                "other/Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'xo' }\n",
            ),
            ("a/mid/Cargo.toml", "[package]\nworkspace = '../../other'\n"),
            (
                "a/mid/c/Cargo.toml",
                "[package]\n[dependencies]\nx = { workspace = true }\n",
            ),
            ("a/xa/Cargo.toml", "[package]\n"),
            ("other/xo/Cargo.toml", "[package]\n"),
        ];

        assert_eq!(
            crates_of(&files)[3],
            "a/mid/c None: Normal Crate(\"other/xo\")"
        );
    }

    #[test]
    fn an_exclude_entry_with_a_parent_segment_covers_nothing() {
        let files = [
            (
                "Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'outer' }\n",
            ),
            (
                "inner/Cargo.toml",
                "[workspace]\nexclude = ['../inner/c']\n[workspace.dependencies]\nx = { path = 'x' }\n",
            ),
            (
                "inner/c/Cargo.toml",
                "[package]\n[dependencies]\nx = { workspace = true }\n",
            ),
            ("outer/Cargo.toml", "[package]\n"),
            ("inner/x/Cargo.toml", "[package]\n"),
        ];

        assert_eq!(
            crates_of(&files)[2],
            "inner/c None: Normal Crate(\"inner/x\")"
        );
    }

    #[test]
    fn an_unread_manifest_is_a_crate_whose_name_and_edges_are_unknown() {
        let files = [
            (
                "a/Cargo.toml",
                "[package]\nname = 'a'\n[dependencies]\nb = { path = '../b' }\n",
            ),
            ("b/Cargo.toml", "[package\n"),
        ];

        assert_eq!(
            crates_of(&files),
            ["a Some(\"a\"): Normal Crate(\"b\")", "b unread"]
        );
    }

    #[test]
    fn an_unread_manifest_ends_the_search_for_a_workspace_root() {
        let files = [
            (
                "Cargo.toml",
                "[workspace]\n[workspace.dependencies]\nx = { path = 'x' }\n",
            ),
            ("mid/Cargo.toml", "[workspace\n"),
            (
                "mid/c/Cargo.toml",
                "[package]\n[dependencies]\nx = { workspace = true }\n",
            ),
        ];

        assert_eq!(
            crates_of(&files)[2],
            "mid/c None: Normal NoWorkspaceRoot(\"x\")"
        );
    }

    #[test]
    fn thousands_of_dependencies_are_indexed_and_resolved_within_the_files_room() {
        let count = 20_000;
        let entries: String = (0..count)
            .map(|n| format!("d{n} = {{ path = 'd' }}\nw{n} = {{ workspace = true }}\n"))
            .collect();
        let inherited: String = (0..count)
            .map(|n| format!("w{n} = {{ path = 'w' }}\n"))
            .collect();
        let manifest = format!("[package]\n[dependencies]\n{entries}");
        let root = format!("[workspace.dependencies]\n{inherited}");
        let files = [
            ("Cargo.toml", root.as_str()),
            ("x/Cargo.toml", manifest.as_str()),
            ("x/d/Cargo.toml", "[package]\n"),
            ("w/Cargo.toml", "[package]\n"),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let Some(Crate::Read { edges, .. }) = tree.crate_at("x") else {
            panic!("{:?}", tree.crate_at("x"));
        };
        let edges: Vec<&Endpoint<'_>> = edges.iter().map(|edge| &edge.to).collect();

        assert_eq!(edges, [&Endpoint::Crate("w"), &Endpoint::Crate("x/d")]);
    }
}
