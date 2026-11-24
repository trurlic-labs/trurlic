//! The bar's judgement of a stored decision for a report: every rule it
//! breaks, where a write stops at the first. The limits are judged apart
//! from the patterns, and each pattern apart from the others, so a report
//! chooses which to pay for; a pattern reads a decision as a write reads
//! an op and its batch's origin.

use std::borrow::Cow;

use super::{Bar, Breach, Broken, PatternFault, Place, at_least, at_most, texts};
use crate::store::quality::{DecisionField, Level, Limit, Quality, QualityFault, WordRange, words};
use crate::store::schema::Decision;

/// A rule of the bar a stored decision breaks, and where: what a report
/// shows of a graph below its own bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    pub field: DecisionField,
    /// The element of a list field, by its position in the decision's list.
    pub element: Option<usize>,
    /// `refuse` for a limit, the pattern's level for a pattern.
    pub level: Level,
    pub fault: QualityFault,
}

impl Bar {
    /// The bar of `quality` for a report, which a pattern that does not
    /// compile must not stop: its limits alone, and the pattern at fault.
    pub(crate) fn for_report(quality: &Quality) -> (Self, Option<PatternFault>) {
        match Self::compile(quality) {
            Ok(bar) => (bar, None),
            Err(fault) => (Self::limits_only(quality), Some(fault)),
        }
    }

    /// The patterns this bar judges: those at `0..pattern_count()` in
    /// `[quality] patterns`, or none.
    pub(crate) fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    /// Every limit `decision` breaks as stored, field by field in the order
    /// of the file's keys and an alternative's by its position, each at
    /// `refuse`. The text rules are validation's, reported as issues.
    pub(crate) fn audit_limits(&self, decision: &Decision) -> Vec<Shortfall> {
        DecisionField::IN_FILE_ORDER
            .into_iter()
            .flat_map(|field| self.limit_checks(decision, field))
            .filter_map(Result::err)
            .filter_map(limit_shortfall)
            .collect()
    }

    /// The pattern at `index` of `[quality] patterns` against `decision` as
    /// stored, at the pattern's level. First its match in the stored
    /// `origin`, when it reads `origin`, as [`Bar::judge_origin`] reads a
    /// batch's. Then its first match over the other fields it reads, as
    /// [`Bar::judge`] reads an op's. A write that recorded the decision
    /// would give both. `require_origin` binds a batch that records, which a
    /// stored decision is no longer.
    pub(crate) fn audit_pattern(&self, index: usize, decision: &Decision) -> Vec<Shortfall> {
        let Some(pattern) = self.patterns.get(index) else {
            return Vec::new();
        };
        let shortfall = |place: Place, found: &regex::Match<'_>| Shortfall {
            field: place.field,
            element: place.index,
            level: pattern.level,
            fault: QualityFault::Pattern(pattern.matched(index, found)),
        };
        let origin = Place::whole(DecisionField::Origin);
        let in_origin = decision
            .origin
            .as_deref()
            .filter(|_| pattern.fields.contains(&DecisionField::Origin))
            .and_then(|text| pattern.first_match([(origin, text)]))
            .map(|(place, found)| shortfall(place, &found));
        let texts: Vec<(Place, Cow<'_, str>)> = DecisionField::IN_FILE_ORDER
            .into_iter()
            .filter(|field| pattern.fields.contains(field))
            .flat_map(|field| texts(decision, field))
            .collect();
        let read = texts.iter().map(|(place, text)| (*place, text.as_ref()));
        let in_fields = pattern
            .first_match(read)
            .map(|(place, found)| shortfall(place, &found));
        in_origin.into_iter().chain(in_fields).collect()
    }

    /// What judging this bar's patterns over `decision` costs at worst, in
    /// state-bytes. Each pattern costs the bytes of each text of its fields
    /// and one for the search itself, an anchor as `file::symbol`, times
    /// the states of its NFA, which its slowest engine walks per byte.
    pub(crate) fn pattern_work(&self, decision: &Decision) -> usize {
        self.patterns
            .iter()
            .flat_map(|pattern| {
                let fields = pattern.fields.iter();
                fields.map(|&field| field_bytes(decision, field).saturating_mul(pattern.states))
            })
            .fold(0, usize::saturating_add)
    }

    /// The limits of `field` held to `decision`: a breach of each it breaks.
    fn limit_checks(&self, decision: &Decision, field: DecisionField) -> Vec<Result<(), Breach>> {
        let place = Place::whole(field);
        match field {
            DecisionField::Reason => {
                let measured = words(&decision.reason);
                let WordRange { min, max } = self.reason_words;
                vec![
                    at_least(place, Limit::ReasonWordsMin, measured, min),
                    at_most(place, Limit::ReasonWordsMax, measured, max),
                ]
            }
            DecisionField::Alternatives => {
                let count = decision.alternatives.len();
                let (limit, bound) = (Limit::AlternativeWordsMax, self.alternative_words_max);
                let each = decision.alternatives.iter().enumerate();
                let elements = each.map(|(index, text)| {
                    at_most(Place::element(field, index), limit, words(text), bound)
                });
                [
                    at_least(place, Limit::AlternativeRequired, count, Some(1)),
                    at_most(place, Limit::AlternativesMax, count, self.alternatives_max),
                ]
                .into_iter()
                .chain(elements)
                .collect()
            }
            DecisionField::Anchors => {
                let count = decision.anchors.len();
                vec![
                    at_least(place, Limit::AnchorRequired, count, Some(1)),
                    at_most(place, Limit::AnchorsMax, count, self.anchors_max),
                ]
            }
            // A choice's bound and every text rule are validation's; tags
            // and the origin have no limit.
            DecisionField::Choice | DecisionField::Tags | DecisionField::Origin => Vec::new(),
        }
    }
}

/// The bytes of the texts of `field` a pattern reads in `decision`, each
/// one more for the search over it.
fn field_bytes(decision: &Decision, field: DecisionField) -> usize {
    let searched = |text: usize| text.saturating_add(1);
    let sum = |lengths: &mut dyn Iterator<Item = usize>| {
        lengths.map(searched).fold(0, usize::saturating_add)
    };
    match field {
        DecisionField::Choice => searched(decision.choice.len()),
        DecisionField::Reason => searched(decision.reason.len()),
        DecisionField::Alternatives => sum(&mut decision.alternatives.iter().map(String::len)),
        DecisionField::Tags => sum(&mut decision.tags.iter().map(String::len)),
        DecisionField::Anchors => sum(&mut decision.anchors.iter().map(|anchor| {
            let symbol = anchor
                .symbol()
                .map_or(0, |symbol| symbol.len() + "::".len());
            anchor.file().len() + symbol
        })),
        DecisionField::Origin => decision
            .origin
            .as_ref()
            .map_or(0, |origin| searched(origin.len())),
    }
}

/// A limit's breach as a report's finding: always `refuse`.
fn limit_shortfall(breach: Breach) -> Option<Shortfall> {
    let Broken::Quality(fault) = breach.broken else {
        return None;
    };
    Some(Shortfall {
        field: breach.place.field,
        element: breach.place.index,
        level: Level::Refuse,
        fault,
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::*;
    use crate::store::corpus::Corpus;
    use crate::store::quality::{Hit, Pattern};
    use crate::store::testing::sample_decision;
    use crate::store::{Anchor, ProjectState};

    fn pattern(text: &str, fields: &[DecisionField], level: Level) -> Pattern {
        Pattern {
            pattern: text.into(),
            fields: fields.iter().copied().collect(),
            level,
            message: "M".into(),
        }
    }

    /// Every rule `bar` finds `decision` breaks, the limits, then each
    /// pattern in its order.
    fn audit(bar: &Bar, decision: &Decision) -> Vec<Shortfall> {
        let patterns =
            (0..bar.pattern_count()).flat_map(|index| bar.audit_pattern(index, decision));
        bar.audit_limits(decision)
            .into_iter()
            .chain(patterns)
            .collect()
    }

    /// A report sees every rule a stored decision breaks, where a write
    /// stops at the first: each limit, each element over its bound, each
    /// pattern at its level; a pattern that reads the origin judges it on
    /// its own, as a write judges a batch's, so it can break the bar there
    /// and in a field; never a text rule, never `require_origin`.
    #[test]
    fn an_audit_finds_every_rule_a_stored_decision_breaks() {
        let both = [DecisionField::Choice, DecisionField::Origin];
        let table = Quality {
            reason_words: WordRange {
                min: Some(5),
                max: None,
            },
            alternatives_max: Some(1),
            alternative_words_max: Some(1),
            require_origin: true,
            patterns: vec![
                pattern("zz", &both, Level::Warn),
                pattern("yy", &both, Level::Refuse),
            ],
            ..Quality::default()
        };
        let (bar, uncompiled) = Bar::for_report(&table);
        assert!(uncompiled.is_none());
        let mut decision = sample_decision("project");
        // A control character breaks a text rule: validation's, not the bar's.
        decision.choice = "Keep yy zz\u{7}".into();
        decision.reason = "Too short.".into();
        decision.alternatives = vec!["one two".into(), "three four".into()];
        decision.origin = Some("task zz".into());

        let found: Vec<_> = audit(&bar, &decision)
            .iter()
            .map(|s| {
                let at = match &s.fault {
                    QualityFault::Pattern(found) => Some(found.at),
                    QualityFault::Limit { .. } | QualityFault::OriginRequired => None,
                };
                (s.field, s.element, s.level, s.fault.as_str(), at)
            })
            .collect();

        let (refuse, warn) = (Level::Refuse, Level::Warn);
        let alternatives = DecisionField::Alternatives;
        assert_eq!(
            found,
            [
                (
                    DecisionField::Reason,
                    None,
                    refuse,
                    "reason_words_min",
                    None
                ),
                (alternatives, None, refuse, "alternatives_max", None),
                (alternatives, Some(0), refuse, "alternative_words_max", None),
                (alternatives, Some(1), refuse, "alternative_words_max", None),
                (
                    DecisionField::Anchors,
                    None,
                    refuse,
                    "anchor_required",
                    None
                ),
                (DecisionField::Origin, None, warn, "pattern", Some(5)),
                (DecisionField::Choice, None, warn, "pattern", Some(8)),
                (DecisionField::Choice, None, refuse, "pattern", Some(5)),
            ]
        );
        decision.origin = None;
        assert_eq!(audit(&bar, &decision).len(), 7);
        let broken = Quality {
            patterns: vec![pattern("(zz", &both, Level::Warn)],
            ..table
        };
        let (limits, uncompiled) = Bar::for_report(&broken);
        assert_eq!(uncompiled.map(|fault| fault.index), Some(0));
        assert_eq!(limits.pattern_count(), 0);
        assert_eq!(audit(&limits, &decision).len(), 5);
    }

    /// The seeded corpus of 600 decisions and a bar of the shipped table
    /// with patterns that match it: in the choice and the origin at once,
    /// in the reason at `refuse`, in tags and anchors.
    fn corpus() -> (TempDir, ProjectState, Bar) {
        #[derive(Deserialize)]
        struct Table {
            quality: Quality,
        }
        let shipped: Table =
            toml::from_str(include_str!("../../../../examples/quality.toml")).unwrap();
        let mut quality = shipped.quality;
        let choice_origin = [DecisionField::Choice, DecisionField::Origin];
        quality.patterns.extend([
            pattern(r"[0-9]7\b", &choice_origin, Level::Warn),
            pattern(r"task [0-9]*3\b", &choice_origin, Level::Refuse),
            pattern(r"\bcrash\b", &[DecisionField::Reason], Level::Refuse),
            pattern(r"^s", &[DecisionField::Tags], Level::Warn),
            pattern(r"::[a-z]+_[a-z]+_", &[DecisionField::Anchors], Level::Warn),
        ]);
        let tmp = TempDir::new().unwrap();
        let state = Corpus::new(600)
            .write(tmp.path())
            .unwrap()
            .load_state()
            .unwrap();
        let bar = Bar::compile(&quality).unwrap();
        (tmp, state, bar)
    }

    /// What a write gives of `found`: the first `refuse`, else every
    /// `warn` as a hit, each where it is.
    fn as_written(found: &[Shortfall]) -> Result<Vec<(Place, Hit)>, (Place, QualityFault)> {
        let place = |s: &Shortfall| Place {
            field: s.field,
            index: s.element,
        };
        if let Some(refused) = found.iter().find(|s| s.level == Level::Refuse) {
            return Err((place(refused), refused.fault.clone()));
        }
        let hits = found.iter().map(|s| match &s.fault {
            QualityFault::Pattern(matched) => (
                place(s),
                Hit {
                    pattern: matched.index,
                    at: matched.at,
                },
            ),
            QualityFault::Limit { .. } | QualityFault::OriginRequired => panic!("{s:?}"),
        });
        Ok(hits.collect())
    }

    fn breach(breach: Breach) -> (Place, QualityFault) {
        match breach.broken {
            Broken::Quality(fault) => (breach.place, fault),
            Broken::Text { .. } => panic!("the corpus is valid: {breach:?}"),
        }
    }

    /// An audit and a write report the same matches over every decision of
    /// the corpus. The audit's findings in the fields are what judging the
    /// decision as a record gives: its first refusal, else its warnings.
    /// Its findings in the origin are what judging the origin as a batch's
    /// gives.
    #[test]
    fn an_audit_reports_what_a_write_reports_over_the_seeded_corpus() {
        let (_tmp, state, bar) = corpus();
        let mut seen = [0usize; 5];
        for decision in state.decisions.values() {
            let (origin, fields): (Vec<Shortfall>, Vec<Shortfall>) = audit(&bar, decision)
                .into_iter()
                .partition(|s| s.field == DecisionField::Origin);

            let judged = bar.judge(decision, &DecisionField::IN_FILE_ORDER);
            let judged_origin = bar.judge_origin(decision.origin.as_deref(), false);

            assert_eq!(as_written(&fields), judged.map_err(breach), "{decision:?}");
            let origin_hits =
                as_written(&origin).map(|hits| hits.into_iter().map(|(_, hit)| hit).collect());
            assert_eq!(origin_hits, judged_origin.map_err(breach), "{decision:?}");
            let refused = |found: &[Shortfall]| found.iter().any(|s| s.level == Level::Refuse);
            let in_both = origin.iter().any(|o| {
                let index = |s: &Shortfall| match &s.fault {
                    QualityFault::Pattern(matched) => Some(matched.index),
                    QualityFault::Limit { .. } | QualityFault::OriginRequired => None,
                };
                fields
                    .iter()
                    .any(|f| index(f).is_some() && index(f) == index(o))
            });
            for (count, seen_here) in seen.iter_mut().zip([
                refused(&fields),
                !refused(&fields) && !fields.is_empty(),
                refused(&origin),
                !refused(&origin) && !origin.is_empty(),
                in_both,
            ]) {
                *count += usize::from(seen_here);
            }
        }
        // Each case is met: a refusal and warnings alone, in the fields and
        // in the origin, and one pattern in both.
        assert!(seen.iter().all(|&count| count > 0), "{seen:?}");
    }

    /// The work a pattern costs is the bytes of each text of its fields as
    /// the bar renders it, one more per text, times its NFA's states.
    #[test]
    fn the_work_of_a_pattern_is_the_texts_it_reads_times_its_states() {
        let every = DecisionField::IN_FILE_ORDER;
        let table = Quality {
            patterns: vec![
                pattern("x", &every, Level::Warn),
                pattern(r"\w{3}y", &[DecisionField::Anchors], Level::Warn),
            ],
            ..Quality::default()
        };
        let bar = Bar::compile(&table).unwrap();
        let (x, y) = (bar.patterns[0].states, bar.patterns[1].states);
        assert_eq!(x, super::super::nfa_states("x").unwrap());
        assert!(y > 10 * x, "{x} {y}");
        let mut decision = sample_decision("project");
        decision.alternatives = vec!["one".into(), "\u{17c}".into()];
        decision.tags = ["t".into()].into();
        decision.anchors = [
            Anchor::new("src/a.rs".into(), Some("run".into())).unwrap(),
            Anchor::new("src".into(), None).unwrap(),
        ]
        .into();
        decision.origin = Some("task".into());
        let searched = |field| -> usize {
            texts(&decision, field)
                .iter()
                .map(|(_, text)| text.len() + 1)
                .sum()
        };
        let anchors = searched(DecisionField::Anchors);
        let all: usize = every.into_iter().map(searched).sum::<usize>() + "task".len() + 1;

        assert_eq!(anchors, "src/a.rs::run".len() + 1 + "src".len() + 1);
        assert_eq!(bar.pattern_work(&decision), all * x + anchors * y);
    }
}
