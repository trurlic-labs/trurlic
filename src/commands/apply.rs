use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::console::out;
use crate::discovery::discover;
use crate::mcp::twin::{self, MAX_DOCUMENT_BYTES, ToolError};
use crate::store::{Applied, Done, Field, OpKind};
use crate::{Error, Result};

use super::Format;

/// `trurlic apply <file>`: the `apply` tool on the batch `file` holds, the
/// JSON object the tool takes as its arguments, or on stdin for `-`. It
/// refuses as the tool does, and a batch it writes leaves the bytes the
/// tool's would.
pub(crate) fn apply(cwd: &Path, file: &Path, format: Format) -> std::result::Result<(), ToolError> {
    let applied = run(cwd, file);
    match (applied, format) {
        (Ok(applied), Format::Json) => Ok(out!("{}", twin::result_json(&applied))?),
        (Ok(applied), Format::Text) => Ok(print(&applied)?),
        (Err(refusal), Format::Json) => {
            out!("{}", twin::refusal_json(&refusal))?;
            Err(refusal)
        }
        (Err(refusal), Format::Text) => Err(refusal),
    }
}

/// The batch is read before the store is found, as a call's request is,
/// and parsed after it, as a call's arguments are.
fn run(cwd: &Path, file: &Path) -> std::result::Result<Applied, ToolError> {
    let arguments = twin::arguments(&read(file)?)?;
    let store = discover(cwd)?;
    twin::apply(&store, &arguments)
}

/// The bytes of `file`, or of stdin for `-`, refused past the size of one
/// MCP message.
fn read(file: &Path) -> std::result::Result<Vec<u8>, ToolError> {
    let limit = u64::try_from(MAX_DOCUMENT_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut document = Vec::new();
    if file == Path::new("-") {
        io::stdin()
            .lock()
            .take(limit)
            .read_to_end(&mut document)
            .map_err(Error::system("read the batch from stdin"))?;
    } else {
        File::open(file)
            .and_then(|opened| opened.take(limit).read_to_end(&mut document))
            .map_err(Error::io(file))?;
    }
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(ToolError::bad_document(format!(
            "{}: a batch is at most {MAX_DOCUMENT_BYTES} bytes",
            file.display()
        )));
    }
    Ok(document)
}

/// One line per op, then the files.
fn print(applied: &Applied) -> Result<()> {
    for done in &applied.ops {
        out!("{}", line(done))?;
    }
    out!(
        "{} file(s) written, {} removed",
        applied.written,
        applied.removed
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
