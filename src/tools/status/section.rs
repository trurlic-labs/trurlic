//! The parts of the `status` report a call can ask for alone, by the name
//! `section` takes, and where each one's omitted items are.

/// A part of the report that can be asked for alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Issues,
    History,
    Components,
    Lint,
    Anchors,
}

/// What the `lint` section says of what it leaves out, true of each kind.
const LINT_MORE: &str = "each kind lists what fits and `counts.lint` counts it: the \
     duplicate pairs left out rank at or below the last listed; the quality findings and the \
     decisions with only file anchors left out follow the last listed by decision name, and \
     `section: \"lint\"` with `from` lists the quality findings from a decision on; they come \
     into view as the listed ones are settled (pairs merged or revised apart, decisions brought \
     to the bar or anchored to a symbol); `counts.lint.pairs` and `counts.lint.patterns` say \
     which decisions the pairs' pass and the `[quality]` patterns did not read and how to read \
     them";

impl Section {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::History => "history",
            Self::Components => "components",
            Self::Lint => "lint",
            Self::Anchors => "anchors",
        }
    }

    /// Where a section's omitted items are, every one of them; for the
    /// ranked findings of `lint`, where they rank and how they come into
    /// view, since no surface lists them all.
    pub(super) const fn more(self) -> &'static str {
        match self {
            Self::Issues => "`trurlic check` lists every issue",
            Self::History => "`git status` in the graph directory lists every file",
            Self::Components => "the files under .trurlic/components/ and .trurlic/decisions/",
            Self::Lint => LINT_MORE,
            Self::Anchors => "`trurlic check` lists every finding",
        }
    }
}

pub(super) const SECTIONS: [(&str, Section); 5] = [
    (Section::Issues.as_str(), Section::Issues),
    (Section::History.as_str(), Section::History),
    (Section::Components.as_str(), Section::Components),
    (Section::Lint.as_str(), Section::Lint),
    (Section::Anchors.as_str(), Section::Anchors),
];

/// The names `section` takes, in the order the schema lists them.
pub(crate) fn section_names() -> impl Iterator<Item = &'static str> {
    SECTIONS.iter().map(|&(name, _)| name)
}
