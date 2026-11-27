//! What a TOML document states as a Cargo manifest ([`Manifest`]), read
//! from the parsed document of any TOML file, so its index stays a
//! function of its kind and bytes.
//!
//! A dependency is read from `[dependencies]`, `[dev-dependencies]` and
//! `[build-dependencies]`, top-level or under any `[target.<cfg>]`,
//! whether written inline or as its own table. One whose value is a
//! table with `workspace = true` is the workspace's, whatever else it
//! holds, as cargo reads it; else one with a string `path` is a path
//! dependency; anything else (a version, a git repository) is not read.
//! `[workspace.dependencies]` keeps every entry, with its string `path`
//! when it has one, and `[workspace]` the strings of its `exclude`. Not
//! read: the underscore spellings
//! (`dev_dependencies`) that edition 2024 refuses, `[patch]`, `[replace]`,
//! the target tables (`[lib]`, `[[bin]]`) and the workspace's `members`.

use toml::de::DeTable;

use crate::code::index::{
    Dependency, DependencyKind, DependencySource, Manifest, Package, Workspace, WorkspaceDependency,
};

/// The manifest `document` is; `None` when it has neither a `[package]`
/// nor a `[workspace]` table.
pub(super) fn manifest(document: &DeTable<'_>) -> Option<Manifest> {
    let package = table(document, "package").map(|package| Package {
        name: string(package, "name"),
        workspace: string(package, "workspace"),
        dependencies: dependencies(document),
    });
    let workspace = table(document, "workspace").map(|workspace| Workspace {
        dependencies: table(workspace, "dependencies")
            .map_or_else(Vec::new, workspace_dependencies),
        exclude: workspace
            .get("exclude")
            .and_then(|exclude| exclude.get_ref().as_array())
            .map_or_else(Vec::new, |paths| {
                let strings = paths.iter().filter_map(|path| path.get_ref().as_str());
                strings.map(str::to_owned).collect()
            }),
    });
    (package.is_some() || workspace.is_some()).then_some(Manifest { package, workspace })
}

/// The path and workspace dependencies of `document`'s tables.
fn dependencies(document: &DeTable<'_>) -> Vec<Dependency> {
    let targets = table(document, "target").into_iter().flat_map(|targets| {
        targets
            .values()
            .filter_map(|target| target.get_ref().as_table())
    });
    let mut found = Vec::new();
    for holder in std::iter::once(document).chain(targets) {
        for kind in DependencyKind::ALL {
            let Some(entries) = table(holder, kind.table()) else {
                continue;
            };
            found.extend(entries.iter().filter_map(|(name, value)| {
                let source = source(name.get_ref(), value.get_ref().as_table()?)?;
                Some(Dependency { kind, source })
            }));
        }
    }
    found
}

/// Where the dependency `name`, whose value is `entry`, is.
fn source(name: &str, entry: &DeTable<'_>) -> Option<DependencySource> {
    let inherited = entry
        .get("workspace")
        .and_then(|value| value.get_ref().as_bool());
    if inherited == Some(true) {
        return Some(DependencySource::Workspace(name.to_owned()));
    }
    string(entry, "path").map(DependencySource::Path)
}

/// Every entry of `[workspace.dependencies]`, with its string `path` when
/// its value is a table that has one.
fn workspace_dependencies(entries: &DeTable<'_>) -> Vec<WorkspaceDependency> {
    entries
        .iter()
        .map(|(name, value)| WorkspaceDependency {
            name: name.get_ref().to_string(),
            path: value
                .get_ref()
                .as_table()
                .and_then(|entry| string(entry, "path")),
        })
        .collect()
}

/// The table `table` holds at `key`.
fn table<'d, 't>(table: &'d DeTable<'t>, key: &str) -> Option<&'d DeTable<'t>> {
    table.get(key)?.get_ref().as_table()
}

/// The string `table` holds at `key`.
fn string(table: &DeTable<'_>, key: &str) -> Option<String> {
    table.get(key)?.get_ref().as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::FileKind;

    /// What the TOML document `text` states as a manifest.
    fn read(text: &str) -> Option<Manifest> {
        crate::code::testing::index(FileKind::Toml, text.as_bytes())
            .unwrap()
            .manifest
            .map(|manifest| *manifest)
    }

    fn path(kind: DependencyKind, path: &str) -> Dependency {
        Dependency {
            kind,
            source: DependencySource::Path(path.to_owned()),
        }
    }

    fn inherited(kind: DependencyKind, name: &str) -> Dependency {
        Dependency {
            kind,
            source: DependencySource::Workspace(name.to_owned()),
        }
    }

    #[test]
    fn a_dependency_with_a_path_or_from_the_workspace_is_read_from_each_table() {
        let text = "\
[package]
name = 'app'
workspace = '..'

[dependencies]
a = { path = '../a', package = 'renamed' }
b = { workspace = true }
both = { path = '../both', workspace = true }
registry = '1'
git = { git = 'git.example/x' }
off = { workspace = false }

[dependencies.table]
path = '../table'

[dev-dependencies]
c = { path = '../c' }

[build-dependencies]
d = { workspace = true }

[dev_dependencies]
old = { path = '../old' }

[target.'cfg(unix)'.dependencies]
e = { path = '../e' }
";
        let package = read(text).and_then(|manifest| manifest.package).unwrap();
        let mut dependencies = package.dependencies;
        dependencies.sort_by_key(|dependency| format!("{dependency:?}"));

        assert_eq!(package.name.as_deref(), Some("app"));
        assert_eq!(package.workspace.as_deref(), Some(".."));
        assert_eq!(
            dependencies,
            [
                inherited(DependencyKind::Build, "d"),
                path(DependencyKind::Dev, "../c"),
                path(DependencyKind::Normal, "../a"),
                path(DependencyKind::Normal, "../e"),
                path(DependencyKind::Normal, "../table"),
                inherited(DependencyKind::Normal, "b"),
                inherited(DependencyKind::Normal, "both"),
            ]
        );
    }

    #[test]
    fn a_workspace_root_keeps_every_dependency_and_its_excluded_paths() {
        let text = "\
[workspace]
members = ['a']
exclude = ['old', 7]

[workspace.dependencies]
a = { path = 'crates/a' }
serde = '1'
";
        let entry = |name: &str, path: Option<&str>| WorkspaceDependency {
            name: name.to_owned(),
            path: path.map(str::to_owned),
        };

        assert_eq!(
            read(text),
            Some(Manifest {
                package: None,
                workspace: Some(Workspace {
                    dependencies: vec![entry("a", Some("crates/a")), entry("serde", None)],
                    exclude: vec!["old".to_owned()],
                }),
            })
        );
        assert_eq!(
            read("[workspace]\n").and_then(|manifest| manifest.workspace),
            Some(Workspace {
                dependencies: Vec::new(),
                exclude: Vec::new(),
            })
        );
    }

    #[test]
    fn a_document_with_no_package_or_workspace_table_is_no_manifest() {
        assert_eq!(read("[dependencies]\na = { path = 'a' }\n"), None);
        assert_eq!(read("package = 'x'\n"), None);
    }

    #[test]
    fn a_package_name_that_is_no_string_is_none() {
        let package = read("[package]\nname.workspace = true\n").and_then(|m| m.package);

        assert_eq!(package.map(|package| package.name), Some(None));
    }
}
