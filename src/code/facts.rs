//! The code facts of one tree, which the shell gathers and hands in as
//! data, and the resolution of an anchor against them.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use crate::store::Anchor;

use super::expand::expand;
use super::index::{Definition, FileIndex, FileKind, IndexFault};
use super::span::{Span, SpanHash};
use super::symbol::{Symbol, SymbolFault};

/// Why the shell has no bytes of a file it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadFault {
    /// Larger than a source file may be; `bytes` is its size when found.
    TooLarge { bytes: u64, max: u64 },
    /// A directory entry that is no regular file once its symlinks are
    /// followed: a FIFO, a device, a socket.
    NotAFile,
    /// The read failed: no permission, a symlink loop, an I/O error.
    Unreadable(io::ErrorKind),
}

impl fmt::Display for ReadFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, max } => {
                write!(f, "is {bytes} bytes, over the {max} a source file may hold")
            }
            Self::NotAFile => f.write_str("is not a regular file"),
            Self::Unreadable(kind) => write!(f, "cannot be read: {kind}"),
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
    /// A file of a kind with a resolver: its index, or why it has none.
    File {
        kind: FileKind,
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

/// Where a resolved anchor points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A file-only anchor on a file: no span, no hash.
    File,
    /// A file-only anchor on a directory.
    Directory,
    /// The one definition the symbol names.
    Symbol { span: Span, hash: SpanHash },
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => f.write_str("the file"),
            Self::Directory => f.write_str("the directory"),
            Self::Symbol { span, hash } => write!(f, "{span}, {hash}"),
        }
    }
}

/// What an anchor names in a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Resolved(Target),
    /// Nothing at the anchor's path.
    FileMissing,
    /// The file defines nothing the symbol names.
    SymbolMissing,
    /// The file defines this many things the symbol names.
    Ambiguous(usize),
    /// A symbol on a directory, which defines nothing.
    SymbolOnDirectory,
    /// A symbol on a file of a kind no resolver reads.
    NoResolver,
    /// A symbol on a file whose index cannot be built.
    Unindexable(FileFault),
    /// A symbol that is not in the grammar of its file's kind.
    Malformed(SymbolFault),
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolved(target) => write!(f, "resolves to {target}"),
            Self::FileMissing => f.write_str("nothing is at its path"),
            Self::SymbolMissing => f.write_str("its file defines nothing its symbol names"),
            Self::Ambiguous(count) => write!(f, "its symbol names {count} definitions"),
            Self::SymbolOnDirectory => f.write_str("its symbol is on a directory"),
            Self::NoResolver => f.write_str("its symbol is on a kind of file no resolver reads"),
            Self::Unindexable(fault) => write!(f, "its file {fault}"),
            Self::Malformed(fault) => write!(f, "its symbol {fault}"),
        }
    }
}

/// The entries of one tree by path, `/`-separated from its root, and the
/// definitions the macro invocations of its Rust files add.
#[derive(Debug)]
pub(crate) struct CodeFacts {
    entries: BTreeMap<String, Entry>,
    expanded: BTreeMap<String, Vec<Definition>>,
}

impl CodeFacts {
    /// The facts of `entries`, which hold every path an anchor to be
    /// resolved names, and every Rust file and `Cargo.toml` of the tree,
    /// whose macros a file can invoke: a path absent from them is missing.
    pub(crate) fn new(entries: BTreeMap<String, Entry>) -> Self {
        let indexed = entries
            .iter()
            .filter_map(|(path, entry)| match entry {
                Entry::File {
                    kind,
                    index: Ok(index),
                } => Some((path.as_str(), (*kind, index))),
                Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed => None,
            })
            .collect();
        let expanded = expand(&indexed)
            .into_iter()
            .map(|(path, definitions)| (path.to_owned(), definitions))
            .collect();
        Self { entries, expanded }
    }

    /// What `anchor` names in this tree.
    pub(crate) fn resolve(&self, anchor: &Anchor) -> Resolution {
        let Some(entry) = self.entries.get(anchor.file()) else {
            return Resolution::FileMissing;
        };
        let Some(symbol) = anchor.symbol() else {
            return Resolution::Resolved(match entry {
                Entry::Directory => Target::Directory,
                Entry::Unindexed | Entry::File { .. } => Target::File,
            });
        };
        match entry {
            Entry::Directory => Resolution::SymbolOnDirectory,
            Entry::Unindexed => Resolution::NoResolver,
            Entry::File {
                index: Err(fault), ..
            } => Resolution::Unindexable(*fault),
            Entry::File {
                kind,
                index: Ok(index),
            } => match Symbol::parse(symbol, kind.syntax()) {
                Ok(symbol) => self.find(anchor.file(), index, &symbol),
                Err(fault) => Resolution::Malformed(fault),
            },
        }
    }

    /// The one definition of `file` that `symbol` names: the one it names,
    /// or among several the one whose whole path it is.
    fn find(&self, file: &str, index: &FileIndex, symbol: &Symbol) -> Resolution {
        let expanded = self.expanded.get(file).into_iter().flatten();
        let named: Vec<&Definition> = index
            .definitions
            .iter()
            .chain(expanded)
            .filter(|definition| symbol.names(&definition.path))
            .collect();
        let mut whole = named.iter().filter(|d| symbol.is_path_of(&d.path));
        let found = match (named.as_slice(), whole.next(), whole.next()) {
            ([], _, _) => return Resolution::SymbolMissing,
            ([one], _, _) | (_, Some(one), None) => one,
            (several, _, _) => return Resolution::Ambiguous(several.len()),
        };
        Resolution::Resolved(Target::Symbol {
            span: found.span,
            hash: found.hash,
        })
    }
}
