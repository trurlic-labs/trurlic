//! A level cut into pages within the result budget. A page lists the
//! children from `from` on, in the level's order, while they fit beside
//! what it leaves out. That is the count of the children after it, the
//! address of the first of them, and the count of those no page lists.
//!
//! A child whose record passes a page of its own is listed with its list
//! left out and counted: a crate's dependencies, a module's or a file's
//! imports into its siblings. A child that passes it even so, or whose
//! address takes more than [`MAX_NEXT_BYTES`], is listed by no page. The
//! page it falls on counts it in `too_large`, and `next` names a child
//! after it. So a walk from `next` to `next` lists each other child once,
//! and counts each such one once.

use serde_json::Value;

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::sources::{Child, DecisionCounts, Dependencies, Level};
use crate::tools::result::ToolError;
use crate::tools::spec::output::{Count, Items, Nested, Str};
use crate::tools::spec::tool::Outcome;
use crate::tools::spec::{Output, Shown, to_value};
use crate::tools::truncate::json_len;

/// The most bytes a child's address may take as JSON for `next` to name
/// it beside a page's children: a quarter of the result.
const MAX_NEXT_BYTES: usize = MAX_TOOL_RESULT_BYTES / 4;

/// What `map` answers: a page of the children of one level.
pub(crate) struct Page {
    /// The children of the level.
    total: usize,
    /// The decisions anchored in the level's holder.
    decisions: DecisionCounts,
    /// Those of them anchored in no one child.
    decisions_in_no_child: DecisionCounts,
    /// The page's, in the level's order.
    children: Vec<Child>,
    /// The children after the page.
    omitted: usize,
    /// The children before `next` that no page lists.
    too_large: usize,
    /// The address of the first child after the page; `None` when none is.
    next: Option<String>,
}

impl Output for Page {
    fn declare(fields: &mut Shown<'_, Self>) {
        let total = "The children of the level, on every page.";
        fields.required("total", total, &Count, |page| &page.total);
        let decisions = "The decisions anchored in the node whose level this is, each once; at \
                         the top, every decision of the graph.";
        let kind = Nested::<DecisionCounts>::new();
        fields.required("decisions", decisions, &kind, |page| &page.decisions);
        let in_no_child = "Those of `decisions` anchored in no one child: through a directory \
                           whose walked files lie under two children or more, and at the top \
                           through no walked file.";
        fields.required("decisions_in_no_child", in_no_child, &kind, |page| {
            &page.decisions_in_no_child
        });
        let children = "The page's children, in the order of the level: crates, modules, \
                        directories, then files, each kind by its path.";
        let kind = Items {
            item: Nested::<Child>::new(),
        };
        fields.required("children", children, &kind, |page| &page.children);
        let omitted = "The children after the page, when it leaves any out.";
        fields.optional("omitted", omitted, &Count, |page| {
            (page.omitted > 0).then_some(&page.omitted)
        });
        let too_large = "The children between the page's first and `next` that no page lists: \
                         one whose record passes a page of its own even with its list left \
                         out, or whose address takes more than a quarter of the result.";
        fields.optional("too_large", too_large, &Count, |page| {
            (page.too_large > 0).then_some(&page.too_large)
        });
        let next = "The address of the first child after the page: `from` lists from it.";
        fields.optional("next", next, &Str, |page| page.next.as_deref());
    }
}

/// The page of `level` that starts at the child whose address is `from`,
/// or at the first.
pub(super) fn page(level: Level, from: Option<&str>) -> Outcome<Page> {
    let Level {
        children,
        decisions,
        decisions_in_no_child,
    } = level;
    let total = children.len();
    let start = start(&children, from)?;
    let mut page = Page {
        total,
        decisions,
        decisions_in_no_child,
        children: Vec::new(),
        omitted: 0,
        too_large: 0,
        next: None,
    };
    let mut pending = rooms(&page, children.into_iter().skip(start).rev().collect());
    let filled = fill(&mut pending, fitted, |child, previous| {
        json_len(&to_value(child)) + usize::from(previous.is_some())
    });
    page.children = filled.listed;
    page.too_large = filled.too_large;
    page.omitted = pending.len();
    page.next = pending.pop().map(|(child, _)| child.address);
    Ok(page)
}

/// What a page lists of the items a walk offers it.
pub(super) struct Filled<T> {
    /// In the walk's order.
    pub(super) listed: Vec<T>,
    /// Those left out, as none fits a page.
    pub(super) too_large: usize,
}

/// The items `pending` holds, the next on top, each with the bytes the
/// items of a page that starts at it may take, taken while they fit the
/// room of the first. Each is taken as `fitted` makes it fit its own room,
/// and `bytes` measures it as JSON after the item listed before it, its
/// comma included. One `fitted` gives none for is counted too large. The
/// first item that does not fit stays on top of `pending`.
pub(super) fn fill<T>(
    pending: &mut Vec<(T, usize)>,
    fitted: impl Fn(T, usize) -> Option<T>,
    bytes: impl Fn(&T, Option<&T>) -> usize,
) -> Filled<T> {
    let room = pending.last().map_or(0, |(_, room)| *room);
    let mut filled = Filled {
        listed: Vec::new(),
        too_large: 0,
    };
    let mut used = 0;
    while let Some((item, own)) = pending.pop() {
        let Some(item) = fitted(item, own) else {
            filled.too_large += 1;
            continue;
        };
        let taken = bytes(&item, filled.listed.last());
        if used + taken > room {
            pending.push((item, own));
            break;
        }
        used += taken;
        filled.listed.push(item);
    }
    filled
}

/// Where a page of `total` items in a stated order starts: at `from`, the
/// argument `from_position`, a previous page's `next`, or at the first. A
/// position past the last item is refused, but 0 on an empty order.
pub(super) fn position(from: Option<usize>, total: usize) -> Outcome<usize> {
    match from {
        None | Some(0) => Ok(0),
        Some(at) if at < total => Ok(at),
        Some(at) => {
            let message = format!(
                "{at} is past the {total} items of this result: a page's `next` names where the \
                 next starts"
            );
            Err(ToolError::invalid("from_position", message))
        }
    }
}

/// The place in `children` of the child whose address is `from`; the
/// first's without it.
fn start(children: &[Child], from: Option<&str>) -> Outcome<usize> {
    let Some(from) = from else {
        return Ok(0);
    };
    let at = children.iter().position(|child| child.address == from);
    at.ok_or_else(|| {
        let message = format!(
            "`{from}` is no child of this level: a page's `next` names where the next starts"
        );
        ToolError::invalid("from", message)
    })
}

/// Each of `pending`, the children from a page's first on in reverse
/// order, with the bytes the children of a page that starts at it may
/// take. That is the result less `empty`, a page with no child, with its
/// notes at their largest and the longest address of a child from it on
/// that `next` can name. One pass from the last child.
fn rooms(empty: &Page, pending: Vec<Child>) -> Vec<(Child, usize)> {
    let noted = Page {
        total: empty.total,
        decisions: empty.decisions,
        decisions_in_no_child: empty.decisions_in_no_child,
        children: Vec::new(),
        omitted: usize::MAX,
        too_large: usize::MAX,
        next: Some(String::new()),
    };
    let frame = json_len(&to_value(&noted));
    let mut longest = 0;
    let rooms = pending.into_iter().map(|child| {
        let address = json_len(&Value::from(child.address.as_str()));
        if address <= MAX_NEXT_BYTES {
            longest = longest.max(address);
        }
        let room = MAX_TOOL_RESULT_BYTES.saturating_sub(frame + longest);
        (child, room)
    });
    rooms.collect()
}

/// `child` as a page lists it within `room` bytes: whole, or with its list
/// left out and counted, a crate's dependencies or a module's or a file's
/// imports into its siblings. `None` when it passes `room` even so, or when
/// its address takes more than [`MAX_NEXT_BYTES`].
fn fitted(mut child: Child, room: usize) -> Option<Child> {
    if json_len(&Value::from(child.address.as_str())) > MAX_NEXT_BYTES {
        return None;
    }
    let fits = |child: &Child| json_len(&to_value(child)) <= room;
    if fits(&child) {
        return Some(child);
    }
    let imports = match (&mut child.module, &mut child.file) {
        (Some(module), _) => Some(&mut module.imports),
        (None, Some(file)) => Some(&mut file.imports),
        (None, None) => None,
    };
    if let Some(imports) = imports {
        imports.omitted = std::mem::take(&mut imports.siblings).len();
    }
    let facts = child.crate_facts.as_mut();
    if let Some(tables) = facts.and_then(|facts| facts.dependencies.as_mut()) {
        let Dependencies {
            normal, dev, build, ..
        } = std::mem::take(tables);
        tables.omitted = normal.len() + dev.len() + build.len();
    }
    fits(&child).then_some(child)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::code::CodeTree;
    use crate::sources::{
        ChildKind, CrateFacts, Definitions, Dependency, EndpointKind, FileFacts, FilePlace,
        ImportEdge, Imports, Lines, ManifestState,
    };
    use crate::store::Decision;
    use crate::store::corpus::Corpus;
    use crate::tools::result::ToolResult;

    /// Each page of `level` as a call's text, from the first page to the
    /// last, each read back.
    fn pages(level: &Level) -> Vec<(String, Value)> {
        let mut pages = Vec::new();
        let mut from: Option<String> = None;
        loop {
            let page = page(level.clone(), from.as_deref()).unwrap();
            let text = ToolResult::structured(to_value(&page)).text().to_owned();
            from.clone_from(&page.next);
            pages.push((text, to_value(&page)));
            if from.is_none() {
                return pages;
            }
        }
    }

    /// Every level of `tree`, its pages walked from `next` to `next`: each
    /// page fits the budget uncut and counts the children after it, and
    /// the pages list each child once, in the level's order. How many
    /// levels, and the most pages one level took.
    fn walk(tree: &CodeTree<'_>, decisions: &[Decision]) -> (usize, usize) {
        let (mut levels, mut most) = (0, 0);
        let mut pending = vec![None];
        while let Some(node) = pending.pop() {
            let level = crate::sources::level_in(tree, decisions, node.as_deref()).unwrap();
            let pages = pages(&level);
            let mut listed = Vec::new();
            for (text, page) in &pages {
                assert!(
                    text.len() <= MAX_TOOL_RESULT_BYTES,
                    "{node:?}: {}",
                    text.len()
                );
                assert_eq!(page.get("truncated"), None, "{node:?}");
                assert_eq!(page.get("too_large"), None, "{node:?}");
                let children = page["children"].as_array().unwrap();
                assert!(
                    !children.is_empty() || level.children.is_empty(),
                    "{node:?}"
                );
                listed.extend(children.iter().map(|child| child["address"].clone()));
                let after = level.children.len() - listed.len();
                let omitted = page.get("omitted").map_or(0, |n| n.as_u64().unwrap());
                assert_eq!(omitted, after as u64, "{node:?}");
            }
            let addresses: Vec<Value> = level
                .children
                .iter()
                .map(|child| child.address.as_str().into())
                .collect();
            assert_eq!(listed, addresses, "{node:?}");
            levels += 1;
            most = most.max(pages.len());
            let below = level.children.into_iter();
            let below = below.filter(|child| child.kind != ChildKind::File);
            pending.extend(below.map(|child| Some(child.address)));
        }
        (levels, most)
    }

    #[test]
    fn every_level_of_this_repository_pages_within_the_budget_each_child_once() {
        let (facts, _) = crate::sources::testing::this_repository();

        let (levels, _) = walk(&CodeTree::of(facts), &[]);

        assert!(levels > 100, "{levels}");
    }

    #[test]
    fn every_level_of_a_corpus_crate_that_pages_pages_within_the_budget_each_child_once() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(Corpus::SIZES[1]).write(tmp.path()).unwrap();
        let state = store.load_state().unwrap();
        let facts = crate::sources::walked(&store);
        let decisions: Vec<Decision> = state.decisions.into_values().collect();

        let (levels, most) = walk(&CodeTree::of(&facts), &decisions);

        assert!(levels > 2, "{levels}");
        assert!(most > 1, "the largest level fits one page");
    }

    /// A level of `children` with no decision.
    fn level(children: Vec<Child>) -> Level {
        Level {
            children,
            decisions: DecisionCounts::default(),
            decisions_in_no_child: DecisionCounts::default(),
        }
    }

    /// A file child at `address` of `imports` imports, each into a node of
    /// a long address.
    fn file(address: &str, imports: usize) -> Child {
        let to = |n: usize| format!("file:{}/{n}.rs", "dir".repeat(20));
        let siblings = (0..imports).map(|n| ImportEdge {
            to: to(n),
            count: 1,
            reexports: 0,
            test_only: false,
        });
        Child {
            address: address.to_owned(),
            kind: ChildKind::File,
            name: address.to_owned(),
            lines: Lines::default(),
            definitions: Definitions::default(),
            decisions: DecisionCounts::default(),
            unresolved_imports: 0,
            crate_facts: None,
            module: None,
            file: Some(FileFacts {
                place: FilePlace::InModule,
                doc: None,
                imports: Imports {
                    siblings: siblings.collect(),
                    ..Imports::default()
                },
            }),
        }
    }

    #[test]
    fn a_child_that_passes_a_page_alone_is_listed_with_its_imports_left_out_and_counted() {
        let children = vec![file("file:a", 3), file("file:b", 2000), file("file:c", 1)];

        let pages = pages(&level(children));

        let shown: Vec<Vec<(&str, usize, u64)>> = pages
            .iter()
            .map(|(text, page)| {
                assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{}", text.len());
                let children = page["children"].as_array().unwrap().iter();
                let shown = children.map(|child| {
                    let imports = &child["file"]["imports"];
                    let listed = imports["siblings"].as_array().unwrap().len();
                    let omitted = imports.get("omitted").map_or(0, |n| n.as_u64().unwrap());
                    (child["address"].as_str().unwrap(), listed, omitted)
                });
                shown.collect()
            })
            .collect();
        assert_eq!(
            shown,
            [vec![
                ("file:a", 3, 0),
                ("file:b", 0, 2000),
                ("file:c", 1, 0)
            ]]
        );
    }

    #[test]
    fn a_crate_that_passes_a_page_alone_is_listed_with_its_dependencies_left_out_and_counted() {
        let dependency = |n: usize| Dependency {
            endpoint: EndpointKind::Unwalked,
            value: format!("../{}/{n}", "far".repeat(20)),
        };
        let mut big = file("crate:big", 0);
        big.kind = ChildKind::Crate;
        big.file = None;
        big.crate_facts = Some(CrateFacts {
            manifest: ManifestState::Read,
            package: None,
            dependencies: Some(Dependencies {
                normal: (0..300).map(dependency).collect(),
                dev: (0..300).map(dependency).collect(),
                ..Dependencies::default()
            }),
        });
        let children = vec![big, file("file:c", 1)];

        let pages = pages(&level(children));

        assert_eq!(pages.len(), 1);
        let (text, page) = &pages[0];
        assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{}", text.len());
        let big = &page["children"][0]["crate"]["dependencies"];
        assert_eq!(big["omitted"], 600, "{big}");
        assert_eq!(
            [&big["normal"], &big["dev"]],
            [&Value::Array(Vec::new()); 2]
        );
        assert_eq!(page["children"][1]["address"], "file:c");
    }

    /// Each page of `level`, each within the budget: the first 12 bytes of
    /// each address it lists, its `too_large` and its `next`.
    fn spelled(level: &Level) -> Vec<(Vec<String>, u64, Option<String>)> {
        let pages = pages(level).into_iter().map(|(text, page)| {
            assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{}", text.len());
            let children = page["children"].as_array().unwrap().iter();
            let listed = children.map(|child| {
                let address = child["address"].as_str().unwrap();
                address.chars().take(12).collect()
            });
            let too_large = page.get("too_large").map_or(0, |n| n.as_u64().unwrap());
            let next = page
                .get("next")
                .map(|next| next.as_str().unwrap().to_owned());
            (listed.collect(), too_large, next)
        });
        pages.collect()
    }

    /// A file of a 13 KiB address of control characters, too long for
    /// `next` to name.
    fn unnamed() -> Child {
        file(&format!("file:{}", "\u{1}".repeat(13 * 1024)), 0)
    }

    #[test]
    fn a_child_too_large_for_any_page_is_counted_and_the_page_lists_on_past_it() {
        let children = vec![file("file:a", 0), unnamed(), file("file:c", 0)];

        let pages = spelled(&level(children));

        let a = |name: &str| vec![name.to_owned()];
        let listed = [a("file:a"), a("file:c")].concat();
        assert_eq!(pages, [(listed, 1, None)]);
    }

    #[test]
    fn a_page_that_fills_past_a_child_too_large_for_any_page_names_the_child_after_it() {
        let children = vec![file("file:a", 180), unnamed(), file("file:c", 120)];

        let pages = spelled(&level(children));

        let a = |name: &str| vec![name.to_owned()];
        assert_eq!(
            pages,
            [
                (a("file:a"), 1, Some("file:c".to_owned())),
                (a("file:c"), 0, None)
            ]
        );
    }

    #[test]
    fn a_child_that_fits_a_page_of_its_own_starts_one_after_a_child_of_a_long_address() {
        let deep = format!("crate:{}", ["\u{1}"; 200].concat().repeat(5));
        let mut big = file("crate:y", 0);
        big.kind = ChildKind::Crate;
        big.file = None;
        big.crate_facts = Some(CrateFacts {
            manifest: ManifestState::Read,
            package: Some("p".repeat(20_000)),
            dependencies: Some(Dependencies::default()),
        });
        let children = vec![file(&deep, 0), big];

        let pages = spelled(&level(children));

        let first: String = deep.chars().take(12).collect();
        assert_eq!(
            pages,
            [
                (vec![first], 0, Some("crate:y".to_owned())),
                (vec!["crate:y".to_owned()], 0, None)
            ]
        );
    }

    #[test]
    fn a_page_from_an_address_that_is_no_child_of_the_level_is_refused_naming_it() {
        let refused = page(level(vec![file("file:a", 0)]), Some("file:gone"))
            .err()
            .unwrap();

        let payload = serde_json::to_value(&refused).unwrap();
        assert_eq!(payload["kind"], "invalid_argument");
        assert_eq!(payload["field"], "from");
        assert!(
            payload["message"].as_str().unwrap().contains("`file:gone`"),
            "{payload}"
        );
    }

    #[test]
    fn a_position_past_the_last_item_is_refused_naming_from_position_but_0_of_none() {
        let past = position(Some(2), 2).err().unwrap();

        let payload = serde_json::to_value(&past).unwrap();
        assert_eq!(
            [&payload["kind"], &payload["field"]],
            ["invalid_argument", "from_position"]
        );
        assert_eq!(
            [position(Some(1), 2), position(Some(0), 0)].map(Result::ok),
            [Some(1), Some(0)]
        );
    }
}
