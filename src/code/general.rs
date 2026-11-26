//! The general resolver: the definitions of a file of any language a
//! query file describes (`language.rs`, `query.rs`), in one walk of its
//! tree and one sweep of what the walk found.
//!
//! The walk visits each node once, from an explicit cursor, and starts
//! the query at each named node with a start depth of 0, so a pattern is
//! matched only where it is rooted: a cursor run once over the whole tree
//! goes quadratic on the deep error trees of hostile input. A query match
//! limit that is hit is a fault, never a dropped match, and the text its
//! predicates and the names read is charged to a budget per byte. The
//! walk keeps its own frame for each ancestor of the node it is at
//! (`general/frame.rs`: its kind, its span, whether a test mark holds it,
//! the innermost visibility mark of each form, the label in force for its
//! children; and here how far each
//! span growth and wrap reaches from it,
//! and the run of siblings of each preceding set right before it), so no
//! parent or sibling is read through tree-sitter, which finds one by a
//! descent from the root. A capture names a node at or below the match's root, which
//! the walk reaches later or now: what it captures waits by node until
//! then. Every match that captures a node as its `@definition` is
//! recorded by the time the walk reaches the node, so an exclusive
//! pattern's claim on it is settled there, whatever the patterns' order.
//!
//! The sweep (`general/place.rs`) places the definitions and the unnamed
//! scopes by the nodes they captured. A definition's path is its
//! container's and its name's segments (`general/name.rs`).

mod frame;
mod name;
mod place;

use std::collections::{BTreeMap, BTreeSet};
use std::iter;

use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use super::index::{FileIndex, IndexFault};
use super::kinds::{Role, Visibility};
use super::language::Language;
use super::query::{Capture, Definer, Rules};
use super::scope::ScopeTree;
use super::span::Lines;
use super::syntax;
use super::tree::TreeIndex;
use super::units::{Leaves, NodeMarks};
use super::work::{MIN_WORK, SCAN_WORK_PER_BYTE, Work};

use frame::{Frame, VisibilityMark, Waiting};

/// The matches a query may have in progress at once. A file that needs
/// more is refused, not read in part.
pub(super) const MATCH_LIMIT: u32 = 256;

/// A file as the resolver reads it: the text its grammar parses and its
/// hash reads, the text as written, of the same length, whose lines a
/// span counts, and the places a rewrite before the parse marked private,
/// in order (`language/c_family.rs`).
pub(super) struct Source<'t> {
    pub(super) parsed: &'t str,
    pub(super) written: &'t str,
    pub(super) private: &'t [usize],
}

impl<'t> Source<'t> {
    /// `text` read as written.
    pub(super) const fn plain(text: &'t str) -> Self {
        Self {
            parsed: text,
            written: text,
            private: &[],
        }
    }
}

/// The index of `source`, a file of `language`, built into `scopes`.
pub(super) fn index(
    language: &Language,
    source: &Source<'_>,
    mut scopes: ScopeTree,
) -> Result<FileIndex, IndexFault> {
    let text = source.parsed;
    for bound in language.before_parse {
        bound(text)?;
    }
    // Each language's query is compiled against the contract in its
    // test, so a fault here is a build's, which no file's bytes cause and
    // whose cause the test names.
    let Ok(rules) = language.rules() else {
        return Err(IndexFault::Grammar);
    };
    let tree = syntax::parse(text, &rules.grammar)?;
    let mut found = collect(rules, &tree, text)?;
    found.private = source.private;
    let placed = place::place(&found, &mut scopes)?;
    let mut marks = found.marks;
    for definition in &placed {
        marks.entry(definition.span.first).or_default().opens = true;
        marks.entry(definition.span.last).or_default().closes = true;
    }
    let (leaves, units) = Leaves::of_marked(&tree, (text, &language.lexicon), &marks);
    let lines = Lines::of(source.written.as_bytes());
    let mut build = TreeIndex::new(text, lines, leaves, scopes);
    for definition in placed {
        let span = definition.span;
        build.define_units(
            definition.scope,
            &(span.start..span.end),
            units.between(span.first, span.last),
            definition.what,
        )?;
    }
    Ok(build.finish())
}

/// A node the walk passed: its id and its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpanNode {
    id: usize,
    start: usize,
    end: usize,
}

impl SpanNode {
    fn of(node: Node<'_>) -> Self {
        Self {
            id: node.id(),
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }

    /// Whether its bytes hold `other`'s.
    const fn holds(&self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// A definition's span: from node `first`'s start to node `last`'s end,
/// one node, or a run of siblings its node closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    first: usize,
    last: usize,
    start: usize,
    end: usize,
}

impl Extent {
    /// Whether its bytes hold `start..end`.
    const fn holds(&self, start: usize, end: usize) -> bool {
        self.start <= start && end <= self.end
    }
}

/// What the walk found of a definition's name node: where its first one
/// starts, the text of each of its segments, whether a test mark holds
/// it, the innermost visibility mark that holds it, the visibility of the
/// innermost `@visibility.within` mark that does, and of the label in
/// force there.
#[derive(Debug, Clone)]
struct NameNode {
    start: usize,
    parts: Vec<String>,
    test: bool,
    visibility: Option<VisibilityMark>,
    within: Option<Visibility>,
    label: Option<Visibility>,
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

/// Where a definition stands: the node its pattern captured, which
/// places it, and its span.
#[derive(Debug, Clone, Copy)]
struct Site {
    node: SpanNode,
    span: Extent,
}

/// A definition pattern's match, completed as the walk reaches the
/// nodes it captured.
#[derive(Debug, Clone)]
struct Candidate<'r> {
    definer: &'r Definer,
    site: Option<Site>,
    name: Option<NameNode>,
    /// The name nodes the walk has yet to reach.
    names_left: usize,
}

/// What the walk found: the candidates, the nodes captured as unnamed
/// scopes and, by id, those captured as no scope, how the hash takes each
/// marked node, by its id, and the names an `@export.name` capture
/// spells; and the places the source marks private.
#[derive(Default)]
struct Found<'r, 't> {
    candidates: Vec<Candidate<'r>>,
    unnamed: Vec<SpanNode>,
    no_scope: BTreeSet<usize>,
    marks: BTreeMap<usize, NodeMarks>,
    exported: BTreeSet<&'t str>,
    private: &'t [usize],
}

impl<'r> Found<'r, '_> {
    /// The site, name and definer of each candidate the walk completed,
    /// in the order matched.
    fn complete(&self) -> impl Iterator<Item = (Site, &NameNode, &'r Definer)> {
        self.candidates.iter().filter_map(|candidate| {
            let name = candidate
                .name
                .as_ref()
                .filter(|_| candidate.names_left == 0)?;
            Some((candidate.site?, name, candidate.definer))
        })
    }

    /// Whether an `@export.name` capture spells the name of one segment
    /// `parts` is.
    fn is_exported(&self, parts: &[String]) -> bool {
        let [one] = parts else {
            return false;
        };
        self.exported.contains(one.as_str())
    }

    /// Whether a place the source marks private is at `start` or after it,
    /// before `name`: a definition whose span starts at `start` and whose
    /// name starts at `name` is private, as `static` would make it.
    fn is_marked_private(&self, start: usize, name: usize) -> bool {
        let at = self.private.partition_point(|place| *place < start);
        self.private.get(at).is_some_and(|place| *place < name)
    }
}

/// The walk's state: the frames of the ancestors; per frame a row of the
/// reach of each growth and wrap from it (`reach`, the rules' growths
/// then wraps, each the depth of the node reached), of the run of each
/// preceding set right before it (`before`), and of that run among its
/// children seen so far (`runs`); and what waits.
struct Walk<'r, 't> {
    rules: &'r Rules,
    text: &'t str,
    frames: Vec<Frame>,
    reach: Vec<Option<usize>>,
    before: Vec<Option<SpanNode>>,
    runs: Vec<Option<SpanNode>>,
    waiting: BTreeMap<usize, Vec<Waiting>>,
    found: Found<'r, 't>,
}

/// Each node of `tree` visited once, the query started at each named one.
fn collect<'r, 't>(
    rules: &'r Rules,
    tree: &Tree,
    text: &'t str,
) -> Result<Found<'r, 't>, IndexFault> {
    let mut walk = Walk {
        rules,
        text,
        frames: Vec::new(),
        reach: Vec::new(),
        before: Vec::new(),
        runs: Vec::new(),
        waiting: BTreeMap::new(),
        found: Found::default(),
    };
    let mut matches = Matches {
        cursor: QueryCursor::new(),
        work: Work::of(text.len(), SCAN_WORK_PER_BYTE, MIN_WORK),
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
        walk.enter(node, &mut matches.work)?;
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

/// The query's cursor and the budget of the text its predicates and the
/// names read.
struct Matches<'t> {
    cursor: QueryCursor,
    work: Work,
    text: &'t str,
}

impl Matches<'_> {
    /// Record each match rooted at `node`; [`IndexFault::TooComplex`] when
    /// the match limit is hit or the predicates' text runs past its
    /// budget.
    fn record_at(&mut self, node: Node<'_>, walk: &mut Walk<'_, '_>) -> Result<(), IndexFault> {
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

impl<'r> Walk<'r, '_> {
    /// Record a match of pattern `pattern` that captured `captures`.
    fn record(&mut self, pattern: usize, captures: &[tree_sitter::QueryCapture<'_>]) {
        let (mut definition, mut names) = (None, Vec::new());
        for capture in captures {
            let id = capture.node.id();
            let what = self.rules.captures.get(capture.index as usize);
            let mut wait = |waiting| self.waiting.entry(id).or_default().push(waiting);
            let mut mark = |set: fn(&mut NodeMarks)| set(self.found.marks.entry(id).or_default());
            match what.copied().unwrap_or(Capture::Predicate) {
                Capture::Definition => definition = Some(id),
                Capture::Name => names.push(id),
                Capture::Test => wait(Waiting::Test),
                Capture::Visibility(visibility) => wait(Waiting::Visibility(visibility)),
                Capture::Within(visibility) => wait(Waiting::Within(visibility)),
                Capture::After(visibility) => wait(Waiting::After(visibility)),
                Capture::Through => wait(Waiting::Through),
                Capture::Unnamed => self.found.unnamed.push(SpanNode::of(capture.node)),
                Capture::NoScope => {
                    self.found.no_scope.insert(id);
                }
                Capture::Export => {
                    if let Some(spelled) = self.text.get(capture.node.byte_range()) {
                        self.found.exported.insert(spelled);
                    }
                }
                Capture::Unit => mark(|marks| marks.whole = true),
                Capture::Tokens => mark(|marks| marks.tokens = true),
                Capture::Chars => mark(|marks| marks.chars = true),
                Capture::Delimiter => mark(|marks| marks.delimiter = true),
                Capture::Join => mark(|marks| marks.join = true),
                Capture::String => mark(|marks| marks.string = true),
                Capture::Escape => mark(|marks| marks.escape = true),
                Capture::Literal => mark(|marks| marks.literal = true),
                Capture::Skip => mark(|marks| marks.skip = true),
                Capture::Group => mark(|marks| marks.group = true),
                Capture::Predicate => {}
            }
        }
        let definer = self.rules.patterns.get(pattern).and_then(Option::as_ref);
        if let (Some(definition), Some(definer)) = (definition, definer) {
            self.candidate(definer, definition, &names);
        }
    }

    /// A candidate of `definer` that waits for its `@definition` node
    /// `definition` and its `@name` nodes `names`, one at least.
    fn candidate(&mut self, definer: &'r Definer, definition: usize, names: &[usize]) {
        if names.is_empty() {
            return;
        }
        let at = self.found.candidates.len();
        self.found.candidates.push(Candidate {
            definer,
            site: None,
            name: None,
            names_left: names.len(),
        });
        let mut wait = |id, waiting| self.waiting.entry(id).or_default().push(waiting);
        wait(definition, Waiting::Definition(at));
        for name in names {
            wait(*name, Waiting::Name(at));
        }
    }

    /// Enter `node`: its frame, from its parent's and what waited for it,
    /// and the candidates that captured it completed. A candidate of a
    /// pattern that is not exclusive is dropped when one that is captured
    /// the same node.
    fn enter(&mut self, node: Node<'_>, work: &mut Work) -> Result<(), IndexFault> {
        let waiting = self.waiting.remove(&node.id()).unwrap_or_default();
        let parent = self.frames.last().copied();
        let frame = Frame::of(node, parent, &waiting);
        for waited in &waiting {
            if let Waiting::After(label) = waited {
                frame::set_label(&mut self.frames, *label);
            }
        }
        self.push_reach(frame.kind, parent.map(|parent| parent.kind));
        self.push_runs(frame, syntax::is_comment(node));
        self.frames.push(frame);
        let claimed = waiting
            .iter()
            .filter_map(Waiting::definition)
            .any(|at| self.is_exclusive(at));
        for waited in waiting {
            match waited {
                Waiting::Definition(at) => {
                    if !claimed || self.is_exclusive(at) {
                        self.complete_span(at);
                    }
                }
                Waiting::Name(at) => self.complete_name(at, node, frame, work)?,
                Waiting::Test
                | Waiting::Visibility(_)
                | Waiting::Within(_)
                | Waiting::After(_)
                | Waiting::Through => {}
            }
        }
        Ok(())
    }

    /// Whether candidate `at`'s pattern is exclusive.
    fn is_exclusive(&self, at: usize) -> bool {
        self.found
            .candidates
            .get(at)
            .is_some_and(|candidate| candidate.definer.exclusive)
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

    /// Push the runs of `frame`'s node: of each preceding set, the run
    /// right before it among its parent's children seen so far, and none
    /// yet among its own. The parent's run goes on through the node when
    /// it is of the set's kinds or a comment, and ends at any other.
    fn push_runs(&mut self, frame: Frame, comment: bool) {
        let rules = self.rules;
        let width = rules.precedings.len();
        let parent_row = self.frames.len().checked_sub(1).map(|depth| depth * width);
        for (at, kinds) in rules.precedings.iter().enumerate() {
            let run = parent_row.and_then(|row| self.runs.get_mut(row + at));
            let before = run.as_deref().copied().flatten();
            if let Some(run) = run {
                *run = match (kinds.contains(&frame.kind), comment) {
                    (true, _) => Some(before.unwrap_or(frame.node)),
                    (false, true) => before,
                    (false, false) => None,
                };
            }
            self.before.push(before);
        }
        self.runs.extend(iter::repeat_n(None, width));
    }

    /// The reach of growth or wrap `at` (counted over the growths, then
    /// the wraps) from the frame at `depth`.
    fn reach_of(&self, depth: usize, at: usize) -> Option<usize> {
        let width = self.rules.growths.len() + self.rules.wraps.len();
        self.reach.get(depth * width + at).copied().flatten()
    }

    /// Complete candidate `at`'s site: the node the walk is at, and its
    /// span, that node grown, wrapped and preceded as its pattern says;
    /// none when its growth reaches nothing.
    fn complete_span(&mut self, at: usize) {
        let Some(definer) = self
            .found
            .candidates
            .get(at)
            .map(|candidate| candidate.definer)
        else {
            return;
        };
        let Some(frame) = self.frames.last().copied() else {
            return;
        };
        let depth = self.frames.len() - 1;
        let grown = match definer.growth {
            None => Some(depth),
            Some(growth) => self.rules.growths.get(growth).and_then(|reached| {
                match reached.to.contains(&frame.kind) {
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
        let span = wrapped.and_then(|depth| self.extent_at(depth, definer));
        if let Some(candidate) = self.found.candidates.get_mut(at) {
            candidate.site = span.map(|span| Site {
                node: frame.node,
                span,
            });
        }
    }

    /// The span of the node of the frame at `depth`, with the run of
    /// siblings right before it that `definer` takes in.
    fn extent_at(&self, depth: usize, definer: &Definer) -> Option<Extent> {
        let node = self.frames.get(depth)?.node;
        let width = self.rules.precedings.len();
        let first = definer
            .preceding
            .and_then(|set| self.before.get(depth * width + set).copied().flatten())
            .unwrap_or(node);
        Some(Extent {
            first: first.id,
            last: node.id,
            start: first.start,
            end: node.end,
        })
    }

    /// Complete candidate `at`'s name: `node`, whose frame is `frame`.
    fn complete_name(
        &mut self,
        at: usize,
        node: Node<'_>,
        frame: Frame,
        work: &mut Work,
    ) -> Result<(), IndexFault> {
        let Some(candidate) = self.found.candidates.get_mut(at) else {
            return Ok(());
        };
        let definer = candidate.definer;
        let parts = match &definer.text {
            // clone: the name's one segment is the candidate's own.
            Some(text) => vec![text.clone()],
            None => name::parts(
                node,
                self.text,
                (definer.separator.as_deref(), &definer.skip),
                work,
            )?,
        };
        candidate.names_left = candidate.names_left.saturating_sub(1);
        match candidate.name.as_mut() {
            Some(name) => name.parts.extend(parts),
            None => {
                candidate.name = Some(NameNode {
                    start: node.start_byte(),
                    parts,
                    test: frame.test,
                    visibility: frame.visibility,
                    within: frame.within,
                    label: frame.after,
                });
            }
        }
        Ok(())
    }

    /// Leave the node the walk is at.
    fn leave(&mut self) {
        self.frames.pop();
        let depth = self.frames.len();
        let width = self.rules.growths.len() + self.rules.wraps.len();
        self.reach.truncate(depth * width);
        let width = self.rules.precedings.len();
        self.before.truncate(depth * width);
        self.runs.truncate(depth * width);
    }
}

#[cfg(test)]
mod tests;
