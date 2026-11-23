//! `trurlic migrate` over the fixture graph of every historical format
//! (`tests/fixtures/formats/<version>/`, each written by the trurlic of its
//! time), and the refusal of a format version the binary cannot read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::golden::assert_golden;
use crate::harness::Project;

/// A project whose `.trurlic/` holds the fixture graph of format `version`.
fn fixture_project(version: &str) -> Project {
    let project = Project::init();
    let store = store(&project);
    fs::remove_dir_all(&store).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formats")
        .join(version);
    copy_dir(&fixture, &store);
    project
}

fn store(project: &Project) -> PathBuf {
    project.path().join(".trurlic")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every file under `dir` but `.state/`.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != ".state") {
                found.extend(files(&path));
            }
        } else {
            found.push(path);
        }
    }
    found
}

/// The files under `root` as text: each one's path, then its content, in
/// path order.
fn render(root: &Path) -> String {
    let mut paths = files(root);
    paths.sort();
    let mut text = String::new();
    for path in paths {
        let name = path.strip_prefix(root).unwrap().display();
        text.push_str(&format!(
            "--- {name}\n{}",
            fs::read_to_string(&path).unwrap()
        ));
    }
    text
}

/// The backup directories beside `.trurlic/`.
fn backups(project: &Project) -> Vec<PathBuf> {
    fs::read_dir(project.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".trurlic-backup-"))
        })
        .collect()
}

fn stdout(project: &Project, args: &[&str]) -> String {
    String::from_utf8(project.run_ok(args).stdout).unwrap()
}

/// `trurlic <args>` fails with status 1; its stderr.
fn refused(project: &Project, args: &[&str]) -> String {
    let output = project.command().args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "trurlic {args:?} must fail");
    String::from_utf8(output.stderr).unwrap()
}

/// Migrate the fixture of `version`, check the result, and compare the
/// report and every migrated byte with the golden file. The backup holds
/// the files as they were, and a second run finds nothing to do.
fn migrates_and_passes_check(version: &str) {
    let project = fixture_project(version);
    let before = render(&store(&project));

    let output = stdout(&project, &["migrate"]);

    let (report, backup) = output.split_once("backup: ").unwrap();
    let backup = project.path().join(backup.trim_end().trim_end_matches('/'));
    assert_eq!(backups(&project), std::slice::from_ref(&backup));
    assert_eq!(render(&backup), before);
    project.run_ok(&["check"]);
    assert_golden(
        &format!("migrate_{version}.txt"),
        &format!("{report}\n{}", render(&store(&project))),
    );
    let again = stdout(&project, &["migrate"]);
    assert!(again.starts_with("Already up to date"), "{again}");
}

#[test]
fn format_0_1_0_migrates_and_passes_check() {
    migrates_and_passes_check("0.1.0");
}

#[test]
fn format_0_2_0_migrates_and_passes_check() {
    migrates_and_passes_check("0.2.0");
}

#[test]
fn format_0_3_0_migrates_and_passes_check() {
    migrates_and_passes_check("0.3.0");
}

#[test]
fn format_0_4_0_migrates_and_passes_check() {
    migrates_and_passes_check("0.4.0");
}

#[test]
fn a_dry_run_prints_the_report_and_writes_nothing() {
    let project = fixture_project("0.3.0");
    let before = render(&store(&project));

    let dry = stdout(&project, &["migrate", "--dry-run"]);

    assert_eq!(render(&store(&project)), before);
    assert!(backups(&project).is_empty());
    let migrated = stdout(&project, &["migrate"]);
    let (report, _) = migrated.split_once("backup: ").unwrap();
    let same = |text: &str| {
        text.replace("Would migrate", "Migrated")
            .replace("Would rewrite", "Rewrote")
    };
    assert_eq!(same(&dry), report);
}

/// A project whose `project.toml` states `version` as its format version.
fn project_with_version(version: &str) -> (Project, PathBuf) {
    let project = Project::init();
    let path = store(&project).join("project.toml");
    let text = fs::read_to_string(&path).unwrap();
    let (_, rest) = text.split_once('\n').unwrap();
    fs::write(&path, format!("format_version = \"{version}\"\n{rest}")).unwrap();
    (project, fs::canonicalize(path).unwrap())
}

#[test]
fn an_unparseable_version_is_refused_by_load_and_by_migrate() {
    let (project, path) = project_with_version("0.4.one");
    let expected = format!(
        "{}: format version `0.4.one` is not MAJOR.MINOR.PATCH",
        path.display()
    );

    for args in [
        &["check"][..],
        &["status"],
        &["migrate"],
        &["migrate", "--dry-run"],
    ] {
        let stderr = refused(&project, args);
        assert!(stderr.contains(&expected), "{args:?}: {stderr}");
    }
    assert!(backups(&project).is_empty());
}

#[test]
fn a_newer_format_is_refused_naming_both_versions() {
    let (project, path) = project_with_version("0.9.0");

    for args in [&["check"][..], &["migrate"]] {
        let stderr = refused(&project, args);
        assert!(
            stderr.contains(&format!(
                "{}: format version 0.9.0 is newer than the 0.4.1",
                path.display()
            )),
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains("upgrade trurlic"), "{stderr}");
    }
}

/// Run `trurlic migrate` on the 0.1.0 fixture stopped at failpoint `spec`,
/// beside a reference project migrated without a stop.
#[cfg(feature = "failpoints")]
fn stopped_migration(spec: &str) -> (Project, Project) {
    let reference = fixture_project("0.1.0");
    reference.run_ok(&["migrate"]);
    let project = fixture_project("0.1.0");
    let output = project
        .command()
        .arg("migrate")
        .env("TRURLIC_FAILPOINT", spec)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("failpoint {spec} hit")),
        "{stderr}"
    );
    (project, reference)
}

/// Stopped after its journal, the commit point, with `project.toml` still
/// in the old format: every other command refuses the store, naming
/// `trurlic migrate`, and touches nothing; the next migrate finishes the
/// commit and finds the store current.
#[cfg(feature = "failpoints")]
#[test]
fn a_migration_stopped_after_its_journal_lands_on_the_next_migrate() {
    let (project, reference) = stopped_migration("commit.journaled:1");

    let stderr = refused(&project, &["check"]);
    assert!(stderr.contains("run `trurlic migrate`"), "{stderr}");
    assert!(store(&project).join(".state/txn.toml").is_file());
    let again = stdout(&project, &["migrate"]);
    assert!(again.starts_with("Already up to date"), "{again}");
    project.run_ok(&["check"]);
    assert_eq!(render(&store(&project)), render(&store(&reference)));
}

/// Stopped after its first rename, which puts `project.toml` in the
/// current format: the store is a current one with a commit to finish, and
/// the next command of any kind finishes it.
#[cfg(feature = "failpoints")]
#[test]
fn a_migration_stopped_after_project_toml_lands_on_the_next_command() {
    let (project, reference) = stopped_migration("commit.applied:1");

    project.run_ok(&["check"]);
    assert!(!store(&project).join(".state/txn.toml").exists());
    assert_eq!(render(&store(&project)), render(&store(&reference)));
}
