//! Test code a file's bytes cannot state, read from the tree, so a file's
//! cached index stays a function of its bytes. A file of a language the
//! general resolver reads is test code whole in a test place its
//! language's row names (`tests/`, `test_*.py`, `conftest.py`,
//! `*.test.ts`, `__tests__/`). A Rust
//! file is test code under its crate's `tests/` directory, or as a module
//! file that only test code declares, a step over the crates like the
//! macro expansion.
//!
//! The walk goes forward from each crate's targets as cargo finds them
//! without a manifest table: `src/lib.rs`, `src/main.rs`, `build.rs`, and
//! each `src/bin/`, `tests/`, `benches/` and `examples/` file (`x.rs` or
//! `x/main.rs`); a target a manifest's `path =` names elsewhere is not
//! read. From each file it follows the `mod name;` declarations the file
//! records, to `name.rs` or `name/mod.rs` in the module's directory, or
//! to the file a relative `#[path]` names (an absolute one names no file
//! of the tree); the module's directory takes a segment for each inline
//! `mod` around the declaration, none for a function or any other item
//! it stands in. A file is test code when it is under
//! its crate's `tests/`, or when every declaration that reaches it is test
//! code, or in a file only test code reaches; a file no target reaches
//! keeps what its own bytes say. Each file is visited at most twice, once
//! as code and once as test, so the walk is linear in the tree.
//!
//! A file an anchor brings in from outside the walk is a root or a
//! module like any other, but the walk does not follow its own
//! declarations: what it declares is read for no other file.

use std::collections::BTreeSet;

use super::crates::{Crates, file_name, parent};
use super::expand::Files;
use super::index::{FileIndex, FileKind, ModuleFile};
use super::kinds::{DefinitionKind, Role};
use super::scope::ScopeId;
use super::symbol::Segment;

/// The files of a target's root in a crate, by their directory.
const ROOT_FILES: [&str; 3] = ["src/lib.rs", "src/main.rs", "build.rs"];

/// The directories each of whose files is a target's root.
const ROOT_DIRS: [&str; 4] = ["src/bin", "tests", "benches", "examples"];

/// The files of `files` and `outside` that are test code whole; `crates`
/// is of the same files. The declarations of `outside`'s Rust files,
/// those no walk took, are not followed.
pub(super) fn test_files<'i>(
    files: &Files<'i>,
    outside: &Files<'i>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    let mut found = rust_test_files(files, outside, crates);
    found.extend(
        files
            .iter()
            .chain(outside)
            .filter(|(path, (kind, _))| {
                kind.language()
                    .is_some_and(|language| language.is_test_place(path))
            })
            .map(|(&path, _)| path),
    );
    found
}

/// The Rust files of `files` and `outside` that are test code whole.
fn rust_test_files<'i>(
    files: &Files<'i>,
    outside: &Files<'i>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    let rust = || {
        files
            .iter()
            .chain(outside)
            .filter(|(_, (kind, _))| *kind == FileKind::Rust)
            .map(|(&path, (_, index))| (path, *index))
    };
    let roots: BTreeSet<&str> = rust()
        .map(|(path, _)| path)
        .filter(|path| relative_to_crate(path, crates).is_some_and(is_target_root))
        .collect();
    let (reached, reached_as_code) = walk_forward(files, outside, crates, &roots);
    rust()
        .map(|(path, _)| path)
        .filter(|path| {
            role_of_place(path, crates) == Role::Test
                || (reached.contains(path) && !reached_as_code.contains(path))
        })
        .collect()
}

/// The files the walk from `roots` reaches, and those it reaches as
/// code; the declarations of the files of `outside` are not followed.
fn walk_forward<'i>(
    files: &Files<'i>,
    outside: &Files<'i>,
    crates: &Crates<'i>,
    roots: &BTreeSet<&'i str>,
) -> (BTreeSet<&'i str>, BTreeSet<&'i str>) {
    let mut reached_as_code = BTreeSet::new();
    let mut reached = BTreeSet::new();
    let mut pending: Vec<(&str, Role)> = roots
        .iter()
        .map(|&root| (root, role_of_place(root, crates)))
        .collect();
    while let Some((path, role)) = pending.pop() {
        let first = match role {
            Role::Code => reached_as_code.insert(path),
            Role::Test => reached.insert(path),
        };
        reached.insert(path);
        let Some((_, index)) = files.get(path).filter(|_| first) else {
            continue;
        };
        let modules = module_scopes(index);
        let declaring = Declaring {
            path,
            is_root: roots.contains(path),
            index,
            modules: &modules,
        };
        for module in &index.rust.module_files {
            let target = declaring.module_file(module, |candidate| {
                files
                    .get_key_value(candidate)
                    .or_else(|| outside.get_key_value(candidate))
                    .map(|(&found, _)| found)
            });
            if let Some(target) = target {
                pending.push((target, role.or(module.role)));
            }
        }
    }
    (reached, reached_as_code)
}

/// The scopes of `index` that are modules: the directories of a module
/// file declared inside them.
fn module_scopes(index: &FileIndex) -> BTreeSet<ScopeId> {
    index
        .definitions
        .iter()
        .filter(|definition| definition.kind == DefinitionKind::Module)
        .map(|definition| definition.scope)
        .collect()
}

/// Whether `relative`, a path from its crate's directory, is a target's
/// root.
fn is_target_root(relative: &str) -> bool {
    ROOT_FILES.contains(&relative)
        || ROOT_DIRS.iter().any(|dir| {
            let rest = relative
                .strip_prefix(dir)
                .and_then(|rest| rest.strip_prefix('/'));
            rest.is_some_and(|rest| match rest.split_once('/') {
                None => rest.ends_with(".rs"),
                Some((_, file)) => file == "main.rs",
            })
        })
}

/// `path` from its crate's directory; `None` outside every crate.
fn relative_to_crate<'p>(path: &'p str, crates: &Crates<'_>) -> Option<&'p str> {
    match crates.crate_of(path) {
        root if root == path => None,
        "" => Some(path),
        root => path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/')),
    }
}

/// `Test` for a file under its crate's `tests/` directory.
pub(super) fn role_of_place(path: &str, crates: &Crates<'_>) -> Role {
    match relative_to_crate(path, crates).is_some_and(|relative| relative.starts_with("tests/")) {
        true => Role::Test,
        false => Role::Code,
    }
}

/// The target of its crate that `path` belongs to by its place, as the
/// path of its directory or root file: `src` for a file of the library or
/// the `src/main.rs` binary, `<dir>/<name>` for one of the `src/bin`,
/// `tests`, `benches` or `examples` target `name` (`name.rs` or `name/`),
/// and any other file of the crate its own; `None` outside every crate. A
/// manifest's `path =` is not read.
pub(super) fn target_of<'p>(path: &'p str, crates: &Crates<'_>) -> Option<&'p str> {
    let relative = relative_to_crate(path, crates)?;
    let start = path.len() - relative.len();
    let in_dir = ROOT_DIRS.iter().find_map(|dir| {
        let rest = relative.strip_prefix(dir)?.strip_prefix('/')?;
        let first = rest.split_once('/').map_or(rest, |(first, _)| first);
        let name = first.strip_suffix(".rs").unwrap_or(first);
        Some(dir.len() + 1 + name.len())
    });
    let end = match in_dir {
        Some(end) => end,
        None if relative.starts_with("src/") => "src".len(),
        None => relative.len(),
    };
    path.get(..start + end)
}

/// A file whose module declarations are followed.
struct Declaring<'a> {
    path: &'a str,
    is_root: bool,
    index: &'a FileIndex,
    /// Its scopes that are inline modules.
    modules: &'a BTreeSet<ScopeId>,
}

impl Declaring<'_> {
    /// The file `module`, declared here, names, as `find` finds it by
    /// its path; `None` when there is none.
    fn module_file<'i>(
        &self,
        module: &ModuleFile,
        find: impl Fn(&str) -> Option<&'i str>,
    ) -> Option<&'i str> {
        let name = self.name_of(module.scope)?;
        let inline = self.inline_modules(module.scope);
        // A crate's root and a `mod.rs` hold their modules beside them; any
        // other `x.rs` holds them in `x/`.
        let mut dir = parent(self.path).to_owned();
        if !self.is_root && file_name(self.path) != "mod.rs" {
            dir = join(&dir, file_name(self.path).strip_suffix(".rs")?);
        }
        let candidates = match &module.path {
            Some(named) if named.starts_with('/') => return None,
            Some(named) if inline.is_empty() => vec![join(parent(self.path), named)],
            Some(named) => vec![join(&join(&dir, &inline.join("/")), named)],
            None => {
                let dir = inline.iter().fold(dir, |dir, module| join(&dir, module));
                vec![
                    join(&dir, &format!("{name}.rs")),
                    join(&dir, &format!("{name}/mod.rs")),
                ]
            }
        };
        candidates
            .into_iter()
            .find_map(|candidate| find(&normalize(&candidate)?))
    }

    /// The last name of the scope `id`.
    fn name_of(&self, id: ScopeId) -> Option<&str> {
        match self.index.scopes.get(id.at())?.name.last()? {
            Segment::Name(name) | Segment::Impl { name, .. } => Some(name),
        }
    }

    /// The names of the inline modules around the scope `id`, outermost
    /// first. A parent's id is below its child's, so the walk ends.
    fn inline_modules(&self, id: ScopeId) -> Vec<&str> {
        let mut names = Vec::new();
        let mut next = self
            .index
            .scopes
            .get(id.at())
            .and_then(|scope| scope.parent);
        while let Some(current) = next.filter(|current| *current < id) {
            if self.modules.contains(&current) {
                names.extend(self.name_of(current));
            }
            next = self
                .index
                .scopes
                .get(current.at())
                .and_then(|scope| scope.parent)
                .filter(|parent| *parent < current);
        }
        names.reverse();
        names
    }
}

/// `dir` and `rest` joined by a `/`; `rest` alone at the root.
fn join(dir: &str, rest: &str) -> String {
    match dir {
        "" => rest.to_owned(),
        dir => format!("{dir}/{rest}"),
    }
}

/// `path` with its `.` segments dropped and each `..` taking the segment
/// before it; `None` when a `..` leaves the tree.
fn normalize(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::index;

    fn test_files_of(files: &[(&str, &str)]) -> Vec<String> {
        let indexes: Vec<(&str, FileKind, FileIndex)> = files
            .iter()
            .map(|(path, text)| {
                let kind = FileKind::of(path).unwrap();
                (
                    *path,
                    kind,
                    index(kind, text.as_bytes(), &crate::code::BlankMacros::default()).unwrap(),
                )
            })
            .collect();
        let tree = indexes
            .iter()
            .map(|(path, kind, index)| (*path, (*kind, index)))
            .collect();
        let crates = Crates::of(&tree, &Files::new());
        test_files(&tree, &Files::new(), &crates)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_file_is_test_code_in_a_test_place_or_reached_by_test_code_alone() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            (
                "src/lib.rs",
                "mod a;\npub mod b;\nmod inline { #[cfg(test)] mod deep; }\n",
            ),
            ("src/main.rs", "mod cli;\n#[cfg(test)]\nmod tests;\n"),
            ("src/cli.rs", ""),
            ("src/a.rs", "fn f() {}\n"),
            ("src/tests.rs", "mod deeper;\n"),
            ("src/tests/deeper.rs", "mod deepest;\n"),
            ("src/tests/deeper/deepest/mod.rs", "fn g() {}\n"),
            (
                "src/b.rs",
                "#[cfg(test)]\nmod checks;\nmod plain;\n#[path = \"../far/away.rs\"]\n#[cfg(test)]\nmod moved;\n",
            ),
            ("src/b/checks.rs", ""),
            ("src/b/plain.rs", ""),
            ("src/inline/deep.rs", ""),
            ("far/away.rs", ""),
            ("src/imp.rs", ""),
            ("src/bin/tool/main.rs", "#[cfg(test)]\nmod helper;\n"),
            ("src/bin/tool/helper.rs", ""),
            ("tests/flow.rs", "mod common;\n"),
            ("tests/common/mod.rs", ""),
            ("crates/x/Cargo.toml", "[package]\n"),
            (
                "crates/x/src/lib.rs",
                "#[cfg(test)]\nmod imp;\n#[cfg(not(test))]\nmod imp;\n",
            ),
            ("crates/x/src/imp.rs", ""),
            ("crates/x/tests/it.rs", ""),
            ("loose/tests/alone.rs", ""),
            ("tools/conftest.py", ""),
            ("tools/run.py", ""),
            ("tools/tests/helpers.py", ""),
            ("tools/test_run.py", ""),
            ("tools/run_test.py", ""),
            ("tools/stubs.pyi", ""),
            ("web/src/app.ts", ""),
            ("web/src/app.test.ts", ""),
            ("web/src/__tests__/setup.js", ""),
            ("web/src/card.spec.jsx", ""),
        ];

        assert_eq!(
            test_files_of(&files),
            [
                "crates/x/tests/it.rs",
                "far/away.rs",
                "src/b/checks.rs",
                "src/bin/tool/helper.rs",
                "src/inline/deep.rs",
                "src/tests.rs",
                "src/tests/deeper.rs",
                "src/tests/deeper/deepest/mod.rs",
                "tests/common/mod.rs",
                "tests/flow.rs",
                "tools/conftest.py",
                "tools/run_test.py",
                "tools/test_run.py",
                "tools/tests/helpers.py",
                "web/src/__tests__/setup.js",
                "web/src/app.test.ts",
                "web/src/card.spec.jsx",
            ]
        );
    }

    #[test]
    fn the_walk_is_linear_in_the_modules_it_follows() {
        let count = 20_000;
        let declarations: String = (0..count)
            .map(|n| format!("#[cfg(test)]\nmod m{n};\n"))
            .collect();
        let names: Vec<String> = (0..count).map(|n| format!("src/m{n}.rs")).collect();
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", declarations.as_str()),
        ];
        files.extend(names.iter().map(|name| (name.as_str(), "")));

        assert_eq!(test_files_of(&files).len(), count);
    }
}
