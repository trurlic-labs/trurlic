//! A file's frame: its spec, its arguments, what it shows, and how its
//! items are cut into pages.
//!
//! A frame's items are one order: the decisions on the file whole, the
//! symbol anchors on it that cover no line, the line ranges, then the
//! definitions. A page lists them from a position while they fit beside
//! what it leaves out, and `next` names the position of the first item
//! after it, so a walk from `next` to `next` lists each item once. A type
//! whose impls in other files pass a page alone is listed with them left
//! out and counted; an item that passes a page even so is listed by no
//! page and counted in `too_large`.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::lint::anchors::KindName;
use crate::sources::{
    self, AnchoredDecision, DEFINITION_KINDS, Doc, FileDefinition, Frame, ImplBlock, Impls,
    IndexState, LineRange, UnspannedAnchor, VISIBILITIES,
};
use crate::store::{ProjectState, StatusName, Store};
use crate::tools::result::ToolError;
use crate::tools::spec::kinds::{Position, Text};
use crate::tools::spec::output::{Bool, Count, Items, Nested, Str, Tag, Texts};
use crate::tools::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use crate::tools::spec::{Args, Fields, Output, Shown, to_value};
use crate::tools::truncate::json_len;

use super::page::{fill, position};
use super::tool::MAX_ADDRESS_BYTES;

pub(crate) static FRAME: ToolSpec<FrameArgs, FramePage> = ToolSpec {
    name: "map_frame",
    title: "Map: a file's frame",
    description: DESCRIPTION,
    destructive: Destructive::No,
    access: Access::Read(frame),
};

const DESCRIPTION: &str = "One file of the code tree: its lines, whether the index read it, the \
decisions on it and on its line ranges, and its definitions, paged from `from_position`.";

/// The arguments of `map_frame`, and of `trurlic map --frame`.
#[derive(Debug)]
pub(crate) struct FrameArgs {
    file: String,
    from_position: Option<usize>,
}

impl Args for FrameArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        let address = Text {
            max_bytes: MAX_ADDRESS_BYTES,
        };
        Self {
            file: fields.required(
                "file",
                "The address of a file, as a level gives it: `file:<path>`.",
                &address,
            ),
            from_position: fields.optional(
                "from_position",
                "The position of the first item to list: a previous page's `next`.",
                &Position,
            ),
        }
    }
}

/// What `map_frame` answers: a page of one file's frame.
pub(crate) struct FramePage {
    /// The frame, its lists holding the page's items.
    frame: Frame,
    /// The items of the frame, on every page.
    total: usize,
    /// The items after the page.
    omitted: usize,
    /// The items before `next` that no page lists.
    too_large: usize,
    /// The position of the first item after the page.
    next: Option<usize>,
}

/// An item of a frame, in the frame's order of kinds.
enum Item {
    OnFile(AnchoredDecision),
    Unspanned(UnspannedAnchor),
    Range(LineRange),
    Definition(FileDefinition),
}

/// The page of the frame of the file at `arguments.file` that starts at
/// `arguments.from_position`.
fn frame(store: &Store, state: &ProjectState, arguments: FrameArgs) -> Outcome<FramePage> {
    let FrameArgs {
        file,
        from_position,
    } = arguments;
    let frame = sources::frame(store, &state.decisions, &file).ok_or_else(|| no_file(&file))?;
    page(frame, from_position)
}

/// The refusal of `file`, which names no file of the code tree. A text
/// with no `:` is no address of any kind, and the form of one is named
/// with it.
fn no_file(file: &str) -> ToolError {
    let form = match file.contains(':') {
        true => "`file:` and the file's path".to_owned(),
        false => format!("`file:{file}`"),
    };
    let message = format!(
        "`{file}` is no file of the code tree: a frame takes a file's address, as a level gives \
         it: {form}"
    );
    ToolError::invalid("file", message)
}

/// The page of `frame` from the position `from`, the argument
/// `from_position`.
fn page(mut frame: Frame, from: Option<usize>) -> Outcome<FramePage> {
    let on_file = std::mem::take(&mut frame.on_file)
        .into_iter()
        .map(Item::OnFile);
    let unspanned = std::mem::take(&mut frame.unspanned_anchors);
    let ranges = std::mem::take(&mut frame.ranges)
        .into_iter()
        .map(Item::Range);
    let definitions = std::mem::take(&mut frame.definitions);
    let items: Vec<Item> = on_file
        .chain(unspanned.into_iter().map(Item::Unspanned))
        .chain(ranges)
        .chain(definitions.into_iter().map(Item::Definition))
        .collect();
    let total = items.len();
    let start = position(from, total)?;
    let mut page = FramePage {
        frame,
        total,
        omitted: usize::MAX,
        too_large: usize::MAX,
        next: Some(usize::MAX),
    };
    let room = MAX_TOOL_RESULT_BYTES.saturating_sub(json_len(&to_value(&page)));
    let pending = items.into_iter().skip(start).rev();
    let mut pending: Vec<(Item, usize)> = pending.map(|item| (item, room)).collect();
    let filled = fill(&mut pending, fitted, |item, previous| {
        let after = previous.is_some_and(|previous| same_list(previous, item));
        bytes(item) + usize::from(after)
    });
    page.too_large = filled.too_large;
    page.omitted = pending.len();
    let after = start + filled.listed.len() + filled.too_large;
    page.next = (page.omitted > 0).then_some(after);
    filled.listed.into_iter().for_each(|item| page.list(item));
    Ok(page)
}

impl FramePage {
    /// Add `item` to the list of its kind.
    fn list(&mut self, item: Item) {
        let frame = &mut self.frame;
        match item {
            Item::OnFile(decision) => frame.on_file.push(decision),
            Item::Unspanned(anchor) => frame.unspanned_anchors.push(anchor),
            Item::Range(range) => frame.ranges.push(range),
            Item::Definition(definition) => frame.definitions.push(definition),
        }
    }
}

/// Whether `a` and `b` are items of one list of a page, which a comma
/// parts.
fn same_list(a: &Item, b: &Item) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// The bytes `item` takes as JSON.
fn bytes(item: &Item) -> usize {
    let value = match item {
        Item::OnFile(decision) => to_value(decision),
        Item::Unspanned(anchor) => to_value(anchor),
        Item::Range(range) => to_value(range),
        Item::Definition(definition) => to_value(definition),
    };
    json_len(&value)
}

/// `item` as a page lists it within `room` bytes: whole, or a type with
/// its impls in other files left out and counted. `None` when it passes
/// `room` even so.
fn fitted(mut item: Item, room: usize) -> Option<Item> {
    if bytes(&item) <= room {
        return Some(item);
    }
    if let Item::Definition(FileDefinition {
        impls: Some(impls), ..
    }) = &mut item
        && let Some(elsewhere) = &mut impls.elsewhere
    {
        impls.omitted = std::mem::take(elsewhere).len();
    }
    (bytes(&item) <= room).then_some(item)
}

impl Output for FramePage {
    fn declare(fields: &mut Shown<'_, Self>) {
        let address = "Its address, as a level gives it.";
        fields.required("address", address, &Str, |page| &page.frame.address);
        let name = "Its file name, the last segment of its path.";
        fields.required("name", name, &Str, |page| &page.frame.name);
        let lines = "Its line count; absent when its bytes were not read, which \
                     `index_state` says why.";
        fields.optional("line_count", lines, &Count, |page| {
            page.frame.line_count.as_ref()
        });
        declare_index(fields);
        let doc = "The first paragraph of its doc the index holds: a Rust file's.";
        let kind = Nested::<Doc>::new();
        fields.optional("doc", doc, &kind, |page| page.frame.doc.as_ref());
        let total = "The items of the frame, on every page: the four lists below, over all \
                     pages.";
        fields.required("total", total, &Count, |page| &page.total);
        declare_lists(fields);
        let omitted = "The items after the page, when it leaves any out.";
        fields.optional("omitted", omitted, &Count, |page| {
            (page.omitted > 0).then_some(&page.omitted)
        });
        let too_large = "The items between the page's first and `next` that no page lists, as \
                         one passes a page alone even with its impls left out.";
        fields.optional("too_large", too_large, &Count, |page| {
            (page.too_large > 0).then_some(&page.too_large)
        });
        let next = "The position of the first item after the page: `from_position` lists from \
                    it.";
        fields.optional("next", next, &Count, |page| page.next.as_ref());
    }
}

/// The fields of a [`FramePage`] that say whether the index read its file.
fn declare_index(fields: &mut Shown<'_, FramePage>) {
    let index = Tag {
        all: &IndexState::ALL,
        name: IndexState::as_str,
    };
    let about = "`indexed`, or why the index holds none of it, so it lists no definition: \
                 `too_large` (past the size a source file may have, or its kind is indexed \
                 within), `not_a_file`, `unreadable`, `no_thread` (no thread to index it on \
                 started), `not_utf8`, `syntax` (TOML or JSON that does not parse), \
                 `too_complex` (past its budget of work or size), `too_deep` (a path past the \
                 nesting bound), `grammar` (a grammar the parser does not load), \
                 `too_many_indentations` (a Python file).";
    fields.required("index_state", about, &index, |page| &page.frame.index_state);
    let cause = "Present when it is not `indexed`: why, as a sentence.";
    fields.optional("cause", cause, &Str, |page| page.frame.cause.as_deref());
}

/// The lists of a [`FramePage`], each holding the page's items.
fn declare_lists(fields: &mut Shown<'_, FramePage>) {
    let on_file = "The decisions with an anchor on the file whole, or on a directory above it, \
                   with no symbol: they cover every line, and no range lists them, whatever \
                   their symbol anchors cover. By name.";
    let kind = Items {
        item: Nested::<AnchoredDecision>::new(),
    };
    fields.required("on_file", on_file, &kind, |page| &page.frame.on_file);
    let unspanned = "The symbol anchors on the file that resolve to no definition in this tree, \
                     so they cover no line, with their finding. By decision.";
    let kind = Items {
        item: Nested::<UnspannedAnchor>::new(),
    };
    fields.required("unspanned_anchors", unspanned, &kind, |page| {
        &page.frame.unspanned_anchors
    });
    let ranges = "Its lines cut into ranges, from line 1 to its line count with no gap or \
                  overlap, each covered by one set of the decisions not in `on_file`, through \
                  the spans their symbol anchors resolve to; a range ends where that set \
                  changes. None when its bytes were not read or it has no line.";
    let kind = Items {
        item: Nested::<LineRange>::new(),
    };
    fields.required("ranges", ranges, &kind, |page| &page.frame.ranges);
    let definitions = "Its definitions, in the order of their spans in the file.";
    let kind = Items {
        item: Nested::<FileDefinition>::new(),
    };
    fields.required("definitions", definitions, &kind, |page| {
        &page.frame.definitions
    });
}

impl Output for AnchoredDecision {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("decision", "The decision's name.", &Str, |decision| {
            &decision.decision
        });
        let status = Tag {
            all: &StatusName::ALL,
            name: StatusName::as_str,
        };
        fields.required("status", "Its status.", &status, |decision| {
            &decision.status
        });
    }
}

impl Output for UnspannedAnchor {
    fn declare(fields: &mut Shown<'_, Self>) {
        let decision = "The decision's name.";
        fields.required("decision", decision, &Str, |anchor| {
            &anchor.decision.decision
        });
        let status = Tag {
            all: &StatusName::ALL,
            name: StatusName::as_str,
        };
        fields.required("status", "Its status.", &status, |anchor| {
            &anchor.decision.status
        });
        let spelled = "The anchor, `file::symbol`.";
        fields.required("anchor", spelled, &Str, |anchor| &anchor.anchor);
        let kind = Tag {
            all: &KindName::IN_TREE,
            name: KindName::as_str,
        };
        let about = "Its finding, as `status` names it with git not asked: `unresolved`: the \
                     file defines nothing its symbol names, on a decision ever accepted; \
                     `planned`: the same on a decision never accepted, as `trurlic check` \
                     names it; `malformed`: a symbol out of the grammar; `ambiguous`: its \
                     symbol names definitions of several paths, none of which it covers; \
                     `unverified`: the file has no index.";
        fields.required("kind", about, &kind, |anchor| &anchor.kind);
        let cause = "What makes it one, as a sentence.";
        fields.required("cause", cause, &Str, |anchor| &anchor.cause);
    }
}

impl Output for LineRange {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("first_line", "Its first line, from 1.", &Count, |range| {
            &range.first_line
        });
        fields.required("last_line", "Its last line.", &Count, |range| {
            &range.last_line
        });
        let decisions = "The decisions one of whose symbol anchors resolves to a span covering \
                         each of its lines, by name; empty where none does.";
        let kind = Items {
            item: Nested::<AnchoredDecision>::new(),
        };
        fields.required("decisions", decisions, &kind, |range| &range.decisions);
    }
}

impl Output for FileDefinition {
    fn declare(fields: &mut Shown<'_, Self>) {
        let path = "Its path's segments as the index spells them, outermost first, as an \
                    anchor's symbol joins them with `::`: a definition is nested in the one \
                    whose path its own extends.";
        let segments = Texts;
        fields.required("path", path, &segments, |definition| &definition.path);
        let kind = Tag {
            all: DEFINITION_KINDS.as_slice(),
            name: std::convert::identity,
        };
        fields.required("kind", "What it is.", &kind, |definition| &definition.kind);
        let visibility = Tag {
            all: VISIBILITIES.as_slice(),
            name: std::convert::identity,
        };
        let about = "Who may name it, where its source states it.";
        fields.optional("visibility", about, &visibility, |definition| {
            definition.visibility.as_ref()
        });
        let test = "Whether it is test code, by its own mark or its file's.";
        fields.required("test", test, &Bool, |definition| &definition.test);
        let first = "The first line of its span, its attributes and doc comments included.";
        fields.required("first_line", first, &Count, |definition| {
            &definition.first_line
        });
        let last = "The last line of its span.";
        fields.required("last_line", last, &Count, |definition| {
            &definition.last_line
        });
        let doc = "The first paragraph of its doc the index holds: a Rust definition's.";
        let kind = Nested::<Doc>::new();
        fields.optional("doc", doc, &kind, |definition| definition.doc.as_ref());
        let impls = "Present on a struct, an enum or a union of a Rust file alone.";
        let kind = Nested::<Impls>::new();
        fields.optional("impls", impls, &kind, |definition| {
            definition.impls.as_ref()
        });
    }
}

impl Output for Impls {
    fn declare(fields: &mut Shown<'_, Self>) {
        let types = "The structs, enums, unions, traits and type aliases of the walked Rust \
                     files that have this type's name, this one included.";
        fields.required("types_named", types, &Count, |impls| &impls.types_named);
        let elsewhere = "Present when `types_named` is 1: the impls of the name in the other \
                         walked Rust files, by file, then by line. Every inherent impl of the \
                         name, however written, and a trait impl whose self type has no path \
                         before the name (`T`, `&T`, `T<U>`, `dyn T`); one written \
                         `std::io::Error` or `crate::x::T` is not listed. Matched by the name \
                         alone: a generic parameter or a type a `use` renames to it is taken for \
                         it, an impl through a type alias of another name is not found, and an \
                         inherent impl with no member is not listed.";
        let kind = Items {
            item: Nested::<ImplBlock>::new(),
        };
        fields.optional("elsewhere", elsewhere, &kind, |impls| {
            impls.elsewhere.as_ref()
        });
        let omitted = "The impls the page left out of `elsewhere`, every one, when the \
                       definition passes a page with them.";
        fields.optional("omitted", omitted, &Count, |impls| {
            (impls.omitted > 0).then_some(&impls.omitted)
        });
    }
}

impl Output for ImplBlock {
    fn declare(fields: &mut Shown<'_, Self>) {
        let file = "Its file's address.";
        fields.required("file", file, &Str, |block| &block.file);
        let path = "Its path's segments, outermost first: those of what holds it, the type's name, \
                    then a trait impl's `<Trait>`.";
        let segments = Texts;
        fields.required("path", path, &segments, |block| &block.path);
        let first = "A trait impl's first line; an inherent impl's first member's, as the \
                     index holds its members alone.";
        fields.required("first_line", first, &Count, |block| &block.first_line);
        let last = "Its last line; an inherent impl's last member's.";
        fields.required("last_line", last, &Count, |block| &block.last_line);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::code::{CodeFacts, CodeTree, Resolution, Target};
    use crate::sources::ChildKind;
    use crate::store::Decision;
    use crate::store::corpus::Corpus;
    use crate::tools::result::ToolResult;

    /// Each page of `frame` from the first to the last, each as a call's
    /// text read back: each fits the budget uncut.
    fn pages(frame: &Frame) -> Vec<Value> {
        let mut pages = Vec::new();
        let mut from = None;
        loop {
            let page = page(frame.clone(), from).unwrap();
            let text = ToolResult::structured(to_value(&page)).text().to_owned();
            assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{}", text.len());
            let read: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(read.get("truncated"), None);
            from = page.next;
            pages.push(read);
            if from.is_none() {
                return pages;
            }
        }
    }

    /// Each decision a range of the file at `path` may list, by name, with
    /// the lines of every span its symbol anchors there resolve to, each
    /// anchor resolved alone. A decision with an anchor on the file whole
    /// or on a directory above it, which no range lists, is left out.
    fn spans(
        facts: &CodeFacts,
        decisions: &BTreeMap<String, Decision>,
        path: &str,
    ) -> Vec<(String, Vec<(usize, usize)>)> {
        let whole = |decision: &Decision| {
            decision.anchors.iter().any(|anchor| {
                let above = format!("{}/", anchor.file());
                anchor.symbol().is_none() && (anchor.file() == path || path.starts_with(&above))
            })
        };
        let ranged = decisions.iter().filter(|(_, decision)| !whole(decision));
        let spanned = ranged.map(|(name, decision)| {
            let on_file = decision
                .anchors
                .iter()
                .filter(|anchor| anchor.file() == path);
            let symbols = on_file.filter(|anchor| anchor.symbol().is_some());
            let spans = symbols.flat_map(|anchor| match facts.resolve(anchor) {
                Resolution::Resolved(Target::Symbol(definitions)) => definitions.lines(),
                _ => Vec::new(),
            });
            (name.clone(), spans.collect())
        });
        spanned.collect()
    }

    /// The decisions of `spans` one of whose spans covers `line`.
    fn covering(spans: &[(String, Vec<(usize, usize)>)], line: usize) -> Vec<String> {
        let covers = |lines: &[(usize, usize)]| {
            lines
                .iter()
                .any(|(first, last)| (*first..=*last).contains(&line))
        };
        let named = spans.iter().filter(|(_, lines)| covers(lines));
        named.map(|(name, _)| name.clone()).collect()
    }

    /// The address of every file of `tree`, its levels walked.
    fn files(tree: &CodeTree<'_>) -> Vec<String> {
        let (mut files, mut pending) = (Vec::new(), vec![None]);
        while let Some(node) = pending.pop() {
            let level = crate::sources::level_in(tree, [], node.as_deref()).unwrap();
            for child in level.children {
                match child.kind {
                    ChildKind::File => files.push(child.address),
                    ChildKind::Crate
                    | ChildKind::OutsideCrates
                    | ChildKind::Module
                    | ChildKind::Directory => pending.push(Some(child.address)),
                }
            }
        }
        files
    }

    /// Every walked file's frame of `facts`, its pages walked from `next`
    /// to `next`: each definition listed once in order, every item
    /// counted, the ranges cutting the file's lines with no gap or overlap,
    /// each line's decisions those [`covering`] finds. The most pages one
    /// frame took.
    fn walk(facts: &CodeFacts, decisions: &BTreeMap<String, Decision>) -> usize {
        let tree = CodeTree::of(facts);
        let mut most = 0;
        for address in files(&tree) {
            let path = address.strip_prefix("file:").unwrap();
            let frame = crate::sources::frame_in(&tree, decisions, &address).unwrap();
            let pages = pages(&frame);
            let listed = |key: &str| -> Vec<Value> {
                let lists = pages
                    .iter()
                    .map(|page| page[key].as_array().unwrap().clone());
                lists.flatten().collect()
            };
            let definitions: Vec<Value> = frame.definitions.iter().map(to_value).collect();
            assert_eq!(listed("definitions"), definitions, "{path}");
            let items = ["on_file", "unspanned_anchors", "ranges", "definitions"];
            let total: usize = items.into_iter().map(|key| listed(key).len()).sum();
            assert_eq!(pages[0]["total"], total, "{path}");
            let ranges = listed("ranges");
            let spans = spans(facts, decisions, path);
            let mut line = 1;
            for range in &ranges {
                assert_eq!(range["first_line"], line, "{path}");
                let last = range["last_line"].as_u64().unwrap() as usize;
                let names = range["decisions"].as_array().unwrap().iter();
                let names: Vec<String> = names
                    .map(|decision| decision["decision"].as_str().unwrap().to_owned())
                    .collect();
                for at in line..=last {
                    assert_eq!(covering(&spans, at), names, "{path}:{at}");
                }
                line = last + 1;
            }
            assert_eq!(line - 1, frame.line_count.unwrap_or(0), "{path}");
            most = most.max(pages.len());
        }
        most
    }

    #[test]
    fn every_frame_of_this_repository_pages_within_the_budget_its_ranges_as_its_anchors_cover() {
        let (facts, _) = crate::sources::testing::this_repository();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let state = crate::discovery::discover(root)
            .unwrap()
            .load_state()
            .unwrap();

        let most = walk(facts, &state.decisions);

        assert!(most > 1, "the largest frame fits one page: {most}");
    }

    /// A corpus file is small, but anchored by many decisions, each
    /// through a symbol or the file.
    #[test]
    fn every_frame_of_a_corpus_project_lists_its_items_once_its_ranges_as_its_anchors_cover() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(Corpus::SIZES[1]).write(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        let facts = crate::sources::walked(&store);

        walk(&facts, &state.decisions);
    }

    /// A frame of `definitions` and nothing else.
    fn frame(definitions: Vec<FileDefinition>) -> Frame {
        Frame {
            address: "file:a.rs".to_owned(),
            name: "a.rs".to_owned(),
            line_count: None,
            index_state: IndexState::Indexed,
            cause: None,
            doc: None,
            on_file: Vec::new(),
            unspanned_anchors: Vec::new(),
            ranges: Vec::new(),
            definitions,
        }
    }

    /// A struct named `name` with `impls` impls in other files.
    fn definition(name: &str, impls: usize) -> FileDefinition {
        let block = |n: usize| ImplBlock {
            file: format!("file:{}/{n}.rs", "far".repeat(20)),
            path: vec![name.to_owned()],
            first_line: n,
            last_line: n,
        };
        FileDefinition {
            path: vec![name.to_owned()],
            kind: "struct",
            visibility: None,
            test: false,
            first_line: 1,
            last_line: 1,
            doc: None,
            impls: Some(Impls {
                types_named: 1,
                elsewhere: Some((0..impls).map(block).collect()),
                omitted: 0,
            }),
        }
    }

    #[test]
    fn a_type_that_passes_a_page_alone_is_listed_with_its_impls_left_out_and_counted() {
        let definitions = vec![
            definition("A", 2),
            definition("B", 2000),
            definition("C", 1),
        ];

        let pages = pages(&frame(definitions));

        let shown: Vec<(String, usize, u64)> = pages
            .iter()
            .flat_map(|page| page["definitions"].as_array().unwrap().clone())
            .map(|definition| {
                let impls = &definition["impls"];
                let listed = impls["elsewhere"].as_array().unwrap().len();
                let omitted = impls.get("omitted").map_or(0, |n| n.as_u64().unwrap());
                (
                    definition["path"][0].as_str().unwrap().to_owned(),
                    listed,
                    omitted,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("A".to_owned(), 2, 0),
                ("B".to_owned(), 0, 2000),
                ("C".to_owned(), 1, 0)
            ]
        );
    }

    #[test]
    fn an_item_too_large_for_any_page_is_counted_and_next_names_the_position_after_it() {
        let huge = "x".repeat(MAX_TOOL_RESULT_BYTES);
        let definitions = vec![definition("A", 0), definition(&huge, 0), definition("C", 0)];

        let pages = pages(&frame(definitions));

        let listed: Vec<&str> = pages
            .iter()
            .flat_map(|page| page["definitions"].as_array().unwrap())
            .map(|definition| definition["path"][0].as_str().unwrap())
            .collect();
        assert_eq!(listed, ["A", "C"]);
        assert_eq!(pages[0]["too_large"], 1);
        assert_eq!(pages[0]["total"], 3);
    }

    #[test]
    fn a_page_from_a_position_starts_at_the_item_there() {
        let definitions = vec![definition("A", 0), definition("B", 0)];

        let from_one = page(frame(definitions), Some(1)).unwrap();

        let listed = from_one.frame.definitions.iter();
        let listed: Vec<&str> = listed
            .map(|definition| definition.path[0].as_str())
            .collect();
        assert_eq!(listed, ["B"]);
    }
}
