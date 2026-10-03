//! Terminal I/O: command output on stdout, diagnostics on stderr, and the
//! one confirmation prompt.
//!
//! Apart from the MCP transport, which owns stdout under `serve`, these
//! functions are the only code that touches the standard streams: one stray
//! line on stdout corrupts the JSON-RPC stream.

use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};

/// Write `line` and a newline to stdout. Unlike `println!`, a closed stdout,
/// such as a pipe whose reader exited, is an error, not a panic.
pub(crate) fn write_out(line: fmt::Arguments<'_>) -> io::Result<()> {
    writeln!(io::stdout().lock(), "{line}")
}

/// Write `line` and a newline to stderr. A failed write is dropped: stderr
/// is where its failure would be reported.
pub(crate) fn write_diag(line: fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}

pub(crate) fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Ask `question` on stderr and read the answer from stdin: `true` only
/// for `y` or `Y`.
pub(crate) fn confirm(question: &str) -> io::Result<bool> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{question} [y/N] ")?;
    stderr.flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

/// `println!` for command output, returning `io::Result<()>`.
macro_rules! out {
    () => {
        $crate::console::write_out(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::console::write_out(format_args!($($arg)*))
    };
}

/// `eprintln!` for diagnostics.
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::console::write_diag(format_args!($($arg)*))
    };
}

pub(crate) use {diag, out};
