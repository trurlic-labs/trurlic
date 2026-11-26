//! The `lint` section gathered and fitted: the findings of each kind, to
//! the most a result can show, with their counts, then each kind fitted
//! to the budget on its own (`fitted`), a page of quality findings ending
//! at a decision's boundary.

use crate::lint::quality;
use crate::store::limits::MAX_NAME_BYTES;
use crate::store::{Decision, ProjectState, Shortfall};
use crate::tools::spec::output::{Count, Nested};
use crate::tools::spec::{Output, Shown};
use crate::tools::status::part::{Listed, Part};

use super::finding::{LintItem, LintKind, lint_limit, quality_item};
use super::pairs::{PairCounts, compared, is_under};
use super::patterns::{self, PAGE_MORE, PatternCounts, Patterns, Reading};

/// How many findings of each kind there are, and how far the pairs' pass
/// and the `[quality]` patterns read.
pub(crate) struct LintCounts {
    /// The pairs the pass found; `None` when the call compares none.
    pub(crate) duplicates: Option<usize>,
    /// How far the pass compared the decisions, when not every one.
    pub(crate) pairs: Option<PairCounts>,
    pub(crate) quality: usize,
    pub(crate) file_anchors_only: usize,
    /// When `[quality]` has patterns.
    pub(crate) patterns: Option<PatternCounts>,
}

/// The findings of each kind, in the section's order, and their counts.
pub(in crate::tools::status) struct Gathered<'s> {
    pub(in crate::tools::status) items: LintItems<'s>,
    pub(in crate::tools::status) counts: LintCounts,
}

/// The findings of each kind before the budget cuts them, and the page of
/// quality findings the patterns read, when they read one.
pub(in crate::tools::status) struct LintItems<'s> {
    pub(in crate::tools::status) kinds: Vec<(LintKind, Listed<LintItem>)>,
    pub(super) page: Option<Page<'s>>,
}

/// A page of quality findings: how far the patterns read, and the
/// decision of each finding listed, in order.
pub(super) struct Page<'s> {
    pub(super) reading: Reading<'s>,
    pub(super) decisions: Vec<&'s str>,
}

impl LintItems<'_> {
    /// The bytes the counts of a page may grow by once the budget cuts its
    /// listing: a `next` and why, beside the counts the frame measured.
    pub(in crate::tools::status) fn reserve(&self) -> usize {
        self.page.as_ref().map_or(0, |_| {
            MAX_NAME_BYTES + r#","next":"","why":"work_limit","more":"""#.len() + PAGE_MORE.len()
        })
    }
}

/// Every finding of `state`, of the decisions recorded under `origin`
/// when one is given (a pair when either decision is), each kind to the
/// most one result can show. The pairs are compared, within their bound
/// (`pairs`), only when `patterns` is `Judged` from the first decision;
/// the quality findings of the patterns only when it is `Judged`, within
/// a bound on the bytes they read, and then only of the decisions it
/// reads from.
pub(in crate::tools::status) fn gather<'s>(
    state: &'s ProjectState,
    origin: Option<&str>,
    patterns: Patterns<'_>,
) -> Gathered<'s> {
    let under = |decision: &Decision| is_under(origin, decision);
    let pairs = compared(state, origin, patterns);
    let limit = lint_limit(LintKind::Quality);
    let wanted = |name: &str, decision: &Decision| patterns.reads(name) && under(decision);
    let (mut shortfalls, reading) = patterns::shortfalls(state, patterns, limit, wanted);
    let page = reading.map(|reading| Page::of(reading, &mut shortfalls.first));
    let files = quality::file_anchors_only(
        &state.decisions,
        lint_limit(LintKind::FileAnchorsOnly),
        under,
    );
    let counts = LintCounts {
        duplicates: pairs.total,
        pairs: pairs.counts,
        quality: shortfalls.total,
        file_anchors_only: files.total,
        patterns: page.as_ref().map(Page::counts),
    };
    let quality = shortfalls
        .first
        .into_iter()
        .map(|(name, shortfall)| quality_item(name, shortfall));
    let anchored = files
        .first
        .into_iter()
        .map(|name| LintItem::FileAnchorsOnly {
            decision: name.to_owned(),
        });
    let kinds = vec![
        (
            LintKind::Duplicate,
            Listed::of(pairs.total.unwrap_or(0), pairs.items),
        ),
        (LintKind::Quality, Listed::of(shortfalls.total, quality)),
        (LintKind::FileAnchorsOnly, Listed::of(files.total, anchored)),
    ];
    Gathered {
        items: LintItems { kinds, page },
        counts,
    }
}

/// The `lint` section of `items`, each kind fitted by `fit`. A page of
/// quality findings ends at a decision's boundary, and `counts` names the
/// decision where the next page starts.
pub(in crate::tools::status) fn fitted(
    items: LintItems<'_>,
    fit: impl Fn(LintKind, Listed<LintItem>) -> Part<LintItem>,
    mut counts: Option<&mut LintCounts>,
) -> Part<LintItem> {
    let LintItems { kinds, page } = items;
    let mut section = Part {
        total: 0,
        items: Vec::new(),
        omitted: 0,
        more: None,
    };
    for (kind, listed) in kinds {
        let mut part = fit(kind, listed);
        if let (LintKind::Quality, Some(page)) = (kind, &page) {
            let listed_to = page.end(&mut part);
            if let Some(counts) = counts.as_deref_mut() {
                counts.patterns = Some(page.reading.counts(listed_to));
            }
        }
        section.total += part.total;
        section.omitted += part.omitted;
        section.items.extend(part.items);
        section.more = section.more.or(part.more);
    }
    section
}

impl<'s> Page<'s> {
    /// The page `reading` read, of the findings it kept, `first`, which it
    /// cuts to the decisions it lists whole. The pass's last decision may
    /// hold more findings than it kept; a first decision that does keeps
    /// those, and the page moves on past it.
    fn of(reading: Reading<'s>, first: &mut Vec<(&'s str, Shortfall)>) -> Self {
        let partial = reading.page_end();
        if first.iter().any(|&(name, _)| Some(name) != partial) {
            first.retain(|&(name, _)| Some(name) != partial);
        }
        let decisions = first.iter().map(|&(name, _)| name).collect();
        Self { reading, decisions }
    }

    /// The counts of the page before the budget cuts its listing.
    fn counts(&self) -> PatternCounts {
        self.reading.counts(self.reading.page_end())
    }

    /// End `part`, the quality findings the budget kept, at a decision's
    /// boundary: the first decision whose findings it does not hold whole.
    /// A page whose first decision's findings alone pass the room lists
    /// those that fit and ends after it, so every page moves on.
    fn end<'p>(&'p self, part: &mut Part<LintItem>) -> Option<&'p str> {
        let kept = part.items.len();
        let listed_to = self
            .decisions
            .get(kept)
            .copied()
            .or_else(|| self.reading.page_end())?;
        let whole = self
            .decisions
            .get(..kept)
            .map_or(kept, |kept| kept.partition_point(|&name| name < listed_to));
        if whole == 0 {
            return self.reading.after(listed_to);
        }
        part.items.truncate(whole);
        part.omitted = part.total.saturating_sub(part.items.len());
        Some(listed_to)
    }
}

impl Output for LintCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let duplicates = "Likely duplicate pairs the pass found, every one among the decisions \
                          it compared; absent when the call compares none.";
        fields.optional("duplicates", duplicates, &Count, |c| c.duplicates.as_ref());
        let pairs = "When the pairs' pass left decisions uncompared: how many it compared, why \
                     no more, and how to compare them.";
        let kind = Nested::<PairCounts>::new();
        fields.optional("pairs", pairs, &kind, |c| c.pairs.as_ref());
        let quality = "Rules of the bar the stored decisions break: every limit's, and the \
                       patterns' in the decisions `patterns` says they read.";
        fields.required("quality", quality, &Count, |c| &c.quality);
        let files = "Decisions whose anchors all name a file and no symbol.";
        fields.required("file_anchors_only", files, &Count, |c| &c.file_anchors_only);
        let patterns = "How far the `[quality]` patterns were judged, when it has any.";
        let kind = Nested::<PatternCounts>::new();
        fields.optional("patterns", patterns, &kind, |c| c.patterns.as_ref());
    }
}
