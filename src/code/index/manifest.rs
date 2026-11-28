//! What a Cargo manifest states of its crate, as a TOML document's index
//! holds it: the package's name, its `path` and workspace dependencies,
//! and a workspace root's dependencies and excluded paths. The TOML
//! resolver reads
//! them from any document (`lexical/manifest.rs`), so an index stays a
//! function of its kind and bytes; the facts over a tree read them for a
//! `Cargo.toml` alone (`facts/crate_graph.rs`).

/// A TOML document that has a `[package]` table, a `[workspace]` table or
/// both: what cargo reads as a manifest. One of the two is always
/// present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    /// `None` for a virtual workspace's manifest.
    pub(in crate::code) package: Option<Package>,
    /// `None` without a `[workspace]` table.
    pub(in crate::code) workspace: Option<Workspace>,
}

/// What `[workspace]` states of the crates below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    /// Every entry of `[workspace.dependencies]`.
    pub(in crate::code) dependencies: Vec<WorkspaceDependency>,
    /// The strings of `exclude`, each a path from the root's directory
    /// as written.
    pub(in crate::code) exclude: Vec<String>,
}

/// What `[package]` states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    /// `package.name` when it is a string; `None` when it is missing or
    /// anything else (`name.workspace = true`, which cargo refuses).
    pub(in crate::code) name: Option<String>,
    /// `package.workspace`, the path of its workspace root's directory
    /// as written, when it is a string.
    pub(in crate::code) workspace: Option<String>,
    /// Those of the top-level tables and of each `[target.<cfg>]`'s.
    pub(in crate::code) dependencies: Vec<Dependency>,
}

/// A dependency that names a crate of the tree, or may: one with a
/// `path`, or one inherited from the workspace. One of the registry or a
/// git repository alone is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dependency {
    pub(in crate::code) kind: DependencyKind,
    pub(in crate::code) source: DependencySource,
}

/// The table a dependency is in, top-level or under a `[target.<cfg>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DependencyKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`: the tests', benches' and examples' alone.
    Dev,
    /// `[build-dependencies]`: the build script's alone.
    Build,
}

impl DependencyKind {
    pub(in crate::code) const ALL: [Self; 3] = [Self::Normal, Self::Dev, Self::Build];

    /// The key of its table in a manifest.
    pub(in crate::code) const fn table(self) -> &'static str {
        match self {
            Self::Normal => "dependencies",
            Self::Dev => "dev-dependencies",
            Self::Build => "build-dependencies",
        }
    }
}

/// Where a dependency's crate is, as its manifest writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DependencySource {
    /// `x = { path = ".." }`: the path, from the manifest's directory.
    Path(String),
    /// `x = { workspace = true }`: its name, which the workspace root's
    /// `[workspace.dependencies]` resolves. It wins over a `path` beside
    /// it, which cargo ignores.
    Workspace(String),
}

/// An entry of `[workspace.dependencies]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceDependency {
    pub(in crate::code) name: String,
    /// The path from the workspace root's directory; `None` for an entry
    /// of the registry or a git repository.
    pub(in crate::code) path: Option<String>,
}
