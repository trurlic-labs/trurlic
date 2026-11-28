//! A search of the code tree: its spec, its arguments, what it shows, and
//! how its hits are cut into pages.
//!
//! A page lists the hits from a position, in their rank, while they fit
//! beside what it leaves out, and `next` names the position of the first
//! hit after it, so a walk from `next` to `next` lists each hit once. A
//! hit that passes a page alone is listed by no page and counted in
//! `too_large`.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::sources::{
    self, DEFINITION_KINDS, Hit, HitDefinition, HitKind, Match, Query, Search, Window,
};
use crate::store::{ProjectState, Store};
use crate::tools::spec::kinds::{Position, QueryText};
use crate::tools::spec::output::{Count, Items, Nested, Str, Tag, Texts};
use crate::tools::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use crate::tools::spec::{Args, Fields, Output, Shown, to_value};
use crate::tools::truncate::json_len;

use super::page::{fill, position};

/// The fewest bytes a hit takes on a page: the three keys every hit holds
/// with empty values, and a comma; a kind's and a match's names are
/// longer. A page lists at most its room over this many hits.
const MIN_HIT_BYTES: usize = r#"{"kind":"","match":"","name":""},"#.len();

pub(crate) static SEARCH: ToolSpec<SearchArgs, SearchPage> = ToolSpec {
    name: "map_search",
    title: "Map: a search",
    description: DESCRIPTION,
    destructive: Destructive::No,
    access: Access::Read(search),
};

const DESCRIPTION: &str = "The nodes and definitions of the code tree whose name or path holds \
`query`: an exact name first, then a name's prefix, then a path's substring, the shorter path \
first, then by path; paged from `from_position`.";

/// The arguments of `map_search`, and of `trurlic map --search`.
#[derive(Debug)]
pub(crate) struct SearchArgs {
    query: Query,
    from_position: Option<usize>,
}

impl Args for SearchArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        Self {
            query: fields.required(
                "query",
                "The text to find, ASCII letters of either case alike: 1 to 256 bytes of UTF-8, \
                 which the lengths here count in characters and so state exactly for ASCII \
                 alone, and no control character (U+0000 to U+001F, U+007F to U+009F).",
                &QueryText,
            ),
            from_position: fields.optional(
                "from_position",
                "The position of the first hit to list: a previous page's `next`.",
                &Position,
            ),
        }
    }
}

/// What `map_search` answers: a page of a search's hits.
pub(crate) struct SearchPage {
    /// Every hit, on every page.
    total: usize,
    /// The page's, in rank.
    hits: Vec<Hit>,
    /// The hits after the page.
    omitted: usize,
    /// The hits before `next` that no page lists.
    too_large: usize,
    /// The position of the first hit after the page.
    next: Option<usize>,
}

/// The page of the hits of `arguments.query` that starts at
/// `arguments.from_position`.
fn search(store: &Store, _: &ProjectState, arguments: SearchArgs) -> Outcome<SearchPage> {
    let SearchArgs {
        query,
        from_position,
    } = arguments;
    page(from_position, |window| {
        sources::search(store, &query, window)
    })
}

/// The page of a search's hits from the position `from`, `found` giving
/// the hits of a window.
fn page(from: Option<usize>, found: impl FnOnce(Window) -> Search) -> Outcome<SearchPage> {
    let empty = SearchPage {
        total: usize::MAX,
        hits: Vec::new(),
        omitted: usize::MAX,
        too_large: usize::MAX,
        next: Some(usize::MAX),
    };
    let room = MAX_TOOL_RESULT_BYTES.saturating_sub(json_len(&to_value(&empty)));
    let found = found(Window {
        from: from.unwrap_or(0),
        most: room / MIN_HIT_BYTES,
    });
    let start = position(from, found.total)?;
    let pending = found.hits.into_iter().rev().map(|hit| (hit, room));
    let mut pending: Vec<(Hit, usize)> = pending.collect();
    let filled = fill(&mut pending, fitted, |hit, previous| {
        json_len(&to_value(hit)) + usize::from(previous.is_some())
    });
    let after = start + filled.listed.len() + filled.too_large;
    Ok(SearchPage {
        total: found.total,
        hits: filled.listed,
        omitted: found.total - after,
        too_large: filled.too_large,
        next: (after < found.total).then_some(after),
    })
}

/// `hit` when it fits `room` bytes.
fn fitted(hit: Hit, room: usize) -> Option<Hit> {
    (json_len(&to_value(&hit)) <= room).then_some(hit)
}

impl Output for SearchPage {
    fn declare(fields: &mut Shown<'_, Self>) {
        let total = "Every hit of the query, on every page.";
        fields.required("total", total, &Count, |page| &page.total);
        let hits = "The page's hits, in rank: by `match`, the better first, then the shorter \
                    path, then the path's bytes; then a node before a definition, nodes by \
                    address, definitions by file, lines and kind.";
        let kind = Items {
            item: Nested::<Hit>::new(),
        };
        fields.required("hits", hits, &kind, |page| &page.hits);
        let omitted = "The hits after the page, when it leaves any out.";
        fields.optional("omitted", omitted, &Count, |page| {
            (page.omitted > 0).then_some(&page.omitted)
        });
        let too_large = "The hits between the page's first and `next` that no page lists, as \
                         one passes a page alone.";
        fields.optional("too_large", too_large, &Count, |page| {
            (page.too_large > 0).then_some(&page.too_large)
        });
        let next = "The position of the first hit after the page: `from_position` lists from \
                    it.";
        fields.optional("next", next, &Count, |page| page.next.as_ref());
    }
}

impl Output for Hit {
    fn declare(fields: &mut Shown<'_, Self>) {
        let address = "Present on a node's hit alone: its address, `node` of a level.";
        fields.optional("address", address, &Str, |hit| hit.address.as_deref());
        let kind = Tag {
            all: &HitKind::ALL,
            name: HitKind::as_str,
        };
        let about = "A node's kind, as a level names it, or `definition`, with the key \
                     `definition`.";
        fields.required("kind", about, &kind, |hit| &hit.kind);
        let name = "A node's name as a level gives it: the last segment of its path, `.` for \
                    the code root's crate, `outside_crates` for what lies outside every crate. A \
                    definition's last segment.";
        fields.required("name", name, &Str, |hit| &hit.name);
        let matched = Tag {
            all: &Match::ALL,
            name: Match::as_str,
        };
        let about = "`exact_name`: its name is the query; `name_prefix`: its name starts with \
                     it; `path_substring`: its path holds it. A node's path is a crate's, a \
                     directory's or a file's, a root module's root directory, any other \
                     module's segments joined by `::`, none for what lies outside every crate; \
                     a definition's is its segments joined by `::`.";
        fields.required("match", about, &matched, |hit| &hit.matched);
        let own = "Present on a hit of kind `definition` alone.";
        let kind = Nested::<HitDefinition>::new();
        fields.optional("definition", own, &kind, |hit| hit.definition.as_ref());
    }
}

impl Output for HitDefinition {
    fn declare(fields: &mut Shown<'_, Self>) {
        let file = "Its file's address, `file` of a frame.";
        fields.required("file", file, &Str, |definition| &definition.file);
        let kind = Tag {
            all: DEFINITION_KINDS.as_slice(),
            name: std::convert::identity,
        };
        fields.required("kind", "What it is.", &kind, |definition| &definition.kind);
        let path = "Its path's segments as the index spells them, outermost first.";
        let segments = Texts;
        fields.required("path", path, &segments, |definition| &definition.path);
        let first = "The first line of its span: where its file's frame shows it.";
        fields.required("first_line", first, &Count, |definition| {
            &definition.first_line
        });
        let last = "The last line of its span.";
        fields.required("last_line", last, &Count, |definition| {
            &definition.last_line
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::code::CodeTree;
    use crate::sources::{ChildKind, search_in};
    use crate::store::corpus::Corpus;
    use crate::tools::result::ToolResult;

    /// The hits of `query` in `tree`, its pages walked from `next` to
    /// `next`, each page fitting the budget uncut and counting the hits
    /// after it; and how many pages.
    fn walk(tree: &CodeTree<'_>, query: &Query) -> (Vec<Value>, usize) {
        let (mut hits, mut pages, mut from) = (Vec::new(), 0, None);
        loop {
            let page = page(from, |window| search_in(tree, query, window)).unwrap();
            let text = ToolResult::structured(to_value(&page)).text().to_owned();
            assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{}", text.len());
            let read: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(read.get("truncated"), None);
            assert_eq!(page.too_large, 0);
            hits.extend(read["hits"].as_array().unwrap().iter().cloned());
            assert_eq!(page.omitted, page.total - hits.len());
            pages += 1;
            from = page.next;
            if from.is_none() {
                return (hits, pages);
            }
        }
    }

    /// What orders `hit`, read from its JSON alone: its match, its path's
    /// length and bytes, a node before a definition, then its address,
    /// lines and kind. A node's path is spelled from its address.
    fn rank(hit: &Value) -> (usize, usize, String, bool, String, u64, u64, String) {
        let text = |value: &Value| value.as_str().unwrap().to_owned();
        let matched = ["exact_name", "name_prefix", "path_substring"];
        let matched = matched
            .iter()
            .position(|name| hit["match"] == *name)
            .unwrap();
        let definition = &hit["definition"];
        let address = text(hit.get("address").unwrap_or(&definition["file"]));
        let path = match definition.as_object() {
            Some(_) => {
                let segments = definition["path"].as_array().unwrap().iter().map(text);
                segments.collect::<Vec<String>>().join("::")
            }
            None => node_path(&address),
        };
        let line = |key: &str| definition.get(key).map_or(0, |n| n.as_u64().unwrap());
        let kind = definition.get("kind").map_or_else(String::new, text);
        let is_definition = definition.is_object();
        (
            matched,
            path.len(),
            path,
            is_definition,
            address,
            line("first_line"),
            line("last_line"),
            kind,
        )
    }

    /// The path a node at `address` is searched by.
    fn node_path(address: &str) -> String {
        if address == "outside_crates" {
            return String::new();
        }
        let (kind, rest) = address.split_once(':').unwrap();
        match (kind, rest.split_once('@')) {
            ("module", Some(("", root))) => root.to_owned(),
            ("module", Some((segments, _))) => segments.to_owned(),
            _ => rest.to_owned(),
        }
    }

    /// Every node of `tree` but its files' definitions: its address and
    /// name, its levels walked.
    fn nodes(tree: &CodeTree<'_>) -> Vec<(String, String)> {
        let (mut nodes, mut pending) = (Vec::new(), vec![None]);
        while let Some(holder) = pending.pop() {
            let level = crate::sources::level_in(tree, [], holder.as_deref()).unwrap();
            for child in level.children {
                if child.kind != ChildKind::File {
                    pending.push(Some(child.address.clone()));
                }
                nodes.push((child.address, child.name));
            }
        }
        nodes
    }

    /// Searches of `queries` in `tree`, paged to completion: each hit
    /// listed once, in the order one call over every hit gives, which the
    /// rank read from the JSON alone orders, with every node of the name
    /// among the exact names. The most pages one search took.
    fn search_all(tree: &CodeTree<'_>, queries: &[String]) -> usize {
        let nodes = nodes(tree);
        let mut most = 0;
        for text in queries {
            let query = Query::parse(text).unwrap();
            let (hits, pages) = walk(tree, &query);
            let whole = Window {
                from: 0,
                most: usize::MAX,
            };
            let once: Vec<Value> = search_in(tree, &query, whole)
                .hits
                .iter()
                .map(to_value)
                .collect();
            assert_eq!(hits, once, "{text}");
            let ranks: Vec<_> = hits.iter().map(rank).collect();
            assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]), "{text}");
            let exact = hits.iter().filter(|hit| hit["match"] == "exact_name");
            let exact: Vec<&Value> = exact.map(|hit| &hit["address"]).collect();
            let named = nodes
                .iter()
                .filter(|(_, name)| name.eq_ignore_ascii_case(text));
            for (address, _) in named {
                assert!(
                    exact.contains(&&Value::from(address.as_str())),
                    "{text}: {address}"
                );
            }
            most = most.max(pages);
        }
        most
    }

    /// Queries drawn from `tree`: one letter, every tenth node's name, and
    /// the name of every 97th definition the letter finds.
    fn queries(tree: &CodeTree<'_>) -> Vec<String> {
        let letter = "e".to_owned();
        let query = Query::parse(&letter).unwrap();
        let window = Window {
            from: 0,
            most: usize::MAX,
        };
        let found = search_in(tree, &query, window).hits;
        let definitions = found
            .iter()
            .filter(|hit| hit.definition.is_some())
            .step_by(97);
        let names = nodes(tree).into_iter().step_by(10).map(|(_, name)| name);
        let drawn = names.chain(definitions.map(|hit| hit.name.clone()));
        let valid = drawn.filter(|name| Query::parse(name).is_ok());
        std::iter::once(letter).chain(valid).collect()
    }

    #[test]
    fn a_search_of_this_repository_s_names_pages_each_hit_once_in_rank() {
        let (facts, _) = crate::sources::testing::this_repository();
        let tree = CodeTree::of(facts);
        let queries = queries(&tree);

        let most = search_all(&tree, &queries);

        assert!(queries.len() > 50, "{}", queries.len());
        assert!(most > 10, "the letter's hits fit {most} pages");
    }

    #[test]
    fn a_search_of_a_corpus_project_s_names_pages_each_hit_once_in_rank() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(Corpus::SIZES[1]).write(tmp.path()).unwrap();
        let facts = crate::sources::walked(&store);
        let tree = CodeTree::of(&facts);

        let most = search_all(&tree, &queries(&tree));

        assert!(most > 1, "the letter's hits fit one page");
    }
}
