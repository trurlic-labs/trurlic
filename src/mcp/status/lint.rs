//! The `lint` section of `status`: what to read that refuses nothing, of
//! three kinds, each gathered to the most a result can show. Likely
//! duplicate pairs come a page of probes at a time, best first within it;
//! quality findings, each rule of the bar a stored decision breaks, and the
//! decisions with only file anchors come by decision name. The section
//! lists them in that order of kinds, each kind fitted to the budget on
//! its own (`fill`). The `[quality]` patterns are judged only for the
//! section asked for alone (`patterns`), which reads a page of the
//! decisions from `from` on for the pairs and the patterns alike.

use std::cmp::Reverse;

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::lint::duplicates::{self, Among, PageEnd, Score, Similarity};
use crate::lint::quality::{self, Found};
use crate::mcp::spec::output::{Count, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown, to_value};
use crate::mcp::truncate::json_len;
use crate::store::limits::{MAX_DUPLICATE_WORK, MAX_NAME_BYTES};
use crate::store::{Decision, Level, ProjectState, QualityFault, Shortfall};

use super::{Listed, Part};

mod patterns;

pub(super) use patterns::Patterns;
pub(crate) use patterns::{Cut, PatternCounts, Work};
use patterns::{PAGE_MORE, Reading};

/// What every surface says of the duplicate pairs a listing leaves out.
pub(crate) const PAIRS_MORE: &str = "the section lists the pairs a page at a time, best first \
     within it, each on the page of the later-named of its decisions the pass probed: \
     `counts.lint.pairs.next` names where the next page starts";

/// What the `lint` section says of what it leaves out, true of each kind.
pub(super) const LINT_MORE: &str = "each kind lists what fits of the page and `counts.lint` \
     counts it: the duplicate pairs and the quality findings of the decisions before the \
     `next` that `counts.lint.pairs` or `counts.lint.patterns` names, which `section: \
     \"lint\"` with `from` lists on from, a pair on the page of the later-named of its \
     decisions the pass probed; the decisions with only file anchors left out follow the \
     last listed by name; the listed ones settled (pairs merged or revised apart, decisions \
     brought to the bar or anchored to a symbol), the rest come into view";

/// A finding of `lint`: something to read, which refuses nothing.
pub(crate) enum LintItem {
    /// Two decisions that likely state one rule twice, `decision` first by
    /// name, and their cosine to six decimals.
    Duplicate {
        decision: String,
        other: String,
        score: String,
    },
    /// A rule of the bar the stored decision breaks.
    Quality(QualityItem),
    /// A decision whose anchors all name a file and no symbol: none says
    /// what part of its file the decision constrains.
    FileAnchorsOnly { decision: String },
}

/// A rule of the bar a stored decision breaks, as a refusal names it.
pub(crate) struct QualityItem {
    pub(crate) decision: String,
    /// The field, an element of a list by its index: `alternatives[1]`.
    pub(crate) field: String,
    pub(crate) rule: &'static str,
    pub(crate) level: Level,
    /// A limit's measure and bound, in the rule's unit.
    pub(crate) limit: Option<Measure>,
    /// A pattern's first match, or where its cache gave up.
    pub(crate) pattern: Option<Matched>,
}

/// The pattern of a finding: its index in `[quality] patterns` and the
/// byte offset of its first match in the field's text, `None` when its
/// cache gave up there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Matched {
    pub(crate) index: usize,
    pub(crate) at: Option<usize>,
}

pub(crate) struct Measure {
    pub(crate) measured: usize,
    pub(crate) bound: usize,
    pub(crate) unit: &'static str,
}

/// The kind of a [`LintItem`], one per variant, in the section's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LintKind {
    Duplicate,
    Quality,
    FileAnchorsOnly,
}

impl LintKind {
    pub(super) const ALL: [Self; 3] = [Self::Duplicate, Self::Quality, Self::FileAnchorsOnly];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Quality => "quality",
            Self::FileAnchorsOnly => "file_anchors_only",
        }
    }
}

impl LintItem {
    const fn kind(&self) -> &'static LintKind {
        match self {
            Self::Duplicate { .. } => &LintKind::Duplicate,
            Self::Quality(_) => &LintKind::Quality,
            Self::FileAnchorsOnly { .. } => &LintKind::FileAnchorsOnly,
        }
    }
}

/// How many findings of each kind the page holds, and how far the pairs'
/// probes and the `[quality]` patterns read.
pub(crate) struct LintCounts {
    pub(crate) duplicates: usize,
    /// How far the pairs' probes read, when not to the last decision.
    pub(crate) pairs: Option<PatternCounts>,
    pub(crate) quality: usize,
    pub(crate) file_anchors_only: usize,
    /// When `[quality]` has patterns.
    pub(crate) patterns: Option<PatternCounts>,
}

/// The findings of each kind, in the section's order, and their counts.
pub(super) struct Gathered<'s> {
    pub(super) items: LintItems<'s>,
    pub(super) counts: LintCounts,
}

/// The findings of each kind before the budget cuts them; the page of
/// pairs and of quality findings, when each kind pages; and whether the
/// call reads a page of the decisions from `from` on.
pub(super) struct LintItems<'s> {
    pub(super) kinds: Vec<(LintKind, Listed<LintItem>)>,
    page: Option<Page<'s>>,
    pairs: Option<PairPage<'s>>,
    paged: bool,
}

/// A page of quality findings: how far the patterns read, and the
/// decision of each finding listed, in order.
struct Page<'s> {
    reading: Reading<'s>,
    decisions: Vec<&'s str>,
}

/// A page of likely duplicates: the pass's page, but its pairs, which
/// the listing holds, and the probe of each pair listed, in order.
struct PairPage<'s> {
    page: duplicates::Page<'s>,
    probes: Vec<&'s str>,
}

impl LintItems<'_> {
    /// The bytes the counts of a page may grow by once the budget cuts its
    /// listing: a `next` and why for each kind that pages.
    pub(super) fn reserve(&self) -> usize {
        let one = MAX_NAME_BYTES
            + r#","pairs":{"judged":0,"unjudged":0,"next":"","why":"work_limit","more":""}"#.len()
            + PAGE_MORE.len();
        usize::from(self.page.is_some()).saturating_add(1) * one
    }
}

/// Every finding of `state`, of the decisions recorded under `origin`
/// when one is given (a pair when either decision is), each kind to the
/// most one result can show. A `lint` call alone reads a page of the
/// decisions from `from` on by name: the pairs' probes within the work
/// bound, and the patterns within the bytes bound; another call reads the
/// pairs' first page, and the limits alone.
pub(super) fn gather<'s>(
    state: &'s ProjectState,
    origin: Option<&str>,
    patterns: Patterns<'_>,
) -> Gathered<'s> {
    let under = |decision: &Decision| is_under(origin, decision);
    let (pairs, duplicates) = pairs(state, origin, patterns.from());
    let limit = lint_limit(LintKind::Quality);
    let wanted = |name: &str, decision: &Decision| patterns.reads(name) && under(decision);
    let (mut shortfalls, reading) = patterns::shortfalls(state, patterns, limit, wanted);
    let page = reading.map(|reading| Page::of(reading, &mut shortfalls.first));
    let files = quality::file_anchors_only(state, lint_limit(LintKind::FileAnchorsOnly), under);
    let counts = LintCounts {
        duplicates: duplicates.total,
        pairs: duplicates.counts,
        quality: shortfalls.total,
        file_anchors_only: files.total,
        patterns: page.as_ref().map(Page::counts),
    };
    let items = LintItems {
        kinds: kinds(duplicates.listed, shortfalls, files),
        page,
        pairs,
        paged: patterns.pages(),
    };
    Gathered { items, counts }
}

/// The listing of each kind, in the section's order.
fn kinds(
    duplicates: Listed<LintItem>,
    shortfalls: Found<(&str, Shortfall)>,
    files: Found<&str>,
) -> Vec<(LintKind, Listed<LintItem>)> {
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
    vec![
        (LintKind::Duplicate, duplicates),
        (LintKind::Quality, Listed::of(shortfalls.total, quality)),
        (LintKind::FileAnchorsOnly, Listed::of(files.total, anchored)),
    ]
}

/// The pairs' listing, its count and its counts when it reads no page.
struct Duplicates {
    listed: Listed<LintItem>,
    total: usize,
    counts: Option<PatternCounts>,
}

/// The page of likely duplicate pairs of `state` from `from` on, those
/// touching a decision recorded under `origin` when one is given, within
/// [`MAX_DUPLICATE_WORK`]; `None` when the decisions' texts alone pass it.
fn pairs<'s>(
    state: &'s ProjectState,
    origin: Option<&str>,
    from: Option<&str>,
) -> (Option<PairPage<'s>>, Duplicates) {
    let named: Vec<&str> = state
        .decisions
        .iter()
        .filter(|(_, decision)| is_under(origin, decision))
        .map(|(name, _)| name.as_str())
        .collect();
    let among = match origin {
        Some(_) => Among::Touching(&named),
        None => Among::Every,
    };
    let similarity = match Similarity::of(state, MAX_DUPLICATE_WORK) {
        Ok(similarity) => similarity,
        Err(past) => {
            let wanted = named
                .iter()
                .filter(|&&name| from.is_none_or(|from| name >= from));
            return (None, unbuilt(wanted.count(), past.work));
        }
    };
    let mut page = similarity.page(among, from, lint_limit(LintKind::Duplicate));
    // Listed by probe until the budget has cut it, so a cut ends at one;
    // best first once cut.
    let mut pairs = std::mem::take(&mut page.pairs);
    pairs.sort_by_key(|pair| (pair.probe, Reverse(pair.score), pair.first, pair.second));
    let probes = pairs.iter().map(|pair| pair.probe).collect();
    let items = pairs.into_iter().map(|pair| LintItem::Duplicate {
        decision: pair.first.to_owned(),
        other: pair.second.to_owned(),
        score: pair.score.to_string(),
    });
    let pairs = PairPage { page, probes };
    let duplicates = Duplicates {
        listed: Listed::of(pairs.page.total, items),
        total: pairs.page.total,
        counts: pairs.counts(None),
    };
    (Some(pairs), duplicates)
}

/// No pair compared: the decisions' texts alone, `work`, pass the bound,
/// and `wanted` decisions went unread.
fn unbuilt(wanted: usize, work: usize) -> Duplicates {
    let counts = PatternCounts {
        judged: 0,
        unjudged: wanted,
        next: None,
        cut: Some(Cut::Unbuilt(Work {
            measured: work,
            bound: MAX_DUPLICATE_WORK,
            unit: "units",
        })),
    };
    Duplicates {
        listed: Listed::of(0, []),
        total: 0,
        counts: Some(counts),
    }
}

impl<'s> PairPage<'s> {
    /// Where the page of pairs ends once the budget kept the first `kept`
    /// of its listing, by probe: at the probe of the first pair it left
    /// out, or after the page's first probe when that probe's pairs alone
    /// pass the room, else where the pass ended.
    fn end(&self, kept: usize) -> Option<(&'s str, PageEnd)> {
        let Some(&cut) = self.probes.get(kept) else {
            return self.page.next;
        };
        if self.probes.first() != Some(&cut) {
            return Some((cut, PageEnd::Pairs));
        }
        let after = self.probes.iter().copied().find(|&probe| probe > cut);
        after
            .map(|probe| (probe, PageEnd::Pairs))
            .or(self.page.next)
    }

    /// Cut `part`, the pairs the budget kept, to the probes before `end`,
    /// then list them best first: the count of the page's pairs before it.
    fn cut(&self, part: &mut Part<LintItem>, end: Option<&str>) -> usize {
        let before = |probe: &&str| end.is_none_or(|end| *probe < end);
        let mut probes = self.probes.iter();
        part.items.retain(|_| probes.next().is_some_and(before));
        part.items.sort_by(|a, b| best_first(a).cmp(&best_first(b)));
        let after = self.probes.iter().filter(|probe| !before(probe)).count();
        let total = match self.probes.iter().any(before) {
            true => self.page.total.saturating_sub(after),
            false => 0,
        };
        part.total = total;
        part.omitted = total.saturating_sub(part.items.len());
        total
    }

    /// The counts of the page ended at `end`, its own end when `None`:
    /// none when it read to the last probe.
    fn counts(&self, end: Option<(&str, PageEnd)>) -> Option<PatternCounts> {
        let (next, why) = end.or(self.page.next)?;
        let judged = self.page.run.partition_point(|&probe| probe < next);
        let cut = match why {
            PageEnd::Work if self.page.next.is_some_and(|(own, _)| own == next) => {
                Cut::WorkLimit(Work {
                    measured: self.page.work,
                    bound: MAX_DUPLICATE_WORK,
                    unit: "units",
                })
            }
            PageEnd::Work | PageEnd::Pairs => Cut::Page,
        };
        Some(PatternCounts {
            judged,
            unjudged: self.page.probes.saturating_sub(judged),
            next: Some(next.to_owned()),
            cut: Some(cut),
        })
    }
}

/// A duplicate's place in a listing best first, then by names: its score,
/// six fixed decimals, compared as text.
fn best_first(item: &LintItem) -> (Reverse<&str>, &str, &str) {
    match item {
        LintItem::Duplicate {
            decision,
            other,
            score,
        } => (Reverse(score), decision, other),
        LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => (Reverse(""), "", ""),
    }
}

/// The part of `parts` of the kind `wanted`.
fn part(parts: &mut [(LintKind, Part<LintItem>)], wanted: LintKind) -> Option<&mut Part<LintItem>> {
    parts
        .iter_mut()
        .find(|(kind, _)| *kind == wanted)
        .map(|(_, part)| part)
}

/// The `lint` section of `items`, each kind fitted by `fit`. A page of
/// pairs ends at a probe, a page of quality findings at a decision; a
/// call that reads a page ends both at the earlier, and `counts` names
/// the decision where the next page starts.
pub(super) fn fitted(
    items: LintItems<'_>,
    fit: impl Fn(LintKind, Listed<LintItem>) -> Part<LintItem>,
    counts: Option<&mut LintCounts>,
) -> Part<LintItem> {
    let LintItems {
        kinds,
        page,
        pairs,
        paged,
    } = items;
    let mut parts: Vec<(LintKind, Part<LintItem>)> = kinds
        .into_iter()
        .map(|(kind, listed)| (kind, fit(kind, listed)))
        .collect();
    let pairs_end = pairs
        .as_ref()
        .zip(part(&mut parts, LintKind::Duplicate))
        .and_then(|(pairs, part)| pairs.end(part.items.len()));
    let quality_end = page
        .as_ref()
        .zip(part(&mut parts, LintKind::Quality))
        .and_then(|(page, part)| page.end(part));
    let end = earlier(pairs_end, quality_end.filter(|_| paged));
    let duplicates = pairs.as_ref().zip(part(&mut parts, LintKind::Duplicate));
    let duplicates = duplicates.map(|(pairs, part)| pairs.cut(part, end.map(|(name, _)| name)));
    if paged && let (Some((end, _)), Some(part)) = (end, part(&mut parts, LintKind::Quality)) {
        part.items
            .retain(|item| quality(item).is_none_or(|found| found.decision.as_str() < end));
        part.omitted = part.total.saturating_sub(part.items.len());
    }
    if let Some(counts) = counts {
        if let Some(total) = duplicates {
            counts.duplicates = total;
            counts.pairs = pairs.as_ref().and_then(|pairs| pairs.counts(end));
        }
        if let Some(page) = &page {
            let listed_to = match (quality_end, end.filter(|_| paged)) {
                (Some(own), Some((end, _))) => Some(own.min(end)),
                (own, end) => own.or(end.map(|(end, _)| end)),
            };
            counts.patterns = Some(page.reading.counts(listed_to));
        }
    }
    joined(parts)
}

/// Where a page ends: the earlier of the end of its pairs and of its
/// quality findings, when either ends.
fn earlier<'a>(
    pairs: Option<(&'a str, PageEnd)>,
    quality: Option<&'a str>,
) -> Option<(&'a str, PageEnd)> {
    match (pairs, quality) {
        (Some((pairs, _)), Some(quality)) if quality < pairs => Some((quality, PageEnd::Pairs)),
        (Some(end), _) => Some(end),
        (None, quality) => quality.map(|quality| (quality, PageEnd::Pairs)),
    }
}

/// The section of `parts`, in their order.
fn joined(parts: Vec<(LintKind, Part<LintItem>)>) -> Part<LintItem> {
    let mut section = Part {
        total: 0,
        items: Vec::new(),
        omitted: 0,
        more: None,
    };
    for (_, part) in parts {
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

/// Whether `decision` is recorded under `origin`: every one when `None`.
fn is_under(origin: Option<&str>, decision: &Decision) -> bool {
    origin.is_none_or(|origin| decision.origin.as_deref() == Some(origin))
}

fn quality_item(decision: &str, shortfall: Shortfall) -> LintItem {
    let Shortfall {
        field,
        element,
        level,
        fault,
    } = shortfall;
    let field = match element {
        Some(index) => format!("{}[{index}]", field.as_str()),
        None => field.as_str().to_owned(),
    };
    let (limit, pattern) = measure(&fault);
    LintItem::Quality(QualityItem {
        decision: decision.to_owned(),
        field,
        rule: fault.as_str(),
        level,
        limit,
        pattern,
    })
}

/// What a finding of `fault` shows: a limit's measure, or its pattern.
const fn measure(fault: &QualityFault) -> (Option<Measure>, Option<Matched>) {
    match fault {
        &QualityFault::Limit {
            limit,
            measured,
            bound,
        } => {
            let unit = limit.unit();
            (
                Some(Measure {
                    measured,
                    bound,
                    unit,
                }),
                None,
            )
        }
        QualityFault::Pattern(found) => {
            let (index, at) = (found.index, Some(found.at));
            (None, Some(Matched { index, at }))
        }
        &QualityFault::GaveUp { index, .. } | &QualityFault::CacheTotal { largest: index, .. } => {
            (None, Some(Matched { index, at: None }))
        }
        QualityFault::OriginRequired => (None, None),
    }
}

/// The most findings of `kind` one result can show: the budget over the
/// smallest finding of that kind, with the comma before it. A kind
/// gathered to it keeps every finding of it a result can show, and no more.
fn lint_limit(kind: LintKind) -> usize {
    let smallest = match kind {
        LintKind::Duplicate => LintItem::Duplicate {
            decision: "a".into(),
            other: "b".into(),
            score: Score::THRESHOLD.to_string(),
        },
        LintKind::Quality => LintItem::Quality(QualityItem {
            decision: "a".into(),
            field: "tags".into(),
            rule: "pattern",
            level: Level::Warn,
            limit: None,
            pattern: Some(Matched {
                index: 0,
                at: Some(0),
            }),
        }),
        LintKind::FileAnchorsOnly => LintItem::FileAnchorsOnly {
            decision: "a".into(),
        },
    };
    MAX_TOOL_RESULT_BYTES / json_len(&to_value(&smallest)).saturating_add(1)
}

const LEVEL: Tag<Level> = Tag {
    all: &[Level::Refuse, Level::Warn],
    name: Level::as_str,
};

impl Output for LintItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Tag {
            all: &LintKind::ALL,
            name: LintKind::as_str,
        };
        let about = "The finding's kind: `duplicate`, two decisions whose choice and reason \
                     have a TF-IDF cosine of 0.5 or more; `quality`, a rule of the bar the \
                     stored decision breaks; `file_anchors_only`, a decision whose anchors all \
                     name a file and no symbol, so none says what part of it the decision \
                     constrains.";
        fields.required("kind", about, &kind, |item| item.kind());
        let decision = "The decision it is about; a duplicate's first by name.";
        fields.required("decision", decision, &Str, |item| match item {
            LintItem::Duplicate { decision, .. } | LintItem::FileAnchorsOnly { decision } => {
                decision
            }
            LintItem::Quality(found) => &found.decision,
        });
        declare_duplicate(fields);
        declare_quality(fields);
    }
}

fn declare_duplicate(fields: &mut Shown<'_, LintItem>) {
    let other = "A duplicate's other decision.";
    fields.optional("other", other, &Str, |item| match item {
        LintItem::Duplicate { other, .. } => Some(other),
        LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => None,
    });
    let score = "A duplicate's cosine, to six decimals.";
    fields.optional("score", score, &Str, |item| match item {
        LintItem::Duplicate { score, .. } => Some(score),
        LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => None,
    });
}

/// The quality finding `item` is, if it is one.
const fn quality(item: &LintItem) -> Option<&QualityItem> {
    match item {
        LintItem::Quality(found) => Some(found),
        LintItem::Duplicate { .. } | LintItem::FileAnchorsOnly { .. } => None,
    }
}

fn declare_quality(fields: &mut Shown<'_, LintItem>) {
    let field = "A quality finding's field, an element by its index: `alternatives[1]`.";
    fields.optional("field", field, &Str, |item| {
        quality(item).map(|found| found.field.as_str())
    });
    let rule = "The rule as a refusal names it: a limit's, `pattern`, `pattern_gave_up` where \
                the pattern's cache filled before it could judge the field, or \
                `pattern_cache_total` where the patterns' caches together passed their total \
                over the decision, `pattern` naming the one that grew the most; a write of it \
                refuses both.";
    fields.optional("rule", rule, &Str, |item| {
        quality(item).map(|found| found.rule)
    });
    let level = "`refuse` for a limit, a give-up and a `refuse` pattern, `warn` for a `warn` one.";
    fields.optional("level", level, &LEVEL, |item| {
        quality(item).map(|found| &found.level)
    });
    let limit = Nested::<Measure>::new();
    let about = "A limit's measure and bound, in the rule's unit.";
    fields.optional("limit", about, &limit, |item| {
        quality(item).and_then(|found| found.limit.as_ref())
    });
    let pattern = "A pattern's index in `[quality] patterns`.";
    fields.optional("pattern", pattern, &Count, |item| {
        quality(item).and_then(|found| found.pattern.as_ref().map(|matched| &matched.index))
    });
    let at = "The byte offset of the pattern's first match in the field's text.";
    fields.optional("at", at, &Count, |item| {
        quality(item).and_then(|found| found.pattern.as_ref()?.at.as_ref())
    });
}

impl Output for Measure {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("measured", "What the field measures.", &Count, |m| {
            &m.measured
        });
        fields.required("bound", "The limit it passes.", &Count, |m| &m.bound);
        let unit = "What both count: `words`, `alternatives` or `anchors`.";
        fields.required("unit", unit, &Str, |m| m.unit);
    }
}

impl Output for LintCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let duplicates = "Likely duplicate pairs of the page.";
        fields.required("duplicates", duplicates, &Count, |c| &c.duplicates);
        let pairs = "How far the pairs' probes read, when not to the last decision: `next`, \
                     where `section: \"lint\"` with `from` reads on.";
        let kind = Nested::<PatternCounts>::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `lint` call from `from`, its listing cut to `room` items a kind:
    /// the pairs listed, as decisions, and the counts.
    fn page_from(
        state: &ProjectState,
        from: Option<&str>,
        room: usize,
    ) -> (Vec<String>, LintCounts) {
        let gathered = gather(state, None, Patterns::Judged { from });
        let mut counts = gathered.counts;
        let fit = |_, listed: Listed<LintItem>| {
            let mut items = listed.items;
            items.truncate(room);
            Part {
                total: listed.total,
                omitted: listed.total - items.len(),
                items,
                more: None,
            }
        };
        let part = fitted(gathered.items, fit, Some(&mut counts));
        let pairs = part.items.iter().filter_map(|item| match item {
            LintItem::Duplicate {
                decision, other, ..
            } => Some(format!("{decision} {other}")),
            LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => None,
        });
        (pairs.collect(), counts)
    }

    /// Twelve decisions of one text, 66 pairs, at most 11 on one probe's
    /// page. A page whose room, 12 items, the listing cuts ends before the
    /// probe of the first pair left out: the first lists the 10 of `d01` to
    /// `d04`, `d05`'s 5 more passing the room, and names `d05` in
    /// `counts.lint.pairs.next`. A walk from `next` to `next`
    /// lists each pair once, and counts each on its page.
    #[test]
    fn a_walk_of_lint_pages_lists_each_pair_once() {
        use crate::store::testing::{empty_state, sample_decision};

        let mut state = empty_state();
        for index in 0..12 {
            let mut decision = sample_decision("project");
            decision.choice = "Retry with backoff".into();
            state.decisions.insert(format!("d{index:02}"), decision);
        }
        let (mut walked, mut counted, mut from) = (Vec::new(), 0, None);
        loop {
            let (pairs, counts) = page_from(&state, from.as_deref(), 12);
            counted += counts.duplicates;
            walked.extend(pairs);
            let Some(next) = counts.pairs.and_then(|pairs| pairs.next) else {
                break;
            };
            from = Some(next);
        }
        let first = page_from(&state, None, 12).1.pairs.unwrap();

        walked.sort();
        walked.dedup();
        assert_eq!((walked.len(), counted), (66, 66));
        assert_eq!((first.judged, first.next.as_deref()), (5, Some("d05")));
        assert!(matches!(first.cut, Some(Cut::Page)));
    }

    /// Each kind is gathered to as many of its smallest findings as one
    /// result holds with their commas, and no more: a kind with larger
    /// findings keeps fewer.
    #[test]
    fn each_kind_keeps_what_a_result_can_show_of_it_and_no_more() {
        let mut limits = Vec::new();
        for kind in LintKind::ALL {
            let limit = lint_limit(kind);
            let held = |count: usize| {
                let items: Vec<LintItem> = (0..count)
                    .map(|_| match kind {
                        LintKind::Duplicate => LintItem::Duplicate {
                            decision: "a".into(),
                            other: "b".into(),
                            score: Score::THRESHOLD.to_string(),
                        },
                        LintKind::Quality => quality_item(
                            "a",
                            Shortfall {
                                field: crate::store::DecisionField::Tags,
                                element: None,
                                level: Level::Warn,
                                fault: QualityFault::Pattern(crate::store::PatternMatch {
                                    index: 0,
                                    at: 0,
                                    found: String::new(),
                                    message: String::new(),
                                }),
                            },
                        ),
                        LintKind::FileAnchorsOnly => LintItem::FileAnchorsOnly {
                            decision: "a".into(),
                        },
                    })
                    .collect();
                let bytes: usize = items.iter().map(|item| json_len(&to_value(item)) + 1).sum();
                bytes <= MAX_TOOL_RESULT_BYTES
            };
            assert!(held(limit), "{kind:?}");
            assert!(!held(limit + 1), "{kind:?}");
            limits.push(limit);
        }
        assert!(limits[0] < limits[2] && limits[1] < limits[2], "{limits:?}");
    }
}
