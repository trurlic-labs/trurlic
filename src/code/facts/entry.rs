//! What the shell found at each path of a tree, handed to the index's
//! facts as data: a directory, a file of a kind no resolver reads, or a
//! file with its lines and its index or why it has none.

use std::fmt;
use std::io::ErrorKind;

use crate::code::index::{FileIndex, FileKind, IndexFault};

/// Why the shell has no bytes of a file it found, or no index of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadFault {
    /// Larger than a source file may be; `bytes` is its size when found.
    TooLarge { bytes: u64, max: u64 },
    /// A directory entry that is no regular file once its symlinks are
    /// followed: a FIFO, a device, a socket.
    NotAFile,
    /// The read failed: no permission, a symlink loop, an I/O error.
    Unreadable(ErrorKind),
    /// The system started no thread to index it on.
    NoThread(ErrorKind),
}

impl fmt::Display for ReadFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, max } => {
                write!(f, "is {bytes} bytes, over the {max} a source file may hold")
            }
            Self::NotAFile => f.write_str("is not a regular file"),
            Self::Unreadable(kind) => write!(f, "cannot be read: {kind}"),
            Self::NoThread(kind) => write!(
                f,
                "is not indexed: no thread to index it on started: {kind}"
            ),
        }
    }
}

/// What the shell found at a path of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Directory,
    /// A file of a kind with no resolver: it exists, and nothing more is
    /// known.
    Unindexed,
    /// A file of a kind with a resolver: its lines, and its index or why
    /// it has none.
    File {
        kind: FileKind,
        /// Its [`line_count`](crate::code::index::line_count); `None` when
        /// the shell read none of its bytes.
        lines: Option<usize>,
        index: Result<FileIndex, FileFault>,
    },
}

/// Why a file of a kind with a resolver has no index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileFault {
    Read(ReadFault),
    Index(IndexFault),
}

impl fmt::Display for FileFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(fault) => fault.fmt(f),
            Self::Index(fault) => fault.fmt(f),
        }
    }
}
