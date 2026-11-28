//! One file of the code tree as `map` frames it: its line count and
//! whether the index read it, its definitions, and where the decisions'
//! anchors lie in it. Read from the walk of the calling tree alone, as a
//! level is: an anchor resolves against that tree, and no git runs.
//!
//! An anchor on the file with no symbol, or on a directory above it, puts
//! its decision on the file, listed apart and in no range. The file's
//! lines are cut into maximal ranges, each with the other decisions whose
//! symbol anchors resolve to spans covering it. A symbol anchor on the
//! file that resolves to no span is listed with its finding, judged as
//! `lint` judges one with git not asked ([`anchors::in_tree`]).

use std::cmp::Ordering;
use std::sync::LazyLock;

use crate::code::{
    self, CodeTree, DefinitionKind, Entry, FileFault, IndexFault, Node, ReadFault, Role,
    SymbolAnchor, Visibility,
};
use crate::lint::anchors::{self, KindName};
use crate::store::{Decision, StatusName, Store};

use super::level::{Doc, walked};

/// A file's frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// Its address ([`CodeTree::address`]).
    pub(crate) address: String,
    /// Its file name.
    pub(crate) name: String,
    /// `None` when its bytes were not read, which `index_state` says why.
    pub(crate) line_count: Option<usize>,
    pub(crate) index_state: IndexState,
    /// Why the index holds none of it, a sentence; `None` when it does.
    pub(crate) cause: Option<String>,
    pub(crate) doc: Option<Doc>,
    /// The decisions on the file whole, by name.
    pub(crate) on_file: Vec<AnchoredDecision>,
    /// The symbol anchors on the file that resolve to no span, by
    /// decision, then in the decision's order.
    pub(crate) unspanned_anchors: Vec<UnspannedAnchor>,
    /// Lines 1 to its line count, in order; none when its bytes were not
    /// read.
    pub(crate) ranges: Vec<LineRange>,
    /// In its index's order.
    pub(crate) definitions: Vec<FileDefinition>,
}

/// Whether the index holds a file, and why not when it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexState {
    Indexed,
    /// Past the size a source file may have or its kind is indexed within.
    TooLarge,
    NotAFile,
    Unreadable,
    NoThread,
    NotUtf8,
    Syntax,
    TooComplex,
    TooDeep,
    Grammar,
    TooManyIndentations,
}

impl IndexState {
    pub(crate) const ALL: [Self; 11] = [
        Self::Indexed,
        Self::TooLarge,
        Self::NotAFile,
        Self::Unreadable,
        Self::NoThread,
        Self::NotUtf8,
        Self::Syntax,
        Self::TooComplex,
        Self::TooDeep,
        Self::Grammar,
        Self::TooManyIndentations,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::TooLarge => "too_large",
            Self::NotAFile => "not_a_file",
            Self::Unreadable => "unreadable",
            Self::NoThread => "no_thread",
            Self::NotUtf8 => "not_utf8",
            Self::Syntax => "syntax",
            Self::TooComplex => "too_complex",
            Self::TooDeep => "too_deep",
            Self::Grammar => "grammar",
            Self::TooManyIndentations => "too_many_indentations",
        }
    }

    const fn of(fault: FileFault) -> Self {
        match fault {
            FileFault::Read(ReadFault::TooLarge { .. })
            | FileFault::Index(IndexFault::TooLarge { .. }) => Self::TooLarge,
            FileFault::Read(ReadFault::NotAFile) => Self::NotAFile,
            FileFault::Read(ReadFault::Unreadable(_)) => Self::Unreadable,
            FileFault::Read(ReadFault::NoThread(_)) => Self::NoThread,
            FileFault::Index(IndexFault::NotUtf8 { .. }) => Self::NotUtf8,
            FileFault::Index(IndexFault::Syntax { .. }) => Self::Syntax,
            FileFault::Index(IndexFault::TooComplex) => Self::TooComplex,
            FileFault::Index(IndexFault::TooDeep) => Self::TooDeep,
            FileFault::Index(IndexFault::Grammar) => Self::Grammar,
            FileFault::Index(IndexFault::TooManyIndentations { .. }) => Self::TooManyIndentations,
        }
    }
}

/// A decision, by name, with its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnchoredDecision {
    pub(crate) decision: String,
    pub(crate) status: StatusName,
}

/// A symbol anchor on a file that resolves to no span of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnspannedAnchor {
    pub(crate) decision: AnchoredDecision,
    /// `file::symbol`.
    pub(crate) anchor: String,
    /// One of [`KindName::IN_TREE`].
    pub(crate) kind: KindName,
    /// What makes it one, a sentence.
    pub(crate) cause: String,
}

/// Lines of a file, and the decisions whose spans cover every one of
/// them, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineRange {
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    pub(crate) decisions: Vec<AnchoredDecision>,
}

/// A definition of a frame's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileDefinition {
    /// Its path's segments, outermost first.
    pub(crate) path: Vec<String>,
    /// One of [`DEFINITION_KINDS`].
    pub(crate) kind: &'static str,
    /// One of [`VISIBILITIES`]; `None` where the source states none.
    pub(crate) visibility: Option<&'static str>,
    pub(crate) test: bool,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    pub(crate) doc: Option<Doc>,
    /// A struct's, an enum's or a union's of a Rust file alone.
    pub(crate) impls: Option<Impls>,
}

/// The impls the other walked Rust files hold of a type's name
/// ([`code::Impls`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Impls {
    /// The holders of the name in the tree, this type included.
    pub(crate) types_named: usize,
    /// By file, then by line; `None` when another holder has the name.
    pub(crate) elsewhere: Option<Vec<ImplBlock>>,
    /// Those a page left out of `elsewhere`, every one, when the
    /// definition passes a page with them.
    pub(crate) omitted: usize,
}

/// An impl of a type's name in another file ([`code::ImplBlock`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImplBlock {
    /// The file's address.
    pub(crate) file: String,
    pub(crate) path: Vec<String>,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
}

/// The name of each kind of definition, the values a definition's `kind`
/// takes.
pub(crate) static DEFINITION_KINDS: LazyLock<Vec<&str>> =
    LazyLock::new(|| DefinitionKind::ALL.map(DefinitionKind::name).to_vec());

/// The name of each visibility, the values a definition's `visibility`
/// takes.
pub(crate) static VISIBILITIES: LazyLock<Vec<&str>> =
    LazyLock::new(|| Visibility::ALL.map(Visibility::name).to_vec());

/// The frame of the file at the address `file` in the tree at `store`'s
/// code root, with the anchors of `decisions`, each by its name; `None`
/// when `file` is no file's address. It reads every source file the walk
/// takes, and fills the index's cache.
pub(crate) fn frame<'d>(
    store: &Store,
    decisions: impl IntoIterator<Item = (&'d String, &'d Decision)>,
    file: &str,
) -> Option<Frame> {
    let facts = walked(store);
    frame_in(&CodeTree::of(&facts), decisions, file)
}

/// A decision as a frame's coverage keys it, ordered by its name, which
/// no other decision has.
#[derive(Clone, Copy)]
struct Key<'d> {
    name: &'d str,
    decision: &'d Decision,
}

impl PartialEq for Key<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Key<'_> {}

impl PartialOrd for Key<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(other.name)
    }
}

/// [`frame`] in `tree`.
pub(crate) fn frame_in<'d>(
    tree: &CodeTree<'_>,
    decisions: impl IntoIterator<Item = (&'d String, &'d Decision)>,
    file: &str,
) -> Option<Frame> {
    let node @ Node::File { path, .. } = tree.node(file)? else {
        return None;
    };
    let Some(Entry::File { lines, index, .. }) = tree.entry(path) else {
        return None;
    };
    let anchors = decisions.into_iter().flat_map(|(name, decision)| {
        let key = Key { name, decision };
        decision.anchors.iter().map(move |anchor| (key, anchor))
    });
    let coverage = tree.coverage(path, anchors);
    let unspanned = coverage.symbols.iter().filter_map(unspanned);
    let ranges = coverage.ranges.into_iter().map(|range| LineRange {
        first_line: range.first_line,
        last_line: range.last_line,
        decisions: range.keys.into_iter().map(named).collect(),
    });
    let definitions = tree.file_definitions(path).into_iter();
    let fault = index.as_ref().err();
    Some(Frame {
        address: tree.address(node),
        name: tree.name(node).to_owned(),
        line_count: *lines,
        index_state: fault.map_or(IndexState::Indexed, |fault| IndexState::of(*fault)),
        cause: fault.map(ToString::to_string),
        doc: tree.doc(node).map(Doc::of),
        on_file: coverage.on_file.into_iter().map(named).collect(),
        unspanned_anchors: unspanned.collect(),
        ranges: ranges.collect(),
        definitions: definitions
            .map(|definition| framed(tree, definition))
            .collect(),
    })
}

/// `symbol` as a frame lists it when it resolves to no span; `None` when
/// it resolves, as its spans lie in the ranges.
fn unspanned(symbol: &SymbolAnchor<'_, Key<'_>>) -> Option<UnspannedAnchor> {
    let kind = anchors::in_tree(&symbol.resolution, &symbol.key.decision.status)?;
    Some(UnspannedAnchor {
        decision: named(symbol.key),
        anchor: symbol.anchor.to_string(),
        kind: kind.name(),
        cause: kind.to_string(),
    })
}

/// The decision `key` names, owned.
fn named(key: Key<'_>) -> AnchoredDecision {
    AnchoredDecision {
        decision: key.name.to_owned(),
        status: key.decision.status.kind(),
    }
}

/// `definition` as a frame lists it.
fn framed(tree: &CodeTree<'_>, definition: code::FileDefinition<'_>) -> FileDefinition {
    FileDefinition {
        path: definition.path,
        kind: definition.kind.name(),
        visibility: definition.visibility.map(Visibility::name),
        test: match definition.role {
            Role::Code => false,
            Role::Test => true,
        },
        first_line: definition.first_line,
        last_line: definition.last_line,
        doc: definition.doc.map(Doc::of),
        impls: definition.impls.map(|found| impls(tree, found)),
    }
}

/// `found`, each file by its address.
fn impls(tree: &CodeTree<'_>, found: code::Impls<'_>) -> Impls {
    match found {
        code::Impls::ByName(blocks) => {
            let elsewhere = blocks.into_iter().map(|block| ImplBlock {
                file: tree.address(block.file),
                path: block.path,
                first_line: block.first_line,
                last_line: block.last_line,
            });
            Impls {
                types_named: 1,
                elsewhere: Some(elsewhere.collect()),
                omitted: 0,
            }
        }
        code::Impls::Ambiguous(holders) => Impls {
            types_named: holders,
            elsewhere: None,
            omitted: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::code::{CodeFacts, FileKind};

    /// A fault the index gives a Rust file for `state`, and whether its
    /// bytes were read.
    fn fault(state: IndexState) -> Option<(FileFault, bool)> {
        let read = |fault| Some((FileFault::Read(fault), false));
        let index = |fault| Some((FileFault::Index(fault), true));
        match state {
            IndexState::Indexed => None,
            IndexState::TooLarge => read(ReadFault::TooLarge { bytes: 9, max: 8 }),
            IndexState::NotAFile => read(ReadFault::NotAFile),
            IndexState::Unreadable => read(ReadFault::Unreadable(std::io::ErrorKind::Other)),
            IndexState::NoThread => read(ReadFault::NoThread(std::io::ErrorKind::Other)),
            IndexState::NotUtf8 => index(IndexFault::NotUtf8 { at: 4 }),
            IndexState::Syntax => index(IndexFault::Syntax { at: 4 }),
            IndexState::TooComplex => index(IndexFault::TooComplex),
            IndexState::TooDeep => index(IndexFault::TooDeep),
            IndexState::Grammar => index(IndexFault::Grammar),
            IndexState::TooManyIndentations => index(IndexFault::TooManyIndentations { max: 2 }),
        }
    }

    /// Each fault the index can have a walked file for, at a file outside
    /// every crate, read with 3 lines where the fault comes after the read.
    #[test]
    fn a_frame_of_a_file_with_no_index_names_its_fault_and_lists_no_definition() {
        let faults: Vec<(IndexState, FileFault, bool)> = IndexState::ALL
            .into_iter()
            .filter_map(|state| fault(state).map(|(fault, read)| (state, fault, read)))
            .collect();
        let mut entries = BTreeMap::new();
        for (at, (_, fault, read)) in faults.iter().enumerate() {
            let entry = Entry::File {
                kind: FileKind::Rust,
                lines: read.then_some(3),
                index: Err(*fault),
            };
            entries.insert(format!("src/{at}.rs"), entry);
        }
        let walked = entries.keys().cloned().collect();
        let facts = CodeFacts::new(entries, walked);
        let tree = CodeTree::of(&facts);

        for (at, (state, _, read)) in faults.iter().enumerate() {
            let frame = frame_in(&tree, &BTreeMap::new(), &format!("file:src/{at}.rs")).unwrap();

            assert_eq!(frame.index_state, *state);
            assert!(frame.cause.is_some(), "{state:?}");
            assert_eq!(frame.definitions, [], "{state:?}");
            let ranges = frame.ranges.iter();
            let ranges: Vec<(usize, usize)> = ranges
                .map(|range| (range.first_line, range.last_line))
                .collect();
            let expected = if *read { vec![(1, 3)] } else { Vec::new() };
            assert_eq!(ranges, expected, "{state:?}");
            assert_eq!(frame.line_count, read.then_some(3), "{state:?}");
        }
        assert_eq!(faults.len(), IndexState::ALL.len() - 1);
    }
}
