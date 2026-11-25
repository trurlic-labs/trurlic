//! The general resolver: the definitions of a file of any language a
//! query file describes (`language.rs`, `query.rs`), in one walk of its
//! tree and one sweep of what the walk found.
//!
//! The walk visits each node once, from an explicit cursor, and starts
//! the query at each named node with a start depth of 0, so a pattern is
//! matched only where it is rooted: a cursor run once over the whole tree
//! goes quadratic on the deep error trees of hostile input. A query match
//! limit that is hit is a fault, never a dropped match, and the text its
//! predicates read is charged to a budget per byte. The walk keeps its
//! own frame for each ancestor of the node it is at (its kind, its span,
//! whether a test mark holds it, the innermost visibility mark, and how
//! far each span growth and wrap reaches from it), so no parent is read
//! through tree-sitter, which finds it by a descent from the root. A capture names a node at or below the match's root, which the
//! walk reaches later or now: what it captures waits by node until then.
//!
//! The sweep places the definitions by span: sorted by start, the widest
//! first, each is under the innermost open container whose span holds it,
//! and two of one span (the names of one statement) are siblings. A
//! definition's path is its container's and its name.

use std::collections::BTreeMap;
use std::iter;

use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use super::index::{FileIndex, IndexFault};
use super::kinds::{Role, Visibility, What};
use super::language::Language;
use super::query::{Capture, Container, Definer, Rules};
use super::scope::{ScopeId, ScopeTree};
use super::symbol::Segment;
use super::syntax::{self, Leaves, NodeMarks};
use super::tree::TreeIndex;
use super::work::{SCAN_WORK_PER_BYTE, Work};

/// The matches a query may have in progress at once. A file that needs
/// more is refused, not read in part.
pub(super) const MATCH_LIMIT: u32 = 256;

/// The index of `text`, a file of `language`.
pub(super) fn index(language: &Language, text: &str) -> Result<FileIndex, IndexFault> {
    if let Some(bound) = language.before_parse {
        bound(text)?;
    }
    // Each language's query is compiled against the contract in its
    // test, so a fault here is a build's, which no file's bytes cause and
    // whose cause the test names.
    let Ok(rules) = language.rules() else {
        return Err(IndexFault::Grammar);
    };
    let tree = syntax::parse(text, &rules.grammar)?;
    let found = collect(rules, &tree, text)?;
    let mut scopes = ScopeTree::for_file(text.len());
    let placed = place(&found, text, &mut scopes)?;
    let mut marks = found.marks;
    let mut starts = Vec::with_capacity(placed.len());
    for definition in &placed {
        marks.entry(definition.span.id).or_default().span = true;
        starts.push(definition.span.start);
    }
    starts.sort_unstable();
    let (leaves, units) = Leaves::of_marked(&tree, (text, &language.lexicon), &marks, &starts);
    let mut build = TreeIndex::new(text, leaves, scopes);
    for definition in placed {
        let span = definition.span;
        // A span the query leaves out of every hash holds no unit.
        let held = units.get(&span.id).cloned().unwrap_or_default();
        build.define_units(
            definition.scope,
            &(span.start..span.end),
            held,
            definition.what,
        )?;
    }
    Ok(build.finish())
}

/// A node a definition's span is: its id and its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpanNode {
    id: usize,
    start: usize,
    end: usize,
}

impl SpanNode {
    /// Whether its bytes hold `start..end`.
    const fn holds(&self, start: usize, end: usize) -> bool {
        self.start <= start && end <= self.end
    }
}

/// A node a `@visibility.<name>` capture marks: its bytes and the
/// visibility.
#[derive(Debug, Clone, Copy)]
struct VisibilityMark {
    start: usize,
    end: usize,
    visibility: Visibility,
}

/// What the walk found of a definition's name node.
#[derive(Debug, Clone, Copy)]
struct NameNode {
    start: usize,
    end: usize,
    test: bool,
    /// The innermost visibility mark that holds it.
    visibility: Option<VisibilityMark>,
}

/// A definition pattern's match, completed as the walk reaches the
/// nodes it captured.
#[derive(Debug, Clone)]
struct Candidate<'r> {
    definer: &'r Definer,
    span: Option<SpanNode>,
    name: Option<NameNode>,
}

/// What a captured node waits for the walk to reach it with.
#[derive(Debug, Clone, Copy)]
enum Waiting {
    Definition(usize),
    Name(usize),
    Test,
    Visibility(Visibility),
}

/// What the walk keeps of an ancestor of the node it is at.
#[derive(Debug, Clone, Copy)]
struct Frame {
    kind: u16,
    node: SpanNode,
    test: bool,
    visibility: Option<VisibilityMark>,
}

/// What the walk found: the candidates, and how the hash takes each
/// marked node, by its id.
struct Found<'r> {
    candidates: Vec<Candidate<'r>>,
    marks: BTreeMap<usize, NodeMarks>,
}

impl<'r> Found<'r> {
    /// The span, name and definer of each candidate the walk completed,
    /// by span start, the widest first, then in the order matched.
    fn complete(&self) -> Vec<(SpanNode, NameNode, &'r Definer)> {
        let mut complete: Vec<(SpanNode, NameNode, &Definer)> = self
            .candidates
            .iter()
            .filter_map(|candidate| Some((candidate.span?, candidate.name?, candidate.definer)))
            .collect();
        complete.sort_by_key(|(span, _, _)| (span.start, std::cmp::Reverse(span.end)));
        complete
    }
}

impl NameNode {
    /// Test code when a test mark holds it.
    const fn role(&self) -> Role {
        match self.test {
            true => Role::Test,
            false => Role::Code,
        }
    }
}

/// The walk's state: the frames of the ancestors, the reach of each
/// growth and wrap from each (`reach`, a row of the rules' growths then
/// wraps per frame, the depth of the node reached), and what waits.
struct Walk<'r> {
    rules: &'r Rules,
    frames: Vec<Frame>,
    reach: Vec<Option<usize>>,
    waiting: BTreeMap<usize, Vec<Waiting>>,
    found: Found<'r>,
}

/// Each node of `tree` visited once, the query started at each named one.
fn collect<'r>(rules: &'r Rules, tree: &Tree, text: &str) -> Result<Found<'r>, IndexFault> {
    let mut walk = Walk {
        rules,
        frames: Vec::new(),
        reach: Vec::new(),
        waiting: BTreeMap::new(),
        found: Found {
            candidates: Vec::new(),
            marks: BTreeMap::new(),
        },
    };
    let mut matches = Matches {
        cursor: QueryCursor::new(),
        work: Work::of(text.len(), SCAN_WORK_PER_BYTE),
        text,
    };
    matches.cursor.set_max_start_depth(Some(0));
    matches.cursor.set_match_limit(MATCH_LIMIT);
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_named() {
            matches.record_at(node, &mut walk)?;
        }
        walk.enter(node);
        if cursor.goto_first_child() {
            continue;
        }
        walk.leave();
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(walk.found);
            }
            walk.leave();
        }
    }
}

/// The query's cursor and the budget of the text its predicates read.
struct Matches<'t> {
    cursor: QueryCursor,
    work: Work,
    text: &'t str,
}

impl Matches<'_> {
    /// Record each match rooted at `node`; [`IndexFault::TooComplex`] when
    /// the match limit is hit or the predicates' text runs past its
    /// budget.
    fn record_at(&mut self, node: Node<'_>, walk: &mut Walk<'_>) -> Result<(), IndexFault> {
        let bytes = self.text.as_bytes();
        let mut over = false;
        let work = &mut self.work;
        let read = |captured: Node<'_>| {
            let range = captured.byte_range();
            over |= work.spend(range.len()).is_err();
            iter::once(match over {
                true => &[][..],
                false => bytes.get(range).unwrap_or_default(),
            })
        };
        let rules = walk.rules;
        let mut matches = self.cursor.matches(&rules.query, node, read);
        while let Some(found) = matches.next() {
            walk.record(found.pattern_index, found.captures());
        }
        drop(matches);
        match over || self.cursor.did_exceed_match_limit() {
            true => Err(IndexFault::TooComplex),
            false => Ok(()),
        }
    }
}

impl Walk<'_> {
    /// Record a match of pattern `pattern` that captured `captures`.
    fn record(&mut self, pattern: usize, captures: &[tree_sitter::QueryCapture<'_>]) {
        let (mut definition, mut name) = (None, None);
        for capture in captures {
            let id = capture.node.id();
            let what = self.rules.captures.get(capture.index as usize);
            let mut wait = |waiting| self.waiting.entry(id).or_default().push(waiting);
            match what.copied().unwrap_or(Capture::Predicate) {
                Capture::Definition => definition = Some(id),
                Capture::Name => name = Some(id),
                Capture::Test => wait(Waiting::Test),
                Capture::Visibility(visibility) => wait(Waiting::Visibility(visibility)),
                Capture::Unit => self.found.marks.entry(id).or_default().whole = true,
                Capture::Skip => self.found.marks.entry(id).or_default().skip = true,
                Capture::Group => self.found.marks.entry(id).or_default().group = true,
                Capture::Predicate => {}
            }
        }
        let definer = self.rules.patterns.get(pattern).and_then(Option::as_ref);
        if let (Some(definition), Some(name), Some(definer)) = (definition, name, definer) {
            let at = self.found.candidates.len();
            self.found.candidates.push(Candidate {
                definer,
                span: None,
                name: None,
            });
            let mut wait = |id, waiting| self.waiting.entry(id).or_default().push(waiting);
            wait(definition, Waiting::Definition(at));
            wait(name, Waiting::Name(at));
        }
    }

    /// Enter `node`: its frame, from its parent's and what waited for it,
    /// and the candidates that captured it completed.
    fn enter(&mut self, node: Node<'_>) {
        let waiting = self.waiting.remove(&node.id()).unwrap_or_default();
        let parent = self.frames.last().copied();
        let frame = frame_of(node, parent, &waiting);
        self.push_reach(frame.kind, parent.map(|parent| parent.kind));
        self.frames.push(frame);
        for waited in waiting {
            match waited {
                Waiting::Definition(at) => self.complete_span(at),
                Waiting::Name(at) => {
                    if let Some(candidate) = self.found.candidates.get_mut(at) {
                        candidate.name = Some(NameNode {
                            start: frame.node.start,
                            end: frame.node.end,
                            test: frame.test,
                            visibility: frame.visibility,
                        });
                    }
                }
                Waiting::Test | Waiting::Visibility(_) => {}
            }
        }
    }

    /// Push the reach of each growth and wrap from a node of `kind` under
    /// one of `parent`: a growth reaches a node of a `to` kind itself,
    /// through one of a `via` kind its parent's reach; a wrap reaches the
    /// outermost node of its kinds in the run of them up from the node.
    fn push_reach(&mut self, kind: u16, parent: Option<u16>) {
        let depth = self.frames.len();
        let rules = self.rules;
        for (at, growth) in rules.growths.iter().enumerate() {
            let reached = match (growth.to.contains(&kind), growth.via.contains(&kind)) {
                (true, _) => Some(depth),
                (false, true) => depth.checked_sub(1).and_then(|up| self.reach_of(up, at)),
                (false, false) => None,
            };
            self.reach.push(reached);
        }
        for (at, wrap) in rules.wraps.iter().enumerate() {
            let continued = parent.is_some_and(|parent| wrap.contains(&parent));
            let reached = match (wrap.contains(&kind), continued) {
                (true, true) => depth
                    .checked_sub(1)
                    .and_then(|up| self.reach_of(up, rules.growths.len() + at)),
                (true, false) => Some(depth),
                (false, _) => None,
            };
            self.reach.push(reached);
        }
    }

    /// The reach of growth or wrap `at` (counted over the growths, then
    /// the wraps) from the frame at `depth`.
    fn reach_of(&self, depth: usize, at: usize) -> Option<usize> {
        let width = self.rules.growths.len() + self.rules.wraps.len();
        self.reach.get(depth * width + at).copied().flatten()
    }

    /// Complete candidate `at`'s span: the node the walk is at, grown and
    /// wrapped as its pattern says; none when its growth reaches nothing.
    fn complete_span(&mut self, at: usize) {
        let Some(definer) = self
            .found
            .candidates
            .get(at)
            .map(|candidate| candidate.definer)
        else {
            return;
        };
        let Some(depth) = self.frames.len().checked_sub(1) else {
            return;
        };
        let grown = match definer.growth {
            None => Some(depth),
            Some(growth) => self.frames.get(depth).and_then(|frame| {
                let to = &self.rules.growths.get(growth)?.to;
                match to.contains(&frame.kind) {
                    true => Some(depth),
                    false => self.reach_of(depth.checked_sub(1)?, growth),
                }
            }),
        };
        let wrapped = grown.map(|grown| match definer.wrap {
            None => grown,
            Some(wrap) => grown
                .checked_sub(1)
                .and_then(|up| self.reach_of(up, self.rules.growths.len() + wrap))
                .unwrap_or(grown),
        });
        let span = wrapped
            .and_then(|depth| self.frames.get(depth))
            .map(|frame| frame.node);
        if let Some(candidate) = self.found.candidates.get_mut(at) {
            candidate.span = span;
        }
    }

    /// Leave the node the walk is at.
    fn leave(&mut self) {
        self.frames.pop();
        let width = self.rules.growths.len() + self.rules.wraps.len();
        self.reach.truncate(self.frames.len() * width);
    }
}

/// The frame of `node` under `parent`: test code when its parent is or a
/// test mark waited for it, and under its own visibility mark or else its
/// parent's innermost.
fn frame_of(node: Node<'_>, parent: Option<Frame>, waiting: &[Waiting]) -> Frame {
    let span = SpanNode {
        id: node.id(),
        start: node.start_byte(),
        end: node.end_byte(),
    };
    let mut frame = Frame {
        kind: node.kind_id(),
        node: span,
        test: parent.is_some_and(|parent| parent.test),
        visibility: parent.and_then(|parent| parent.visibility),
    };
    for waited in waiting {
        match *waited {
            Waiting::Test => frame.test = true,
            Waiting::Visibility(visibility) => {
                frame.visibility = Some(VisibilityMark {
                    start: span.start,
                    end: span.end,
                    visibility,
                });
            }
            Waiting::Definition(_) | Waiting::Name(_) => {}
        }
    }
    frame
}

/// A definition placed in its file: its scope, its span node and what it
/// is.
struct Placed {
    scope: ScopeId,
    span: SpanNode,
    what: What,
}

/// The visibility the innermost mark that holds `name` states, when
/// `span` holds the mark.
fn visibility_of(name: &NameNode, span: SpanNode) -> Option<Visibility> {
    name.visibility
        .filter(|mark| span.holds(mark.start, mark.end))
        .map(|mark| mark.visibility)
}

/// A container open in the sweep: its span, kind and scope.
struct Open {
    span: SpanNode,
    container: Container,
    scope: ScopeId,
}

/// The definitions of `found`, each under the innermost container whose
/// span holds its own, its scope entered in `scopes`.
fn place(found: &Found<'_>, text: &str, scopes: &mut ScopeTree) -> Result<Vec<Placed>, IndexFault> {
    let mut open: Vec<Open> = Vec::new();
    let mut placed = Vec::new();
    for (span, name, definer) in found.complete() {
        // Two definitions of one span are siblings.
        while open.last().is_some_and(|top| {
            !top.span.holds(span.start, span.end)
                || (top.span.start, top.span.end) == (span.start, span.end)
        }) {
            open.pop();
        }
        let container = open.last().map_or(Container::Top, |top| top.container);
        let spelled = text.get(name.start..name.end).unwrap_or_default();
        if !definer.admits(container) || spelled.trim().is_empty() {
            continue;
        }
        let kind = definer.kind_in(container);
        let parent = open.last().map(|top| top.scope);
        let scope = scopes.enter(parent, vec![Segment::name(spelled)])?;
        if definer.container {
            open.push(Open {
                span,
                container: Container::Kind(kind),
                scope,
            });
        }
        let what = What {
            kind,
            role: name.role(),
            visibility: visibility_of(&name, span).or(definer.visibility),
        };
        placed.push(Placed { scope, span, what });
    }
    Ok(placed)
}

#[cfg(test)]
mod tests;
