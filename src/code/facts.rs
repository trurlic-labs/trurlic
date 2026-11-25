//! The code facts of one tree, which the shell gathers and hands in as
//! data, and the resolution of an anchor against them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;

use crate::store::Anchor;

use super::expand::{Arms, Crates, Expansion, Files, MANIFEST, expand, expand_file, file_name};
use super::index::{Definition, FileIndex, FileKind, IndexFault};
use super::kinds::Role;
use super::roles::test_files;
use super::scope::Scopes;
use super::span::{Span, SpanHash};
use super::symbol::{Segment, Symbol, SymbolFault};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A file-only anchor on a file: no span, no hash.
    File,
    /// A file-only anchor on a directory.
    Directory,
    /// The definitions of the one path the symbol names.
    Symbol(Definitions),
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => f.write_str("the file"),
            Self::Directory => f.write_str("the directory"),
            Self::Symbol(definitions) => definitions.fmt(f),
        }
    }
}

/// The definitions of one path, one at least: their spans in file order,
/// a span two of them share listed once, and one hash over them
/// ([`SpanHash::of_members`]). The cfg twins of an item, a field and its
/// getter of one name, a key an array's elements each hold: each is one
/// thing an anchor names, whose every span a change may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Definitions {
    first: Span,
    /// The spans after the first, in file order.
    more: Vec<Span>,
    hash: SpanHash,
}

impl Definitions {
    /// The set of `members`, each a definition's span and hash; `None`
    /// for none.
    fn of(members: impl Iterator<Item = (Span, SpanHash)>) -> Option<Self> {
        let mut members: Vec<(Span, SpanHash)> = members.collect();
        members.sort_unstable();
        members.dedup_by_key(|(span, _)| *span);
        let hashes: Vec<SpanHash> = members.iter().map(|(_, hash)| *hash).collect();
        let mut spans = members.into_iter().map(|(span, _)| span);
        Some(Self {
            first: spans.next()?,
            more: spans.collect(),
            hash: SpanHash::of_members(&hashes),
        })
    }

    /// Each span, in file order.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the anchor findings and the diff review are the first callers"
        )
    )]
    pub(crate) fn spans(&self) -> impl Iterator<Item = &Span> {
        std::iter::once(&self.first).chain(&self.more)
    }

    /// The hash of the set: one definition's own, or that of several.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the anchor findings and the diff review are the first callers"
        )
    )]
    pub(crate) const fn hash(&self) -> SpanHash {
        self.hash
    }
}

impl fmt::Display for Definitions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.more.is_empty() {
            write!(f, "{} definitions: ", self.more.len() + 1)?;
        }
        write!(f, "{}", self.first)?;
        for span in &self.more {
            write!(f, "; {span}")?;
        }
        write!(f, ", {}", self.hash)
    }
}

/// What an anchor names in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution {
    Resolved(Target),
    /// Nothing at the anchor's path.
    FileMissing,
    /// The file defines nothing the symbol names.
    SymbolMissing,
    /// The symbol names definitions of this many paths, two or more,
    /// which a fuller spelling tells apart.
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
            Self::Ambiguous(paths) => write!(f, "its symbol names definitions of {paths} paths"),
            Self::SymbolOnDirectory => f.write_str("its symbol is on a directory"),
            Self::NoResolver => f.write_str("its symbol is on a kind of file no resolver reads"),
            Self::Unindexable(fault) => write!(f, "its file {fault}"),
            Self::Malformed(fault) => write!(f, "its symbol {fault}"),
        }
    }
}

/// The entries of one tree by path, `/`-separated from its root, the
/// definitions the macro invocations of its Rust files add, and the Rust
/// files that are test code whole.
///
/// The tree's own facts, its macros, crates and test files, come from the
/// files the walk takes alone, built once. A file indexed only because an
/// anchor names it is read against those, its own macros and the
/// manifests on its own path, which govern no walked file, and adds
/// nothing any other file is read against, so what an anchor resolves
/// to never depends on which other anchors there are.
#[derive(Debug)]
pub(crate) struct CodeFacts {
    entries: BTreeMap<String, Entry>,
    /// What each Rust file's invocations add, or why they cannot.
    expanded: BTreeMap<String, Result<Expansion, IndexFault>>,
    test_files: BTreeSet<String>,
}

/// What the invocations of each Rust file of `outside`, files no walk
/// took, add: each is read against its crate's arms, and its own macros,
/// which no other file sees.
fn expand_outside(
    outside: &Files<'_>,
    crates: &Crates<'_>,
) -> BTreeMap<String, Result<Expansion, IndexFault>> {
    let mut tables: BTreeMap<&str, Arms<'_>> = BTreeMap::new();
    let mut expanded = BTreeMap::new();
    for (&path, (kind, index)) in outside {
        if *kind != FileKind::Rust || index.invocations.is_empty() {
            continue;
        }
        let owner = crates.crate_of(path);
        let expansion = match index.macros.is_empty() {
            true => expand_file(
                index,
                tables
                    .entry(owner)
                    .or_insert_with(|| crates.arms_of(owner, None)),
            ),
            false => expand_file(index, &crates.arms_of(owner, Some(&index.macros))),
        };
        if let Some(expansion) = expansion.transpose() {
            expanded.insert(path.to_owned(), expansion);
        }
    }
    expanded
}

impl CodeFacts {
    /// The facts of `entries`, which hold every path an anchor to be
    /// resolved names, every file of `walked` (the tree's Rust files and
    /// manifests, whose macros a file can invoke) and the manifests above
    /// each anchored Rust file outside them: a path absent from them is
    /// missing.
    pub(crate) fn new(entries: BTreeMap<String, Entry>, walked: &BTreeSet<String>) -> Self {
        let indexed: Files<'_> = entries
            .iter()
            .filter_map(|(path, entry)| match entry {
                Entry::File {
                    kind,
                    index: Ok(index),
                } => Some((path.as_str(), (*kind, index))),
                Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed => None,
            })
            .collect();
        let (tree, alone): (Files<'_>, Files<'_>) = indexed
            .iter()
            .map(|(path, file)| (*path, *file))
            .partition(|(path, _)| walked.contains(*path));
        let (manifests, outside): (Files<'_>, Files<'_>) = alone
            .into_iter()
            .partition(|(path, _)| file_name(path) == MANIFEST);
        let crates = Crates::of(&tree, &manifests);
        let mut expanded: BTreeMap<String, Result<Expansion, IndexFault>> = expand(&tree, &crates)
            .into_iter()
            .map(|(path, expansion)| (path.to_owned(), expansion))
            .collect();
        expanded.extend(expand_outside(&outside, &crates));
        let test_files: BTreeSet<String> = test_files(&tree, &outside, &crates)
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self {
            entries,
            expanded,
            test_files,
        }
    }

    /// Every definition of the tree's indexed files, those its macro
    /// invocations add included, with its file, the scopes its path is
    /// walked through, and its role in the tree: test code when its file
    /// is.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the counts of definitions in and out of anchored spans are the first callers"
        )
    )]
    pub(crate) fn definitions(
        &self,
    ) -> impl Iterator<Item = (&str, Scopes<'_>, &Definition, Role)> {
        self.entries.iter().flat_map(move |(path, entry)| {
            let expansion = self.expanded.get(path);
            // A file whose expansion failed has no index, as it resolves.
            let index = match (entry, expansion) {
                (_, Some(Err(_))) => None,
                (
                    Entry::File {
                        index: Ok(index), ..
                    },
                    None | Some(Ok(_)),
                ) => Some(index),
                (Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed, _) => {
                    None
                }
            };
            let expansion = expansion.and_then(|expansion| expansion.as_ref().ok());
            let file_role = match self.test_files.contains(path) {
                true => Role::Test,
                false => Role::Code,
            };
            let scopes = index.map(|index| self.scopes(index, expansion));
            let added = expansion.map_or(&[][..], |expansion| &expansion.definitions);
            index
                .map_or(&[][..], |index| &index.definitions)
                .iter()
                .chain(added)
                .filter_map(move |definition| {
                    Some((
                        path.as_str(),
                        scopes?,
                        definition,
                        definition.role.or(file_role),
                    ))
                })
        })
    }

    /// The scopes the paths of `index`'s file are walked through.
    fn scopes<'a>(&self, index: &'a FileIndex, expansion: Option<&'a Expansion>) -> Scopes<'a> {
        Scopes {
            file: &index.scopes,
            added: expansion.map_or(&[][..], |expansion| &expansion.scopes),
        }
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

    /// The definitions of `file` that `symbol` names: those of the one
    /// path among them, its whole path winning over paths it ends; with
    /// definitions of several paths, `Ambiguous`.
    fn find(&self, file: &str, index: &FileIndex, symbol: &Symbol) -> Resolution {
        let expansion = match self.expanded.get(file) {
            Some(Err(fault)) => return Resolution::Unindexable(FileFault::Index(*fault)),
            Some(Ok(expansion)) => Some(expansion),
            None => None,
        };
        let scopes = self.scopes(index, expansion);
        let added = expansion.map_or(&[][..], |expansion| &expansion.definitions);
        let named: Vec<&Definition> = index
            .definitions
            .iter()
            .chain(added)
            .filter(|definition| symbol.admits(definition.kind))
            .filter(|definition| symbol.names(scopes.path_from_end(definition.scope)))
            .collect();
        let whole: Vec<&Definition> = named
            .iter()
            .copied()
            .filter(|d| symbol.is_path_of(scopes.path_from_end(d.scope)))
            .collect();
        let chosen = match whole.is_empty() {
            true => named,
            false => whole,
        };
        let mut paths: BTreeMap<Vec<&Segment>, Vec<&Definition>> = BTreeMap::new();
        for definition in chosen {
            let path = scopes.path_from_end(definition.scope).collect();
            paths.entry(path).or_default().push(definition);
        }
        let mut sets = paths.into_values();
        match (sets.next(), sets.len()) {
            (None, _) => Resolution::SymbolMissing,
            (Some(set), 0) => Definitions::of(set.iter().map(|d| (d.span, d.hash)))
                .map_or(Resolution::SymbolMissing, |found| {
                    Resolution::Resolved(Target::Symbol(found))
                }),
            (Some(_), others) => Resolution::Ambiguous(others + 1),
        }
    }
}
