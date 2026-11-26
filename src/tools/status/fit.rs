//! Fitting a `status` report to the result budget: each section's items
//! measured as JSON, one level of [`budget::level`] found for all of them,
//! and each section keeping the leading items that level allows, with the
//! count of the rest and where it is. The `lint` section is fitted kind by
//! kind, so a long kind does not crowd out a short one.

use crate::budget::{self, MAX_TOOL_RESULT_BYTES, Shrinkable};
use crate::tools::spec::{Output, to_value};
use crate::tools::truncate::json_len;

use super::anchors::AnchorItem;
use super::lint::{self, LintItems, LintKind};
use super::report::{ComponentItem, HistoryItem, IssueItem};
use super::{LintItem, Part, Report, Section};

/// The bytes a report keeps, beyond its frame, for the `truncated`
/// notice of a cut, which no report needs unless its frame alone, its
/// paths, fills the budget.
const NOTICE_BYTES: usize = 64;

/// A section's items before the budget cuts them, out of `total`: every
/// item but a ranked section's, which holds only the best a result can.
pub(super) struct Listed<T> {
    pub total: usize,
    pub items: Vec<T>,
}

impl<T> Listed<T> {
    pub(super) fn all(items: impl IntoIterator<Item = T>) -> Self {
        let items: Vec<T> = items.into_iter().collect();
        Self {
            total: items.len(),
            items,
        }
    }

    /// The first `items` of `total`.
    pub(super) fn of(total: usize, items: impl IntoIterator<Item = T>) -> Self {
        Self {
            total,
            items: items.into_iter().collect(),
        }
    }

    /// The section with none of its items shown: what a report measures
    /// before it is filled.
    pub(super) const fn frame(&self) -> Part<T> {
        Part {
            total: self.total,
            items: Vec::new(),
            omitted: 0,
            more: None,
        }
    }
}

/// The `lint` section of `kinds` with none of its items shown.
pub(super) fn lint_frame(kinds: &[(LintKind, Listed<LintItem>)]) -> Part<LintItem> {
    Part {
        total: kinds.iter().map(|(_, listed)| listed.total).sum(),
        items: Vec::new(),
        omitted: 0,
        more: None,
    }
}

/// The items of each section asked for, before the budget cuts them; the
/// `lint` section's by kind, each kind fitted on its own, so a long kind
/// does not crowd out a short one.
pub(super) struct Items<'s> {
    pub issues: Option<Listed<IssueItem>>,
    pub history: Option<Listed<HistoryItem>>,
    pub components: Option<Listed<ComponentItem>>,
    pub lint: Option<LintItems<'s>>,
    pub anchors: Option<Listed<AnchorItem>>,
}

/// Put into `report`, whose sections hold no item yet, each section's
/// leading items that fit the budget beside the rest of it.
pub(super) fn fill(report: &mut Report, items: Items<'_>) {
    let sizes = sizes(&items);
    let Items {
        issues,
        history,
        components,
        lint,
        anchors,
    } = items;
    let reserve = lint.as_ref().map_or(0, LintItems::reserve);
    let fixed = json_len(&to_value(&*report)) + NOTICE_BYTES + reserve;
    let level = budget::level(&sizes, fixed, MAX_TOOL_RESULT_BYTES).unwrap_or(0);
    report.issues = issues.map(|items| Sizes::part(&sizes, Slot::Issues, items, level));
    report.history = history.map(|items| Sizes::part(&sizes, Slot::History, items, level));
    report.components = components.map(|items| Sizes::part(&sizes, Slot::Components, items, level));
    report.anchors = anchors.map(|items| Sizes::part(&sizes, Slot::Anchors, items, level));
    report.lint = lint.map(|lint| {
        let fit = |kind, listed| Sizes::part(&sizes, Slot::Lint(kind), listed, level);
        lint::fitted(lint, fit, report.counts.lint.as_mut())
    });
}

/// The sizes of each section `items` holds, a kind of the `lint` section
/// each apart.
fn sizes(items: &Items<'_>) -> Vec<Sizes> {
    let mut sizes: Vec<Sizes> = [
        items.issues.as_ref().map(|l| Sizes::of(Slot::Issues, l)),
        items.history.as_ref().map(|l| Sizes::of(Slot::History, l)),
        items
            .components
            .as_ref()
            .map(|l| Sizes::of(Slot::Components, l)),
        items.anchors.as_ref().map(|l| Sizes::of(Slot::Anchors, l)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let kinds = items.lint.iter().flat_map(|lint| &lint.kinds);
    sizes.extend(kinds.map(|(kind, listed)| Sizes::of(Slot::Lint(*kind), listed)));
    sizes
}

/// What [`Sizes`] measures: a section, or one kind of the `lint` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Issues,
    History,
    Components,
    Lint(LintKind),
    Anchors,
}

impl Slot {
    const fn section(self) -> Section {
        match self {
            Self::Issues => Section::Issues,
            Self::History => Section::History,
            Self::Components => Section::Components,
            Self::Lint(_) => Section::Lint,
            Self::Anchors => Section::Anchors,
        }
    }
}

/// The sizes of a section's items as JSON: what [`budget::level`] cuts.
struct Sizes {
    slot: Slot,
    /// The bytes of the first `k + 1` items with the commas between them.
    ends: Vec<usize>,
    /// Whether the section has items beyond those given, so that even
    /// whole it carries the note of what it leaves out.
    beyond: bool,
}

impl Sizes {
    fn of<T: Output>(slot: Slot, listed: &Listed<T>) -> Self {
        let mut ends = Vec::with_capacity(listed.items.len());
        let mut end = 0usize;
        for (index, item) in listed.items.iter().enumerate() {
            end = end
                .saturating_add(usize::from(index > 0))
                .saturating_add(json_len(&to_value(item)));
            ends.push(end);
        }
        Self {
            slot,
            ends,
            beyond: listed.total > listed.items.len(),
        }
    }

    /// The items of `slot` kept at `level`, with the count of the rest.
    fn part<T>(all: &[Self], slot: Slot, listed: Listed<T>, level: usize) -> Part<T> {
        let Listed { total, mut items } = listed;
        let limit = all
            .iter()
            .find(|sizes| sizes.slot == slot)
            .and_then(|sizes| budget::limit(sizes, level).map(|limit| sizes.count_within(limit)));
        if let Some(keep) = limit {
            items.truncate(keep);
        }
        let omitted = total.saturating_sub(items.len());
        Part {
            total,
            items,
            omitted,
            more: (omitted > 0).then(|| slot.section().more()),
        }
    }

    fn count_within(&self, limit: usize) -> usize {
        self.ends.partition_point(|&end| end <= limit)
    }
}

impl Shrinkable for Sizes {
    fn full(&self) -> usize {
        let items = self.ends.last().copied().unwrap_or(0);
        if self.beyond {
            items.saturating_add(self.note())
        } else {
            items
        }
    }

    fn note(&self) -> usize {
        // `,"omitted":N,"more":"..."`, N at most 20 digits.
        41 + self.slot.section().more().len()
    }

    fn kept(&self, limit: usize) -> usize {
        self.count_within(limit)
            .checked_sub(1)
            .and_then(|last| self.ends.get(last))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> ComponentItem {
        ComponentItem {
            name: name.into(),
            decisions: 0,
            proposed: 0,
        }
    }

    /// A section keeps the prefix of its items the level allows, and
    /// counts the rest with where to find them.
    #[test]
    fn a_cut_section_keeps_a_prefix_and_counts_the_rest() {
        let items = Listed::all((0..100).map(|i| item(&format!("c{i:02}"))));
        let sizes = [Sizes::of(Slot::Components, &items)];
        let whole = sizes[0].full();
        let level = budget::level(&sizes, 0, whole / 2).unwrap();

        let part = Sizes::part(&sizes, Slot::Components, items, level);

        assert_eq!(part.total, 100);
        assert!(part.items.len() < 50, "{}", part.items.len());
        assert_eq!(part.omitted, 100 - part.items.len());
        assert_eq!(part.items[0].name, "c00");
        assert_eq!(part.more, Some(Section::Components.more()));
        let one = Listed::all([item("c00")]);
        let fits = Sizes::part(&sizes, Slot::Components, one, whole);
        assert_eq!((fits.omitted, fits.more), (0, None));
    }

    /// A ranked section given its best items out of more counts the rest
    /// even when every given item fits, and its size holds that note.
    #[test]
    fn a_ranked_section_counts_what_its_selection_left_out() {
        let best = Listed {
            total: 1000,
            items: vec![item("c00"), item("c01")],
        };
        let sizes = Sizes::of(Slot::Lint(LintKind::Duplicate), &best);
        assert_eq!(sizes.full(), sizes.ends[1] + sizes.note());

        let part = Sizes::part(
            &[sizes],
            Slot::Lint(LintKind::Duplicate),
            best,
            MAX_TOOL_RESULT_BYTES,
        );

        assert_eq!((part.total, part.items.len(), part.omitted), (1000, 2, 998));
        assert_eq!(part.more, Some(Section::Lint.more()));
    }
}
