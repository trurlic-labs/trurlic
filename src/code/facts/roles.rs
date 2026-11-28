//! Test code a file's bytes cannot state, read from the tree, so a file's
//! cached index stays a function of its bytes. A file of a language the
//! general resolver reads is test code whole in a test place its
//! language's row names (`tests/`, `test_*.py`, `conftest.py`,
//! `*.test.ts`, `__tests__/`). A Rust file is test code under its crate's
//! `tests/` directory, or as a module file that only test code declares:
//! a step over the crates by the `mod` walk (`walk.rs`), after each
//! file's own index. A file is test code when it is under its crate's
//! `tests/`, or when every declaration that reaches it is test code, or in
//! a file only test code reaches; a file no target reaches keeps what its
//! own bytes say.

use std::collections::BTreeSet;

use super::crates::{Crates, Files};
use super::walk::{Declared, Reach, relative_to_crate, walk};
use crate::code::index::{FileKind, Role};
use crate::code::parse::language_of;

/// The files of `files` and `outside` that are test code whole; `crates`
/// is of the same files. The declarations of `outside`'s Rust files,
/// those no walk took, are not followed.
pub(super) fn test_files<'i>(
    files: &Files<'i>,
    outside: &Files<'i>,
    crates: &Crates<'i>,
) -> BTreeSet<&'i str> {
    let reached = walk::<ReachedAs>(files, outside, crates, &mut ());
    let is_test = |path: &str, kind: FileKind| {
        if kind != FileKind::Rust {
            return language_of(kind).is_some_and(|language| language.is_test_place(path));
        }
        role_of_place(path, crates) == Role::Test
            || reached.get(path) == Some(&ReachedAs(Role::Test))
    };
    files
        .iter()
        .chain(outside)
        .filter(|(path, (kind, _))| is_test(path, *kind))
        .map(|(&path, _)| path)
        .collect()
}

/// The role a file is reached with: test code when its place or a
/// declaration on the way is, and code once any walk reaches it as code.
/// It rises once, from test to code, so a file is followed at most twice.
#[derive(Debug, PartialEq, Eq)]
struct ReachedAs(Role);

impl<'i> Reach<'i> for ReachedAs {
    type Shared = ();

    fn root(root: &'i str, crates: &Crates<'i>) -> Self {
        Self(role_of_place(root, crates))
    }

    fn through(&self, declared: &Declared<'i>, (): &mut ()) -> Self {
        Self(self.0.or(declared.role))
    }

    /// `None`: a second file is no module of the crate, so it keeps what
    /// its own bytes say, as a file no target reaches does.
    fn second_file(&self, _declared: &Declared<'i>, (): &mut ()) -> Option<Self> {
        None
    }

    fn join(&mut self, other: Self) -> bool {
        match (self.0, other.0) {
            (Role::Test, Role::Code) => {
                self.0 = Role::Code;
                true
            }
            (Role::Code, _) | (Role::Test, Role::Test) => false,
        }
    }
}

/// `Test` for a file under its crate's `tests/` directory.
fn role_of_place(path: &str, crates: &Crates<'_>) -> Role {
    match relative_to_crate(path, crates).is_some_and(|relative| relative.starts_with("tests/")) {
        true => Role::Test,
        false => Role::Code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::FileIndex;

    fn test_files_of(files: &[(&str, &str)]) -> Vec<String> {
        let indexes: Vec<(&str, FileKind, FileIndex)> = files
            .iter()
            .map(|(path, text)| {
                let kind = FileKind::of(path).unwrap();
                (
                    *path,
                    kind,
                    crate::code::testing::index(kind, text.as_bytes()).unwrap(),
                )
            })
            .collect();
        let tree: Files<'_> = indexes
            .iter()
            .map(|(path, kind, index)| (*path, (*kind, index)))
            .collect();
        let crates = Crates::of(tree.keys().copied(), []);
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

    #[test]
    fn the_second_file_of_a_module_keeps_the_role_its_bytes_give() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "#[cfg(test)]\nmod m;\n"),
            ("src/m.rs", ""),
            ("src/m/mod.rs", ""),
        ];

        assert_eq!(test_files_of(&files), ["src/m.rs"]);
    }

    #[test]
    fn a_manifest_with_no_index_still_makes_its_crate_for_the_test_roles() {
        let files = [
            ("inner/Cargo.toml", "[package\n"),
            ("inner/src/lib.rs", "#[cfg(test)]\nmod t;\n"),
            ("inner/src/t.rs", "fn f() {}\n"),
            ("inner/tests/x.rs", "fn g() {}\n"),
        ];
        let facts = crate::code::testing::facts(&files, &[]);
        let roles: Vec<(&str, Role)> = facts
            .definitions()
            .map(|(path, _, _, role)| (path, role))
            .collect();

        assert_eq!(
            roles,
            [
                ("inner/src/lib.rs", Role::Test),
                ("inner/src/t.rs", Role::Test),
                ("inner/tests/x.rs", Role::Test),
            ]
        );
    }
}
