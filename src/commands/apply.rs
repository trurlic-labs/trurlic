use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::Result;
use crate::console::{diag, out};
use crate::mcp::twin::{self, Answer, Duplicates, LINT_MORE, MAX_DOCUMENT_BYTES, ToolError};
use crate::store::{Applied, Done, Field, OpKind, Warning};

use super::{Format, answer};

/// `trurlic apply <file>`: the `apply` tool on the batch `file` holds, the
/// JSON object the tool takes as its arguments, or on stdin for `-`. It
/// refuses as the tool does, and a batch it writes leaves the bytes the
/// tool's would.
pub(crate) fn apply(cwd: &Path, file: &Path, format: Format) -> std::result::Result<(), ToolError> {
    answer(run(cwd, file), format, print)
}

/// The batch is read before the store is found, as a call's request is,
/// and parsed after it, as a call's arguments are.
fn run(cwd: &Path, file: &Path) -> std::result::Result<Answer, ToolError> {
    let arguments = twin::arguments(&read(file)?)?;
    twin::apply(cwd, &arguments)
}

/// The bytes of `file`, or of stdin for `-`, refused past the size of one
/// MCP message. A file that cannot be read holds no batch, as one that is
/// not JSON holds none: the refusal is the batch's, not the store's. A
/// pipe is read as a file is (`trurlic apply <(...)`), so a FIFO is not
/// refused: no lock is held while it is read.
fn read(file: &Path) -> std::result::Result<Vec<u8>, ToolError> {
    let limit = u64::try_from(MAX_DOCUMENT_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut document = Vec::new();
    let read = if file == Path::new("-") {
        io::stdin().lock().take(limit).read_to_end(&mut document)
    } else {
        File::open(file).and_then(|opened| opened.take(limit).read_to_end(&mut document))
    };
    let shown = if file == Path::new("-") {
        Path::new("stdin")
    } else {
        file
    }
    .display();
    if let Err(e) = read {
        return Err(ToolError::bad_document(format!(
            "{shown}: cannot read the batch: {e}"
        )));
    }
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(ToolError::bad_document(format!(
            "{shown}: a batch is at most {MAX_DOCUMENT_BYTES} bytes"
        )));
    }
    Ok(document)
}

/// One line per op, then the files; each warning on stderr, with the
/// message of its pattern, then the likely duplicates, each with the count
/// of those not listed.
fn print(answer: &Answer) -> Result<()> {
    let applied = &answer.applied;
    for done in &applied.ops {
        out!("{}", line(done))?;
    }
    for warning in &applied.warnings {
        diag!("warning: {}", said(applied, warning));
    }
    let unlisted = applied.ops.iter().map(|done| done.more_warnings);
    let unlisted = unlisted.fold(applied.more_origin_warnings, usize::saturating_add);
    if unlisted > 0 {
        diag!("warning: and {unlisted} more, by the same patterns of `[quality]`");
    }
    for duplicates in &answer.duplicates {
        diag!("warning: {}", likely(duplicates));
    }
    if answer.more_duplicates > 0 {
        diag!(
            "warning: and {} more decision(s) with likely duplicates: `trurlic status --section \
             lint` ranks the pairs, and {LINT_MORE}",
            answer.more_duplicates
        );
    }
    out!(
        "{} file(s) written, {} removed",
        applied.written,
        applied.removed
    )
}

/// A decision's likely duplicates as a terminal shows them.
fn likely(duplicates: &Duplicates) -> String {
    let nearest: Vec<String> = duplicates
        .nearest
        .iter()
        .map(|nearest| format!("`{}` ({})", nearest.decision, nearest.score))
        .collect();
    format!(
        "`{}` is a likely duplicate of {}",
        duplicates.decision,
        nearest.join(", ")
    )
}

/// A warning as a terminal shows it: the op, the argument and what the
/// pattern says.
fn said(applied: &Applied, warning: &Warning) -> String {
    let op = warning.op.and_then(|index| applied.ops.get(index));
    let at = op.map_or_else(String::new, |done| {
        format!("{} `{}`, ", done.op.as_str(), done.name)
    });
    let message = applied
        .patterns
        .iter()
        .find(|note| note.index == warning.pattern)
        .map_or("", |note| note.message.as_str());
    format!(
        "{at}`{}`: matches `[quality] patterns[{}]` at byte {}: {message}",
        warning.field, warning.pattern, warning.at
    )
}

fn line(done: &Done) -> String {
    let name = &done.name;
    let mut line = match done.op {
        OpKind::Record => format!("recorded `{name}`"),
        OpKind::Revise => format!("revised `{name}`: {}", fields(&done.changed)),
        OpKind::Rename => format!("renamed `{name}` to `{}`", shown(done.to.as_ref())),
        OpKind::Merge => format!("merged `{}` into `{name}`", shown(done.absorbed.as_ref())),
        OpKind::Remove => match &done.repoint_to {
            Some(heir) => format!("removed `{name}`, its dependents now on `{heir}`"),
            None => format!("removed `{name}`"),
        },
        OpKind::AddComponent => format!("added component `{name}`"),
        OpKind::ReviseComponent => {
            format!("revised component `{name}`: {}", fields(&done.changed))
        }
        OpKind::RenameComponent => {
            format!(
                "renamed component `{name}` to `{}`",
                shown(done.to.as_ref())
            )
        }
        OpKind::RemoveComponent => format!("removed component `{name}`"),
    };
    if let Some(status) = done.status {
        line.push_str(&format!(", now {}", status.as_str()));
    }
    if done.rewrote > 0 {
        line.push_str(&format!(" ({} other file(s) rewritten)", done.rewrote));
    }
    line
}

fn fields(changed: &[Field]) -> String {
    let names: Vec<&str> = changed.iter().map(|field| field.as_str()).collect();
    names.join(", ")
}

fn shown(name: Option<&String>) -> &str {
    name.map_or("", String::as_str)
}
