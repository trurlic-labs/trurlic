//! Test code a Rust file's bytes cannot state, read from the tree: a file
//! under its crate's `tests/` directory, and a module file that only test
//! code declares. It is a step over the tree, like the macro expansion,
//! so a file's cached index stays a function of its bytes.
//!
//! The walk goes forward from each crate's targets as cargo finds them
//! without a manifest table: `src/lib.rs`, `src/main.rs`, `build.rs`, and
//! each `src/bin/`, `tests/`, `benches/` and `examples/` file (`x.rs` or
//! `x/main.rs`); a target a manifest's `path =` names elsewhere is not
//! read. From each file it follows the `mod name;` declarations the file
//! records, to `name.rs` or `name/mod.rs` in the module's directory, or
//! to the file a `#[path]` names. A file is test code when it is under
//! its crate's `tests/`, or when every declaration that reaches it is test
//! code, or in a file only test code reaches; a file no target reaches
//! keeps what its own bytes say. Each file is visited at most twice, once
//! as code and once as test, so the walk is linear in the tree.

use std::collections::{BTreeMap, BTreeSet};

use super::expand::{Crates, file_name, parent};
use super::index::{FileIndex, FileKind, ModuleFile};
use super::kinds::Role;
use super::scope::Scopes;
use super::symbol::Segment;

/// The files of a target's root in a crate, by their directory.
const ROOT_FILES: [&str; 3] = ["src/lib.rs", "src/main.rs", "build.rs"];

/// The directories each of whose files is a target's root.
const ROOT_DIRS: [&str; 4] = ["src/bin", "tests", "benches", "examples"];

/// The Rust files of `files` that are test code whole; `crates` is of
/// the same files.
pub(super) fn test_files<'i>(
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    let roots = target_roots(files, crates);
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
        if !first {
            continue;
        }
        let Some((_, index)) = files.get(path) else {
            continue;
        };
        let is_root = roots.contains(path);
        for module in &index.module_files {
            if let Some(target) = module_file(path, is_root, index, module, files) {
                pending.push((target, role.or(module.role)));
            }
        }
    }
    files
        .iter()
        .filter(|(_, (kind, _))| *kind == FileKind::Rust)
        .map(|(&path, _)| path)
        .filter(|path| {
            role_of_place(path, crates) == Role::Test
                || (reached.contains(path) && !reached_as_code.contains(path))
        })
        .collect()
}

/// The root files of the targets of every crate of `crates` that `files`
/// holds: each file whose place in its crate is a target's root.
fn target_roots<'i>(
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    files
        .iter()
        .filter(|(_, (kind, _))| *kind == FileKind::Rust)
        .map(|(&path, _)| path)
        .filter(|path| relative_to_crate(path, crates).is_some_and(is_target_root))
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
fn role_of_place(path: &str, crates: &Crates<'_>) -> Role {
    let root = crates.crate_of(path);
    let relative = match root {
        _ if root == path => None,
        "" => Some(path),
        root => path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/')),
    };
    match relative.is_some_and(|relative| relative.starts_with("tests/")) {
        true => Role::Test,
        false => Role::Code,
    }
}

/// The file of `files` that `module`, declared in the file at `path`,
/// names; `None` when there is none.
fn module_file<'i>(
    path: &str,
    is_root: bool,
    index: &FileIndex,
    module: &ModuleFile,
    files: &BTreeMap<&'i str, (FileKind, &'i FileIndex)>,
) -> Option<&'i str> {
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    let mut names: Vec<&str> = scopes
        .path_from_end(module.scope)
        .map(|segment| match segment {
            Segment::Name(name) => name.as_str(),
            Segment::Impl { name, .. } => name.as_str(),
        })
        .collect();
    names.reverse();
    let (name, inline) = names.split_last()?;
    // A crate's root and a `mod.rs` hold their modules beside them; any
    // other `x.rs` holds them in `x/`.
    let mut dir = parent(path).to_owned();
    if !is_root && file_name(path) != "mod.rs" {
        dir = join(&dir, file_name(path).strip_suffix(".rs")?);
    }
    let candidates = match &module.path {
        Some(named) if inline.is_empty() => vec![join(parent(path), named)],
        Some(named) => vec![join(&join(&dir, &inline.join("/")), named)],
        None => {
            let dir = inline.iter().fold(dir, |dir, module| join(&dir, module));
            vec![
                join(&dir, &format!("{name}.rs")),
                join(&dir, &format!("{name}/mod.rs")),
            ]
        }
    };
    candidates.into_iter().find_map(|candidate| {
        let normal = normalize(&candidate)?;
        files
            .get_key_value(normal.as_str())
            .map(|(&found, _)| found)
    })
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
                (*path, kind, index(kind, text.as_bytes()).unwrap())
            })
            .collect();
        let tree = indexes
            .iter()
            .map(|(path, kind, index)| (*path, (*kind, index)))
            .collect();
        let crates = Crates::of(&tree);
        test_files(&tree, &crates)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_file_is_test_code_under_tests_or_reached_by_test_code_alone() {
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
