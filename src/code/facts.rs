//! The code facts of one tree, which the shell gathers and hands in as
//! data, and the resolution of an anchor against them.

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::fmt;
use std::io::ErrorKind;

use crate::store::Anchor;

use super::crates::Crates;
use super::definitions::Definitions;
use super::expand::{Expansion, Files, expand};
use super::index::{Definition, Element, FileIndex, FileKind, IndexFault};
use super::kinds::{DefinitionKind, Role};
use super::roles::{role_of_place, target_of, test_files};
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

impl Resolution {
    /// Whether this and `other` each resolve to the definitions of one
    /// path whose set hash is the same: the same code, wherever it is.
    pub(crate) fn same_code(&self, other: &Self) -> bool {
        let (Self::Resolved(Target::Symbol(ours)), Self::Resolved(Target::Symbol(theirs))) =
            (self, other)
        else {
            return false;
        };
        ours.hash() == theirs.hash()
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

/// The indexed files of `entries`, those `walked` holds apart from the
/// rest: the tree's files and those an anchor brought.
fn split<'i>(
    entries: &'i BTreeMap<String, Entry>,
    walked: &BTreeSet<String>,
) -> (Files<'i>, Files<'i>) {
    entries
        .iter()
        .filter_map(|(path, entry)| Some((path.as_str(), indexed(entry)?)))
        .partition(|(path, _)| walked.contains(*path))
}

impl CodeFacts {
    /// The facts of `entries`, which hold every path an anchor to be
    /// resolved names, every file of `walked` (the tree's Rust files and
    /// manifests, whose macros a file can invoke) and the manifests above
    /// each anchored Rust file outside them: a path absent from them is
    /// missing.
    pub(crate) fn new(entries: BTreeMap<String, Entry>, walked: &BTreeSet<String>) -> Self {
        let (tree, outside) = split(&entries, walked);
        let crates = Crates::of(&tree, &outside);
        let expanded: BTreeMap<String, Result<Expansion, IndexFault>> = expand(&tree, &crates)
            .into_iter()
            .chain(expand(&outside, &crates))
            .map(|(path, expansion)| (path.to_owned(), expansion))
            .collect();
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
        let expanded = expand(&outside, &crates)
            .into_iter()
            .map(|(path, expansion)| (path.to_owned(), expansion))
            .collect();
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
    /// spells the last name of its symbol among the arguments it keeps,
    /// as a word or in a group's tokens: a type a macro the facts do not
    /// hold defines may be there.
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
        index.rust.invocations.iter().any(|invocation| {
            invocation.arguments.iter().any(|argument| match argument {
                Element::Word(word) => word == name,
                Element::Group(_, Some(tokens)) => tokens.contains(name),
                Element::Token(_)
                | Element::Group(_, None)
                | Element::Binding(_)
                | Element::Forwarded(..)
                | Element::Many => false,
            })
        })
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

    /// Whether the file at `path` has an index that holds a definition.
    pub(crate) fn defines_anything(&self, path: &str) -> bool {
        self.entries
            .get(path)
            .and_then(indexed)
            .is_some_and(|(_, index)| !index.definitions.is_empty())
    }

    /// Whether `anchor`'s file defines its symbol as the whole path of a
    /// definition, no outer segment left out.
    pub(crate) fn defines_whole(&self, anchor: &Anchor) -> bool {
        let Some((kind, index)) = self.entries.get(anchor.file()).and_then(indexed) else {
            return false;
        };
        let Some(Ok(symbol)) = anchor.symbol().map(|s| Symbol::parse(s, kind.syntax())) else {
            return false;
        };
        let expansion = self
            .expanded
            .get(anchor.file())
            .and_then(|e| e.as_ref().ok());
        let scopes = self.scopes(index, expansion);
        let added = expansion.map_or(&[][..], |expansion| &expansion.definitions);
        index.definitions.iter().chain(added).any(|definition| {
            symbol.is_path_of(definition.kind, scopes.path_from_end(definition.scope))
        })
    }

    /// For each of `anchors`, a symbol on a Rust file that may be gone,
    /// where its code may have moved ([`Namesakes`]). One pass over the
    /// tree's definitions serves them all, and none is made for no anchor.
    /// A file of another language is not searched: its crate and target
    /// are not known.
    pub(crate) fn namesakes<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a Anchor>,
    ) -> BTreeMap<&'a Anchor, Namesakes> {
        let wanted: Vec<(&Anchor, Symbol)> = anchors
            .into_iter()
            .filter(|anchor| FileKind::of(anchor.file()) == Some(FileKind::Rust))
            .filter_map(|anchor| {
                let symbol = Symbol::parse(anchor.symbol()?, FileKind::Rust.syntax());
                Some((anchor, symbol.ok()?))
            })
            .collect();
        if wanted.is_empty() {
            return BTreeMap::new();
        }
        let last = wanted
            .iter()
            .filter_map(|(_, symbol)| symbol.segments().last())
            .collect();
        let holders = self.holders(&last);
        let (tree, outside) = split(&self.entries, &self.walked);
        let crates = Crates::of(&tree, &outside);
        let mut found = BTreeMap::new();
        for (anchor, symbol) in wanted {
            let files = symbol.segments().last().and_then(|own| holders.get(own));
            let at_home = files.is_some_and(|files| files.contains(anchor.file()))
                || self.spells_in_invocation(anchor);
            let place = target_of(anchor.file(), &crates);
            let role = match self.test_files.contains(anchor.file()) {
                true => Role::Test,
                false => role_of_place(anchor.file(), &crates),
            };
            let elsewhere = files
                .into_iter()
                .flatten()
                .filter(|&&file| file != anchor.file() && self.walked.contains(file))
                .filter(|&&file| place.is_some() && target_of(file, &crates) == place)
                .filter(|&&file| self.writes(file, &symbol, role))
                .map(|&file| file.to_owned())
                .collect();
            found.insert(anchor, Namesakes { at_home, elsewhere });
        }
        found
    }

    /// The files that define something whose own segment, the last of its
    /// path, is one of `last`, by that segment.
    fn holders<'s>(
        &'s self,
        last: &BTreeSet<&Segment>,
    ) -> BTreeMap<&'s Segment, BTreeSet<&'s str>> {
        let mut holders: BTreeMap<&Segment, BTreeSet<&str>> = BTreeMap::new();
        for (path, scopes, definition, _) in self.definitions() {
            let own = scopes.path_from_end(definition.scope).next();
            if let Some(own) = own.filter(|own| last.contains(own)) {
                holders.entry(own).or_default().insert(path);
            }
        }
        holders
    }

    /// Whether the file at `path` writes an item of `role` whose whole
    /// path `symbol` is: no re-export, and no type a macro invocation
    /// defines, whose code is the macro's.
    fn writes(&self, path: &str, symbol: &Symbol, role: Role) -> bool {
        let Some((_, index)) = self.entries.get(path).and_then(indexed) else {
            return false;
        };
        let file_role = match self.test_files.contains(path) {
            true => Role::Test,
            false => Role::Code,
        };
        let scopes = self.scopes(index, None);
        index.definitions.iter().any(|definition| {
            definition.kind != DefinitionKind::Reexport
                && definition.role.or(file_role) == role
                && symbol.is_path_of(definition.kind, scopes.path_from_end(definition.scope))
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
            .filter(|definition| {
                symbol.names(definition.kind, scopes.path_from_end(definition.scope))
            })
            .collect();
        let whole: Vec<&Definition> = named
            .iter()
            .copied()
            .filter(|d| symbol.is_path_of(d.kind, scopes.path_from_end(d.scope)))
            .collect();
        let chosen = match whole.is_empty() {
            true => named,
            false => whole,
        };
        let mut sets = sets_by_path(chosen, scopes).into_values();
        match (sets.next(), sets.len()) {
            (None, _) => Resolution::SymbolMissing,
            (Some((first, rest)), 0) => {
                Resolution::Resolved(Target::Symbol(Definitions::of(first, &rest)))
            }
            (Some(_), others) => Resolution::Ambiguous(others + 1),
        }
    }
}

/// Where a symbol anchor's code may have moved, by what the tree holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Namesakes {
    /// Whether the anchor's own file defines the symbol's last segment
    /// under any path, or spells it among the arguments a macro invocation
    /// keeps: the code may be there under another spelling.
    pub(crate) at_home: bool,
    /// The other walked files of the anchor's crate target, by place, that
    /// write an item of its file's test role whose whole path the symbol
    /// is ([`CodeFacts::namesakes`]).
    pub(crate) elsewhere: Vec<String>,
}

/// The `definitions` of each path, as `scopes` spells it: the first of the
/// path, and the rest of its set.
fn sets_by_path<'d, 's>(
    definitions: Vec<&'d Definition>,
    scopes: Scopes<'s>,
) -> BTreeMap<Vec<&'s Segment>, (&'d Definition, Vec<&'d Definition>)> {
    let mut paths: BTreeMap<Vec<&Segment>, (&Definition, Vec<&Definition>)> = BTreeMap::new();
    for definition in definitions {
        let path = scopes.path_from_end(definition.scope).collect();
        match paths.entry(path) {
            btree_map::Entry::Vacant(vacant) => {
                vacant.insert((definition, Vec::new()));
            }
            btree_map::Entry::Occupied(mut occupied) => occupied.get_mut().1.push(definition),
        }
    }
    paths
}
