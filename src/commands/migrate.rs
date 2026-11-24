//! `trurlic migrate`: bring a store in an older format to the current one.
//!
//! The store plans the migration (`Store::plan_migration`) and commits it in
//! one journaled commit. This command tells the store the placement, stated
//! with `--placement` or read from the project's git; takes the lock; keeps a
//! copy of the old files beside `.trurlic/`, with the report in it; and
//! prints the report. The patterns and tags the migration takes out of the
//! graph live on only in that copy and its report, so both are flushed to
//! disk before the commit.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::console::{diag, out};
use crate::discovery::discover;
use crate::store::{
    FORMAT_VERSION, Fact, Loss, MigrationReport, PatternRecord, Placement, PlacementBasis,
    PlacementSource, Store, Timestamp, sync_dir,
};
use crate::{Error, Result};

use super::{DryRun, graph_name, placement};

/// The report's file in the backup directory.
const REPORT_FILE: &str = "migration-report.txt";

/// The backup names tried for one second before the last one's error stands.
const MAX_BACKUPS_PER_SECOND: u32 = 100;

/// Migrate the store above `cwd`. `placement`, when given, is the one the
/// migration writes; without it git tells. `DryRun::Yes` prints the report
/// and writes nothing.
pub(crate) fn migrate(cwd: &Path, dry_run: DryRun, placement: Option<Placement>) -> Result<()> {
    let store = discover(cwd)?;
    let source = match placement {
        Some(placement) => PlacementSource::Stated(placement),
        None => match placement::tracked(&store) {
            Ok(Some(tracked)) => PlacementSource::Git(Ok(tracked)),
            Ok(None) => PlacementSource::NoWorkTree,
            Err(e) => PlacementSource::Git(Err(e)),
        },
    };
    if dry_run == DryRun::Yes {
        return match store.preview_migration(source)? {
            Some(migration) => print(&report(&migration.report, &graph_name(&store), DryRun::Yes)),
            None => up_to_date(),
        };
    }

    let lock = store.lock()?;
    // Planned under the lock, so no write lands between the plan and its
    // commit, and a store another process migrated meanwhile is current.
    let Some(migration) = store.plan_migration(&lock, source)? else {
        return up_to_date();
    };
    let lines = report(&migration.report, &graph_name(&store), DryRun::No);
    // Copied under the lock, so no concurrent write can tear the copy.
    let backup = backup_dir(&store)?;
    if let Err(e) = back_up(store.root(), &backup, &lines) {
        let _ = fs::remove_dir_all(&backup);
        return Err(e);
    }
    match store.commit_migration(&lock, migration) {
        Ok(_) => {}
        // Recorded in the journal: the migration lands, and the copy stays.
        Err(e @ Error::WritePending { .. }) => return Err(e),
        Err(e) => {
            let _ = fs::remove_dir_all(&backup);
            return Err(e);
        }
    }
    drop(lock);

    print(&lines)?;
    let name = Path::new(backup.file_name().unwrap_or_default()).display();
    out!("backup: {name}/")?;
    out!("report: {name}/{REPORT_FILE}")?;
    Ok(())
}

fn up_to_date() -> Result<()> {
    out!("Already up to date (format version {FORMAT_VERSION}).")?;
    Ok(())
}

fn print(lines: &[String]) -> Result<()> {
    for line in lines {
        out!("{line}")?;
    }
    Ok(())
}

/// The report on the graph `graph` as lines: each step with what it
/// dropped and what else it reports, then the files written and removed,
/// all in the order the migration met them.
fn report(report: &MigrationReport, graph: &str, dry_run: DryRun) -> Vec<String> {
    let (migrate, rewrite, remove) = match dry_run {
        DryRun::Yes => ("Would migrate", "Would rewrite", "Would remove"),
        DryRun::No => ("Migrated", "Rewrote", "Removed"),
    };
    let mut lines = vec![format!(
        "{migrate} {graph} from format {} to {FORMAT_VERSION}.",
        report.from
    )];
    for step in &report.steps {
        let title = format!("  {} -> {} ({})", step.from, step.to, step.change);
        if step.losses.is_empty() && step.facts.is_empty() {
            lines.push(format!("{title}: nothing dropped"));
            continue;
        }
        lines.push(format!("{title}:"));
        losses(&mut lines, &step.losses);
        facts(&mut lines, &step.facts);
    }
    lines.push(format!("{rewrite} {} file(s):", report.rewritten.len()));
    lines.extend(report.rewritten.iter().map(|file| format!("  {file}")));
    if !report.removed.is_empty() {
        lines.push(format!("{remove} {} file(s):", report.removed.len()));
        lines.extend(report.removed.iter().map(|file| format!("  {file}")));
    }
    lines
}

fn losses(lines: &mut Vec<String>, losses: &[Loss]) {
    if losses.is_empty() {
        return;
    }
    lines.push(format!("    {} value(s) dropped:", losses.len()));
    for loss in losses {
        lines.push(format!(
            "      {}: {} = {}",
            loss.file, loss.field, loss.value
        ));
    }
}

/// `facts`, sorted, one section per kind: a heading, then one entry each.
fn facts(lines: &mut Vec<String>, facts: &[Fact]) {
    for run in facts.chunk_by(|a, b| std::mem::discriminant(a) == std::mem::discriminant(b)) {
        let Some(first) = run.first() else {
            continue;
        };
        lines.push(format!("    {}", heading(first, run)));
        for fact in run {
            entry(lines, fact);
        }
    }
}

/// How the placement was told.
fn told(basis: PlacementBasis) -> String {
    match basis {
        PlacementBasis::Stated => "stated with --placement".to_owned(),
        PlacementBasis::AllTracked { files } => {
            format!("git tracks all {files} files of the store")
        }
        PlacementBasis::NoneTracked { files } => {
            format!("git tracks none of the {files} files of the store")
        }
    }
}

/// The heading of `run`, the facts of the kind of `first`.
fn heading(first: &Fact, run: &[Fact]) -> String {
    let count = run.len();
    match first {
        Fact::Placement { placement, basis } => {
            format!(
                "placement = {}: {}",
                quoted(placement.as_str()),
                told(*basis)
            )
        }
        Fact::Renamed { .. } => {
            format!("{count} file(s) whose `name` differs from the stem, which is the name now:")
        }
        Fact::Reversed { .. } => format!("{count} `constrains` edge(s) written as `depends_on`:"),
        Fact::Normalized { .. } => format!("{count} anchor(s) respelled for format 0.5:"),
        Fact::Unnormalized { .. } => {
            format!(
                "{count} anchor symbol(s) kept as written, which format 0.5 may read otherwise:"
            )
        }
        Fact::History { .. } => {
            let entries: usize = run
                .iter()
                .filter_map(|fact| match fact {
                    Fact::History { entries, .. } => Some(*entries),
                    // A run holds facts of one kind.
                    Fact::Placement { .. }
                    | Fact::Renamed { .. }
                    | Fact::Reversed { .. }
                    | Fact::Normalized { .. }
                    | Fact::Unnormalized { .. }
                    | Fact::Pattern(_)
                    | Fact::Tags { .. } => None,
                })
                .sum();
            format!("history entries dropped: {entries}, from {count} decision(s):")
        }
        Fact::Pattern(_) => format!("{count} pattern(s) taken out of the graph:"),
        Fact::Tags { .. } => format!("{count} component(s) whose tags are taken out of the graph:"),
    }
}

fn entry(lines: &mut Vec<String>, fact: &Fact) {
    match fact {
        // The heading says it all.
        Fact::Placement { .. } => {}
        Fact::Renamed { file, name } => {
            lines.push(format!("      {file}: name = {}", quoted(name)))
        }
        Fact::Reversed { from, to } => {
            lines.push(format!(
                "      {to} depends_on {from}, for {from} constrains {to}"
            ));
        }
        Fact::Normalized { file, old, new } => lines.push(format!("      {file}: {old} -> {new}")),
        Fact::Unnormalized { file, anchor, why } => {
            lines.push(format!("      {file}: {anchor}: {why}"));
        }
        Fact::History { file, entries } => lines.push(format!("      {file}: {entries}")),
        Fact::Pattern(pattern) => pattern_lines(lines, pattern),
        Fact::Tags { node, tags } => lines.push(format!("      {node}: tags = {}", list(tags))),
    }
}

/// `pattern` as the TOML table it was, with its edges and tags as keys.
fn pattern_lines(lines: &mut Vec<String>, pattern: &PatternRecord) {
    lines.push(format!("      [{}]", pattern.name));
    for (key, text) in [
        ("name", &pattern.title),
        ("description", &pattern.description),
    ] {
        if let Some(text) = text {
            lines.push(format!("      {key} = {}", quoted(text)));
        }
    }
    for (key, names) in [
        ("tags", &pattern.tags),
        ("member_of", &pattern.member_of),
        ("applies_to", &pattern.applies_to),
    ] {
        if !names.is_empty() {
            lines.push(format!("      {key} = {}", list(names)));
        }
    }
}

/// `text` as a one-line TOML string.
fn quoted(text: &str) -> String {
    toml::Value::from(text).to_string()
}

fn list(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| quoted(name)).collect();
    format!("[{}]", quoted.join(", "))
}

/// A new directory beside the store for its backup: `.trurlic-backup-`
/// and the UTC time to the second in ISO 8601 basic format
/// (`20250601T103000`), which has no `:` for Windows to refuse, with `-2`,
/// `-3` and on appended while the name is taken, so a backup never lands in
/// an earlier one.
fn backup_dir(store: &Store) -> Result<PathBuf> {
    let now = Timestamp::now()?.civil();
    let name = format!(
        ".trurlic-backup-{:04}{:02}{:02}T{:02}{:02}{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    );
    let parent = store
        .root()
        .parent()
        .ok_or_else(|| Error::Validation("store root has no parent directory".into()))?;
    let mut path = parent.join(&name);
    for suffix in 2..=MAX_BACKUPS_PER_SECOND {
        match fs::create_dir(&path) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                path = parent.join(format!("{name}-{suffix}"));
            }
            created => return created.map(|()| path).map_err(Error::io(parent)),
        }
    }
    fs::create_dir(&path).map_err(Error::io(&path))?;
    Ok(path)
}

/// Copy the store at `root` to `backup` with the report `lines` in it, all
/// flushed to disk.
fn back_up(root: &Path, backup: &Path, lines: &[String]) -> Result<()> {
    copy_dir_recursive(root, backup)?;
    let path = backup.join(REPORT_FILE);
    // A new file: a store file of that name is in the copy already.
    let written = File::create_new(&path).and_then(|mut file| {
        for line in lines {
            writeln!(file, "{line}")?;
        }
        file.sync_all()
    });
    written.map_err(Error::io(&path))?;
    sync_dir(backup).map_err(Error::io(backup))?;
    let parent = backup.parent().unwrap_or(backup);
    sync_dir(parent).map_err(Error::io(parent))
}

/// Copy the directory `src` to `dst`, but for `.state/` and the graph's
/// repository `.git/`, every file flushed. The files of a directory are copied in parallel: each flush
/// waits on the disk, and a store holds hundreds of files.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(Error::io(dst))?;
    let mut files = Vec::new();
    for entry in fs::read_dir(src).map_err(Error::io(src))? {
        let entry = entry.map_err(Error::io(src))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(Error::io(&src_path))?;
        if ft.is_symlink() {
            // Skip symlinks rather than follow them out of the store tree; warn
            // so an incomplete backup is never silent.
            diag!(
                "warning: backup skipped symlink {}; copy its target if needed",
                src_path.display()
            );
        } else if ft.is_dir() {
            // Skip .state/, which holds the lock, temp files and the journal,
            // and .git/, whose history the migration's commit extends.
            if entry.file_name() != ".state" && entry.file_name() != ".git" {
                copy_dir_recursive(&src_path, &dst_path)?;
            }
        } else {
            files.push((src_path, dst_path));
        }
    }
    files.par_iter().try_for_each(|(from, to)| {
        fs::copy(from, to)
            .and_then(|_| File::open(to)?.sync_all())
            .map_err(Error::io(to))
    })?;
    sync_dir(dst).map_err(Error::io(dst))
}
