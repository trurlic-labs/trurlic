//! `trurlic migrate`: bring a store in an older format to the current one.
//!
//! The store plans the migration (`Store::plan_migration`) and commits it in
//! one journaled commit; this command takes the lock, keeps a copy of the
//! old files beside `.trurlic/`, and prints what each step dropped.

use std::fs;
use std::path::Path;

use crate::console::{diag, out};
use crate::store::{FORMAT_VERSION, Loss, MigrationReport, Store, Timestamp};
use crate::{Error, Result};

use super::DryRun;

/// Migrate the store above `cwd`. `DryRun::Yes` prints the report and the
/// files the migration would rewrite, and writes nothing.
pub(crate) fn migrate(cwd: &Path, dry_run: DryRun) -> Result<()> {
    let store = Store::discover(cwd)?;
    if dry_run == DryRun::Yes {
        return match store.preview_migration()? {
            Some(migration) => print_report(&migration.report, DryRun::Yes),
            None => up_to_date(),
        };
    }

    let lock = store.lock()?;
    // Planned under the lock, so no write lands between the plan and its
    // commit, and a store another process migrated meanwhile is current.
    let Some(migration) = store.plan_migration(&lock)? else {
        return up_to_date();
    };
    // Copied under the lock, so no concurrent write can tear the copy.
    let backup_name = backup_name()?;
    let backup_path = store
        .root()
        .parent()
        .ok_or_else(|| Error::Validation("store root has no parent directory".into()))?
        .join(&backup_name);
    if let Err(e) = copy_dir_recursive(store.root(), &backup_path) {
        let _ = fs::remove_dir_all(&backup_path);
        return Err(e);
    }
    let report = store.commit_migration(&lock, migration)?;
    drop(lock);

    print_report(&report, DryRun::No)?;
    out!("backup: {backup_name}/")?;
    Ok(())
}

fn up_to_date() -> Result<()> {
    out!("Already up to date (format version {FORMAT_VERSION}).")?;
    Ok(())
}

/// Each step with what it dropped, then the files written and removed, all
/// in the order the migration met them.
fn print_report(report: &MigrationReport, dry_run: DryRun) -> Result<()> {
    let (migrate, rewrite, remove) = match dry_run {
        DryRun::Yes => ("Would migrate", "Would rewrite", "Would remove"),
        DryRun::No => ("Migrated", "Rewrote", "Removed"),
    };
    out!(
        "{migrate} .trurlic/ from format {} to {FORMAT_VERSION}.",
        report.from
    )?;
    for step in &report.steps {
        let title = format!("{} -> {} ({})", step.from, step.to, step.change);
        print_losses(&title, &step.losses)?;
    }
    out!("{rewrite} {} file(s):", report.rewritten.len())?;
    for file in &report.rewritten {
        out!("  {file}")?;
    }
    if !report.removed.is_empty() {
        out!("{remove} {} file(s):", report.removed.len())?;
        for file in &report.removed {
            out!("  {file}")?;
        }
    }
    Ok(())
}

fn print_losses(title: &str, losses: &[Loss]) -> Result<()> {
    if losses.is_empty() {
        out!("  {title}: nothing dropped")?;
        return Ok(());
    }
    out!("  {title}: dropped {}", losses.len())?;
    for loss in losses {
        out!("    {}: {} = {}", loss.file, loss.field, loss.value)?;
    }
    Ok(())
}

/// `.trurlic-backup-` and the UTC time to the second in ISO 8601 basic
/// format (`20250601T103000`), which has no `:` for Windows to refuse.
fn backup_name() -> Result<String> {
    let now = Timestamp::now()?.civil();
    Ok(format!(
        ".trurlic-backup-{:04}{:02}{:02}T{:02}{:02}{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    ))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(Error::io(dst))?;
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
            continue;
        }
        if ft.is_dir() {
            // Skip .state/: it holds the lock, temp files and the journal.
            if entry.file_name() == ".state" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(Error::io(&dst_path))?;
        }
    }
    Ok(())
}
