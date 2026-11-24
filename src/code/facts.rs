//! The code facts of one tree, which the shell gathers and hands in as
//! data, and the resolution of an anchor against them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::ErrorKind;

use crate::store::Anchor;

use super::crates::Crates;
use super::definitions::Definitions;
use super::expand::{Arms, Expansion, Files, Held, Lookup, expand, expand_file};
use super::index::{Definition, FileIndex, FileKind, IndexFault};
use super::kinds::Role;
use super::roles::test_files;
use super::scope::Scopes;
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
    Unreadable(ErrorKind),
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
    /// The paths the walk took, which a count over the tree reads.
    walked: BTreeSet<String>,
}

/// The kind and index of `entry`, when it is a file with an index.
fn indexed(entry: &Entry) -> Option<(FileKind, &FileIndex)> {
    match entry {
        Entry::File {
            kind,
            index: Ok(index),
        } => Some((*kind, index)),
        Entry::File { index: Err(_), .. } | Entry::Directory | Entry::Unindexed => None,
    }
}

/// What the invocations of each Rust file of `outside`, files no walk
/// took, add: each is read against its own macros, which no other file
/// sees, then its crate's shared arms.
fn expand_outside(
    outside: &Files<'_>,
    crates: &Crates<'_>,
) -> BTreeMap<String, Result<Expansion, IndexFault>> {
    let mut tables: BTreeMap<&str, Arms<'_>> = BTreeMap::new();
    let mut expanded = BTreeMap::new();
    for (&path, (kind, index)) in outside {
        if *kind != FileKind::Rust || index.rust.invocations.is_empty() {
            continue;
        }
        let owner = crates.crate_of(path);
        let shared = tables.entry(owner).or_insert_with(|| crates.arms_of(path));
        let mut own = Arms::default();
        for found in &index.rust.macros {
            own.add(Held {
                found,
                owner,
                exported: false,
            });
        }
        own.resolve(|own, from, hand_off| {
            let name = hand_off.macro_name.as_str();
            match hand_off.from_crate {
                true => own.held(name).or_else(|| crates.held_in(from.owner, name)),
                false => own.held(name).or_else(|| shared.held(name)),
            }
        });
        let lookup = Lookup {
            own: (!index.rust.macros.is_empty()).then_some(&own),
            shared,
        };
        let expansion = expand_file(index, lookup);
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
            .filter_map(|(path, entry)| Some((path.as_str(), indexed(entry)?)))
            .collect();
        let (tree, outside): (Files<'_>, Files<'_>) = indexed
            .iter()
            .map(|(path, file)| (*path, *file))
            .partition(|(path, _)| walked.contains(*path));
        let crates = Crates::of(&tree, &outside);
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
            // clone: the facts keep which paths the walk took.
            walked: walked.clone(),
        }
    }

    /// The facts of `read`, paths of another tree read there with what was
    /// found, `None` where nothing was: each read against this tree's
    /// macros and crates, which stand in for that tree's, so no walk of it
    /// is needed. Only the read paths are in the facts, and none is walked.
    pub(crate) fn beside(&self, read: &BTreeMap<String, Option<Entry>>) -> Self {
        let entries: BTreeMap<String, Entry> = read
            .iter()
            // clone: the facts own what was read; the paths also mask this tree's.
            .filter_map(|(path, entry)| Some((path.clone(), entry.clone()?)))
            .collect();
        let tree: Files<'_> = self
            .entries
            .iter()
            .filter(|(path, _)| self.walked.contains(*path) && !read.contains_key(*path))
            .filter_map(|(path, entry)| Some((path.as_str(), indexed(entry)?)))
            .collect();
        let outside: Files<'_> = entries
            .iter()
            .filter_map(|(path, entry)| Some((path.as_str(), indexed(entry)?)))
            .collect();
        let crates = Crates::of(&tree, &outside);
        let expanded = expand_outside(&outside, &crates);
        let test_files = test_files(&tree, &outside, &crates)
            .into_iter()
            .filter(|path| entries.contains_key(*path))
            .map(str::to_owned)
            .collect();
        Self {
            entries,
            expanded,
            test_files,
            walked: BTreeSet::new(),
        }
    }

    /// For each of `anchors` whose symbol is one plain name on a code file,
    /// how many walked files of that file's kind hold a definition of that
    /// name at any depth, those macro invocations add included: whether
    /// the name alone picks out a place. A file only an anchor brought in
    /// is not counted. One pass over the tree serves them all, and none is
    /// made when no anchor is one name.
    pub(crate) fn spread<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a Anchor>,
    ) -> BTreeMap<&'a Anchor, usize> {
        let wanted: BTreeMap<&Anchor, (FileKind, String)> = anchors
            .into_iter()
            .filter_map(|anchor| Some((anchor, self.lone_name(anchor)?)))
            .collect();
        if wanted.is_empty() {
            return BTreeMap::new();
        }
        let names: BTreeSet<(FileKind, &str)> = wanted
            .values()
            .map(|(kind, name)| (*kind, name.as_str()))
            .collect();
        let mut definers: BTreeMap<(FileKind, &str), BTreeSet<&str>> = BTreeMap::new();
        for (path, scopes, definition, _) in self.definitions() {
            let Some(Entry::File { kind, .. }) = self.entries.get(path) else {
                continue;
            };
            let Some(Segment::Name(name)) = scopes.path_from_end(definition.scope).next() else {
                continue;
            };
            if self.walked.contains(path) && names.contains(&(*kind, name.as_str())) {
                definers
                    .entry((*kind, name.as_str()))
                    .or_default()
                    .insert(path);
            }
        }
        wanted
            .iter()
            .map(|(&anchor, (kind, name))| {
                let files = definers.get(&(*kind, name.as_str()));
                (anchor, files.map_or(0, BTreeSet::len))
            })
            .collect()
    }

    /// The one plain name `anchor`'s symbol is, with its file's kind, when
    /// that kind is code and the file has an index.
    fn lone_name(&self, anchor: &Anchor) -> Option<(FileKind, String)> {
        let Some(Entry::File { kind, index: Ok(_) }) = self.entries.get(anchor.file()) else {
            return None;
        };
        let symbol = Symbol::parse(anchor.symbol()?, kind.syntax()).ok()?;
        let [Segment::Name(name)] = symbol.segments() else {
            return None;
        };
        // clone: the name outlives the symbol parsed here.
        kind.is_code().then(|| (*kind, name.clone()))
    }

    /// Whether a macro invocation in item position in `anchor`'s file
    /// spells the last name of its symbol among its first tokens: a type a
    /// macro the facts do not hold defines may be there.
    pub(crate) fn spells_in_invocation(&self, anchor: &Anchor) -> bool {
        let Some(Entry::File {
            kind,
            index: Ok(index),
        }) = self.entries.get(anchor.file())
        else {
            return false;
        };
        let symbol = anchor
            .symbol()
            .and_then(|symbol| Symbol::parse(symbol, kind.syntax()).ok());
        let Some(Segment::Name(name)) = symbol.as_ref().and_then(|s| s.segments().last()) else {
            return false;
        };
        index
            .rust
            .invocations
            .iter()
            .any(|invocation| invocation.tokens.iter().any(|token| token == name))
    }

    /// Every definition of the tree's indexed files, those its macro
    /// invocations add included, with its file, the scopes its path is
    /// walked through, and its role in the tree: test code when its file
    /// is.
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
            (Some(set), 0) => match set.split_first() {
                Some((first, rest)) => {
                    Resolution::Resolved(Target::Symbol(Definitions::of(first, rest)))
                }
                None => Resolution::SymbolMissing,
            },
            (Some(_), others) => Resolution::Ambiguous(others + 1),
        }
    }
}
