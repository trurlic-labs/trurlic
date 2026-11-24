//! `trurlic migrate` over the fixture graph of every historical format
//! (`tests/fixtures/formats/<version>/`, each written by the trurlic of its
//! time), the placement it reads from git, and the refusal of a format
//! version the binary cannot read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::golden::assert_golden;
use crate::harness::{Project, git, isolated_git};

/// The report's file in the backup directory.
const REPORT_FILE: &str = "migration-report.txt";

/// `migrate` with the placement stated, so no test but the placement ones
/// asks git.
const MIGRATE: [&str; 3] = ["migrate", "--placement", "tracked"];

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

/// The report a migration printed and the backup directory it names.
fn split_report(project: &Project, output: &str) -> (String, PathBuf) {
    let (report, rest) = output.split_once("backup: ").unwrap();
    let (backup, report_line) = rest.split_once('\n').unwrap();
    let backup = project.path().join(backup.trim_end_matches('/'));
    let name = backup.file_name().unwrap().to_str().unwrap();
    assert_eq!(report_line, format!("report: {name}/{REPORT_FILE}\n"));
    (report.to_owned(), backup)
}

/// Migrate the fixture of `version`, check the result, and compare the
/// report and every migrated byte with the golden file. The backup holds
/// the files as they were and the report, and a second run finds nothing
/// to do.
fn migrates_and_passes_check(version: &str) {
    let project = fixture_project(version);
    let before = render(&store(&project));

    let output = stdout(&project, &MIGRATE);

    let (report, backup) = split_report(&project, &output);
    assert_eq!(backups(&project), std::slice::from_ref(&backup));
    assert_eq!(
        fs::read_to_string(backup.join(REPORT_FILE)).unwrap(),
        report
    );
    fs::remove_file(backup.join(REPORT_FILE)).unwrap();
    assert_eq!(render(&backup), before);
    project.run_ok(&["check"]);
    assert_golden(
        &format!("migrate_{version}.txt"),
        &format!("{report}\n{}", render(&store(&project))),
    );
    assert!(!store(&project).join("patterns").exists());
    let again = stdout(&project, &MIGRATE);
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
fn format_0_4_1_migrates_and_passes_check() {
    migrates_and_passes_check("0.4.1");
}

#[test]
fn a_dry_run_prints_the_report_and_writes_nothing() {
    let project = fixture_project("0.4.1");
    let before = render(&store(&project));

    let dry = stdout(
        &project,
        &["migrate", "--dry-run", "--placement", "tracked"],
    );

    assert_eq!(render(&store(&project)), before);
    assert!(backups(&project).is_empty());
    let migrated = stdout(&project, &MIGRATE);
    let (report, _) = split_report(&project, &migrated);
    let same = |text: &str| {
        text.replace("Would migrate", "Migrated")
            .replace("Would rewrite", "Rewrote")
            .replace("Would remove", "Removed")
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
        let stderr = project.run_refused(args);
        assert!(stderr.contains(&expected), "{args:?}: {stderr}");
    }
    assert!(backups(&project).is_empty());
}

#[test]
fn a_newer_format_is_refused_naming_both_versions() {
    let (project, path) = project_with_version("0.9.0");

    for args in [&["check"][..], &["migrate"]] {
        let stderr = project.run_refused(args);
        assert!(
            stderr.contains(&format!(
                "{}: format version 0.9.0 is newer than the 0.5.0",
                path.display()
            )),
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains("upgrade trurlic"), "{stderr}");
    }
}

/// A store of the format before this one: every command but `migrate`
/// names `trurlic migrate`, and touches nothing; `migrate` brings it to the
/// current format.
#[test]
fn the_format_before_this_one_is_refused_until_migrated() {
    let project = fixture_project("0.4.1");
    let before = render(&store(&project));

    for args in [&["check"][..], &["status"], &["fmt"]] {
        let stderr = project.run_refused(args);
        assert!(
            stderr.contains("run `trurlic migrate`"),
            "{args:?}: {stderr}"
        );
    }
    assert_eq!(render(&store(&project)), before);
    project.run_ok(&MIGRATE);
    project.run_ok(&["check"]);
}

/// The 0.4.1 fixture in a git repository, its graph committed, every git
/// call isolated from the user's configuration.
fn tracked_fixture() -> Project {
    let project = fixture_project("0.4.1");
    git(project.path(), &["init", "--quiet"]);
    git(project.path(), &["add", ".trurlic"]);
    git(
        project.path(),
        &["commit", "--quiet", "--no-verify", "-m", "graph"],
    );
    project
}

/// `trurlic migrate` reading git with no configuration of the user's.
fn migrate_reading_git(project: &Project) -> std::process::Output {
    project
        .command()
        .arg("migrate")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
}

fn placement(project: &Project) -> String {
    let text = fs::read_to_string(store(project).join("project.toml")).unwrap();
    let line = text
        .lines()
        .find(|line| line.starts_with("placement"))
        .unwrap();
    line.to_owned()
}

#[test]
fn a_graph_git_tracks_whole_is_tracked() {
    let project = tracked_fixture();

    let output = migrate_reading_git(&project);

    assert!(output.status.success(), "{output:?}");
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("placement = \"tracked\": git tracks all 10 files of the store"),
        "{report}"
    );
    assert_eq!(placement(&project), "placement = \"tracked\"");
}

#[test]
fn a_graph_git_does_not_track_is_shared() {
    let project = fixture_project("0.4.1");
    git(project.path(), &["init", "--quiet"]);
    fs::write(project.path().join(".git/info/exclude"), ".trurlic/\n").unwrap();

    let output = migrate_reading_git(&project);

    assert!(output.status.success(), "{output:?}");
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("placement = \"shared\": git tracks none of the 10 files of the store"),
        "{report}"
    );
    assert_eq!(placement(&project), "placement = \"shared\"");
}

/// `migrate` refused before anything is written: the store is as it was,
/// and no backup is made.
fn refused_untouched(project: &Project, output: &std::process::Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(backups(project).is_empty());
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn a_graph_git_tracks_in_part_is_refused_naming_how_to_state_it() {
    let project = tracked_fixture();
    project.write(
        "decisions/added-after-the-commit.toml",
        &project.read("decisions/no-unsafe-code.toml"),
    );
    let before = render(&store(&project));

    let output = migrate_reading_git(&project);

    let stderr = refused_untouched(&project, &output);
    assert!(
        stderr.contains(
            "git tracks 10 of its 11 files; not tracked: decisions/added-after-the-commit.toml"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("`trurlic migrate --placement tracked`"),
        "{stderr}"
    );
    assert!(stderr.contains("`--placement shared`"), "{stderr}");
    assert_eq!(render(&store(&project)), before);
    project.run_ok(&MIGRATE);
}

#[test]
fn a_graph_in_no_work_tree_is_refused_naming_how_to_state_it() {
    let project = fixture_project("0.4.1");

    let output = migrate_reading_git(&project);

    let stderr = refused_untouched(&project, &output);
    assert!(stderr.contains("not a git repository"), "{stderr}");
    assert!(
        stderr.contains("`trurlic migrate --placement tracked`"),
        "{stderr}"
    );
}

/// A `trurlic migrate` started from a git hook of another repository
/// inherits its variables; git still answers for the project.
#[test]
fn the_callers_git_variables_do_not_redirect_the_placement() {
    let project = fixture_project("0.4.1");
    git(project.path(), &["init", "--quiet"]);
    let other = Project::without_store();
    git(other.path(), &["init", "--quiet"]);
    fs::create_dir_all(other.path().join(".trurlic")).unwrap();
    fs::write(other.path().join(".trurlic/project.toml"), "").unwrap();
    git(other.path(), &["add", ".trurlic"]);
    let other_git = other.path().join(".git");

    let output = project
        .command()
        .arg("migrate")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_DIR", &other_git)
        .env("GIT_WORK_TREE", other.path())
        .env("GIT_INDEX_FILE", other_git.join("index"))
        .env("GIT_COMMON_DIR", &other_git)
        .env(
            "GIT_CONFIG_PARAMETERS",
            format!("'core.worktree'='{}'", other.path().display()),
        )
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(placement(&project), "placement = \"shared\"");
    let plain = isolated_git(project.path())
        .args(["ls-files", "--", ".trurlic"])
        .env("GIT_DIR", &other_git)
        .env("GIT_WORK_TREE", other.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(plain.stdout).unwrap(),
        ".trurlic/project.toml\n"
    );
}

/// Writing `A constrains B` as `B depends_on A` must not close a cycle:
/// `write-through-a-temp-file-and-rename` already depends on
/// `one-toml-file-per-node`.
#[test]
fn a_constrains_edge_that_closes_a_cycle_is_refused_naming_its_members() {
    let project = fixture_project("0.4.1");
    let graph = project.read("graph.toml");
    project.write(
        "graph.toml",
        &format!(
            "{graph}\n[[edges]]\nfrom = \"write-through-a-temp-file-and-rename\"\n\
             to = \"one-toml-file-per-node\"\nkind = \"constrains\"\n"
        ),
    );
    let before = render(&store(&project));

    let output = project.command().args(MIGRATE).output().unwrap();

    let stderr = refused_untouched(&project, &output);
    assert!(
        stderr.contains(
            "closes a depends_on cycle among `one-toml-file-per-node`, \
             `write-through-a-temp-file-and-rename`"
        ),
        "{stderr}"
    );
    assert_eq!(render(&store(&project)), before);
}

/// Run `trurlic migrate` on the 0.4.1 fixture stopped at failpoint `spec`,
/// beside a reference project migrated without a stop.
#[cfg(feature = "failpoints")]
fn stopped_migration(spec: &str) -> (Project, Project) {
    let reference = fixture_project("0.4.1");
    reference.run_ok(&MIGRATE);
    let project = fixture_project("0.4.1");
    let output = project
        .command()
        .args(MIGRATE)
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

/// Stopped with its files staged and no journal: nothing is committed, so
/// the store is the 0.4.1 one, and the next migrate makes the whole commit.
#[cfg(feature = "failpoints")]
#[test]
fn a_migration_stopped_before_its_journal_is_made_again_by_the_next_migrate() {
    let before = render(&store(&fixture_project("0.4.1")));
    let (project, reference) = stopped_migration("commit.staged:1");

    assert_eq!(render(&store(&project)), before);
    assert!(!store(&project).join(".state/txn.toml").exists());
    project.run_ok(&MIGRATE);
    project.run_ok(&["check"]);
    assert_eq!(render(&store(&project)), render(&store(&reference)));
}

/// Stopped after its journal, the commit point, with `project.toml` still
/// in the old format and `graph.toml` and the pattern still to remove:
/// every other command refuses the store, naming `trurlic migrate`, and
/// touches nothing; the next migrate finishes the commit and finds the
/// store current.
#[cfg(feature = "failpoints")]
#[test]
fn a_migration_stopped_after_its_journal_lands_on_the_next_migrate() {
    let (project, reference) = stopped_migration("commit.journaled:1");

    let stderr = project.run_refused(&["check"]);
    assert!(stderr.contains("run `trurlic migrate`"), "{stderr}");
    assert!(store(&project).join(".state/txn.toml").is_file());
    assert!(store(&project).join("graph.toml").is_file());
    let again = stdout(&project, &MIGRATE);
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

/// The 0.4.1 fixture's migration renames eight files, then removes
/// `graph.toml` and the pattern: stopped after the first removal, the next
/// command removes the pattern too.
#[cfg(feature = "failpoints")]
#[test]
fn a_migration_stopped_between_its_removals_lands_on_the_next_command() {
    let (project, reference) = stopped_migration("commit.applied:9");

    assert!(!store(&project).join("graph.toml").exists());
    assert!(
        store(&project)
            .join("patterns/durable-storage.toml")
            .is_file()
    );
    project.run_ok(&["check"]);
    assert!(
        !store(&project)
            .join("patterns/durable-storage.toml")
            .exists()
    );
    assert_eq!(render(&store(&project)), render(&store(&reference)));
}
