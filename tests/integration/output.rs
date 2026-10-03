//! Command output on a stdout nobody reads.

use std::io;

use crate::harness::Project;

/// `println!` panicked when stdout was closed, as when the reader of a pipe
/// exits early; the command now fails with the I/O error.
#[test]
fn a_closed_stdout_fails_the_command_without_a_panic() {
    let project = Project::init();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);

    let output = project
        .command()
        .arg("status")
        .stdout(writer)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("error: I/O error: Broken pipe"),
        "{stderr}"
    );
}
