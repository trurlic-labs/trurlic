//! Golden-file assertions.
//!
//! `assert_golden(name, actual)` compares `actual` with
//! `tests/integration/golden/<name>`. Run with `TRURLIC_UPDATE_GOLDEN=1`
//! to write `actual` to the file instead, then review the diff in git.

use std::env;
use std::fs;
use std::path::PathBuf;

const UPDATE_VAR: &str = "TRURLIC_UPDATE_GOLDEN";

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration/golden")
        .join(name)
}

fn updating() -> bool {
    env::var_os(UPDATE_VAR).is_some_and(|setting| setting == "1")
}

/// Assert that `actual` equals the golden file `name`, or rewrite the file
/// when `TRURLIC_UPDATE_GOLDEN=1`.
pub fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if updating() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden file {}: {e}\nrun with {UPDATE_VAR}=1 to create it",
            path.display()
        )
    });
    // A Windows checkout may convert line endings; the content is what counts.
    let expected = expected.replace("\r\n", "\n");
    if expected != actual {
        let line = first_difference(&expected, actual);
        panic!(
            "{} differs from the output, first at line {line}\n\
             run with {UPDATE_VAR}=1 to accept the new output, then review the diff\n\
             --- expected\n{expected}\n--- actual\n{actual}",
            path.display()
        );
    }
}

fn first_difference(expected: &str, actual: &str) -> usize {
    let mismatch = expected
        .lines()
        .zip(actual.lines())
        .position(|(e, a)| e != a);
    mismatch.unwrap_or_else(|| expected.lines().count().min(actual.lines().count())) + 1
}
