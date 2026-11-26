//! The bar's judgement of a stored decision for a report: every rule it
//! breaks, where a write stops at the first. The limits are judged apart
//! from the patterns, so a report chooses which to pay for; the patterns
//! read a decision as a write reads an op and its batch's origin, over one
//! [`Scan`] for the page.

use std::borrow::Cow;

use super::{Bar, Breach, Broken, PatternFault, Place, Scan, searched, texts};
use crate::store::model::Decision;
use crate::store::model::{DecisionField, Level, Quality};
use crate::store::quality::QualityFault;

/// A rule of the bar a stored decision breaks, and where: what a report
/// shows of a graph below its own bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shortfall {
    pub(crate) field: DecisionField,
    /// The element of a list field, by its position in the decision's list.
    pub(crate) element: Option<usize>,
    /// `refuse` for a limit, a give-up and the total, the pattern's level
    /// for a match.
    pub(crate) level: Level,
    pub(crate) fault: QualityFault,
}

impl Shortfall {
    const fn at(place: Place, level: Level, fault: QualityFault) -> Self {
        Self {
            field: place.field,
            element: place.index,
            level,
            fault,
        }
    }
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

    /// Every limit `decision` breaks as stored, field by field in the order
    /// of the file's keys and an alternative's by its position, each at
    /// `refuse`. The text rules are validation's, reported as issues.
    pub(crate) fn audit_limits(&self, decision: &Decision) -> Vec<Shortfall> {
        DecisionField::IN_FILE_ORDER
            .into_iter()
            .flat_map(|field| self.checks(decision, field))
            .filter_map(Result::err)
            .filter_map(limit_shortfall)
            .collect()
    }

    /// The bytes the patterns read to judge `decision` as stored, its
    /// origin included, as a write charges them.
    pub(crate) fn pattern_bytes(&self, decision: &Decision) -> usize {
        let texts = audited(decision);
        let read = |pattern: &super::Compiled| {
            searched(pattern.read(&texts).into_iter().map(|(_, text)| text))
        };
        self.patterns
            .iter()
            .map(read)
            .fold(0, usize::saturating_add)
    }

    /// The patterns, in their order, against `decision` as stored, each at
    /// its level, over the page's `scan`. First a pattern's match in the
    /// stored `origin`, when it reads `origin`, as [`Bar::judge_origin`]
    /// reads a batch's; then its first match over the other fields it
    /// reads, as [`Bar::judge_patterns`] reads an op's. A write that
    /// recorded the decision would give both. A cache that gives up, or
    /// caches that grow past their total, stop the patterns as they stop a
    /// write: the matches before it, and that stop as a finding at
    /// `refuse`. `require_origin` binds a batch that records, which a
    /// stored decision is no longer.
    pub(crate) fn audit_patterns(
        &self,
        decision: &Decision,
        scan: &mut Scan,
    ) -> (Vec<Shortfall>, Option<Shortfall>) {
        let texts = audited(decision);
        let mut found = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let (origin, fields): (Vec<_>, Vec<_>) = pattern
                .read(&texts)
                .into_iter()
                .partition(|(place, _)| place.field == DecisionField::Origin);
            for part in [origin, fields] {
                match scan.search(index, pattern, &part) {
                    Ok(Some((place, matched))) => {
                        let fault = QualityFault::Pattern(pattern.matched(index, &matched));
                        found.push(Shortfall::at(place, pattern.level, fault));
                    }
                    Ok(None) => {}
                    Err((place, fault)) => {
                        return (found, Some(Shortfall::at(place, Level::Refuse, fault)));
                    }
                }
            }
        }
        (found, None)
    }
}

/// The texts of `decision` the patterns read as stored: its stored origin,
/// then its fields in the order of the file's keys.
fn audited(decision: &Decision) -> Vec<(Place, Cow<'_, str>)> {
    let origin = decision
        .origin
        .as_deref()
        .map(|origin| (Place::whole(DecisionField::Origin), Cow::Borrowed(origin)));
    let fields = DecisionField::IN_FILE_ORDER
        .into_iter()
        .flat_map(|field| texts(decision, field));
    origin.into_iter().chain(fields).collect()
}

/// A limit's breach as a report's finding, always `refuse`: none for a
/// text rule's, which validation reports. No limit is a bound on the
/// bytes the patterns read.
fn limit_shortfall(breach: Breach) -> Option<Shortfall> {
    match breach.broken {
        Broken::Quality(fault) => Some(Shortfall::at(breach.place, Level::Refuse, fault)),
        Broken::Text { .. } | Broken::Work => None,
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::*;
    use crate::store::corpus::Corpus;
    use crate::store::limits::{MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_CACHE_TOTAL_BYTES};
    use crate::store::model::Pattern;
    use crate::store::quality::Hit;
    use crate::store::quality::Scan;
    use crate::store::testing::{OUTGROWS_ITS_CACHE, coin_flips, sample_decision};
    use crate::store::{ProjectState, WordRange};

    fn pattern(text: &str, fields: &[DecisionField], level: Level) -> Pattern {
        Pattern {
            pattern: text.into(),
            fields: fields.iter().copied().collect(),
            level,
            message: "M".into(),
        }
    }

    /// Every rule `bar` finds `decision` breaks, the limits, then the
    /// patterns in their order over fresh caches with no total, and what
    /// stopped them.
    fn audit(bar: &Bar, decision: &Decision) -> Vec<Shortfall> {
        let mut scan = Scan::of(&bar.patterns, 0, usize::MAX);
        let (patterns, stopped) = bar.audit_patterns(decision, &mut scan);
        let mut found = bar.audit_limits(decision);
        found.extend(patterns.into_iter().chain(stopped));
        found
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
                    QualityFault::Limit { .. }
                    | QualityFault::GaveUp { .. }
                    | QualityFault::CacheTotal { .. }
                    | QualityFault::OriginRequired => None,
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
        assert!(limits.patterns.is_empty());
        assert_eq!(audit(&limits, &decision).len(), 5);
    }

    /// A pattern whose cache gives up in a decision's origin stops the
    /// patterns there, as it stops a write: the give-up is the finding, at
    /// `refuse` whatever its level, and neither a field after it nor a
    /// later pattern that would match is judged.
    #[test]
    fn a_give_up_is_a_refuse_finding_and_the_patterns_read_no_further() {
        let fields = [DecisionField::Origin, DecisionField::Choice];
        let table = Quality {
            patterns: vec![
                pattern(OUTGROWS_ITS_CACHE, &fields, Level::Warn),
                pattern("b", &fields, Level::Warn),
            ],
            ..Quality::default()
        };
        let bar = Bar::compile(&table).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = format!("a{}", "b".repeat(20));
        decision.origin = Some(coin_flips(12_000, 3));

        let (found, stopped) = bar.audit_patterns(&decision, &mut bar.page_scan());

        assert_eq!(found, []);
        let stopped = stopped.map(|s| (s.field, s.level, s.fault));
        let gave_up = QualityFault::GaveUp { index: 0 };
        assert_eq!(
            stopped,
            Some((DecisionField::Origin, Level::Refuse, gave_up))
        );
    }

    /// The seeded corpus of 600 decisions and a bar of the shipped table
    /// with patterns that match it: in the choice and the origin at once,
    /// in the reason at `refuse`, in tags and anchors.
    fn corpus() -> (TempDir, ProjectState, Bar) {
        let (tmp, state) = seeded();
        let bar = Bar::compile(&corpus_table()).unwrap();
        (tmp, state, bar)
    }

    /// The shipped table and patterns that match the seeded corpus.
    fn corpus_table() -> Quality {
        let mut quality = shipped();
        let choice_origin = [DecisionField::Choice, DecisionField::Origin];
        quality.patterns.extend([
            pattern(r"[0-9]7(?:\W|$)", &choice_origin, Level::Warn),
            pattern(r"task [0-9]*3(?:\W|$)", &choice_origin, Level::Refuse),
            pattern(
                r"(?:^|\W)crash(?:\W|$)",
                &[DecisionField::Reason],
                Level::Refuse,
            ),
            pattern(r"^s", &[DecisionField::Tags], Level::Warn),
            pattern(r"::[a-z]+_[a-z]+_", &[DecisionField::Anchors], Level::Warn),
        ]);
        quality
    }

    /// A report charges a decision what a write that records it charges:
    /// each pattern the texts it reads, its origin's included, one byte
    /// more per text, over every decision of the seeded corpus.
    #[test]
    fn a_report_charges_a_decision_what_a_write_that_records_it_charges() {
        let (_tmp, state) = seeded();
        let mut table = corpus_table();
        for pattern in &mut table.patterns {
            pattern.level = Level::Warn;
        }
        let bar = Bar::compile(&table).unwrap();
        for decision in state.decisions.values() {
            let mut scan = Scan::of(&bar.patterns, usize::MAX, usize::MAX);
            let fields = DecisionField::IN_FILE_ORDER.into_iter();
            let texts: Vec<_> = fields.flat_map(|field| texts(decision, field)).collect();

            bar.judge_patterns(&texts, &mut scan).unwrap();
            bar.judge_origin(decision.origin.as_deref(), &mut scan)
                .unwrap();

            assert_eq!(usize::MAX - scan.left, bar.pattern_bytes(decision));
        }
    }

    /// The `[quality]` table the repository ships.
    fn shipped() -> Quality {
        #[derive(Deserialize)]
        struct Table {
            quality: Quality,
        }
        let shipped: Table =
            toml::from_str(include_str!("../../../../examples/quality.toml")).unwrap();
        shipped.quality
    }

    /// The seeded corpus of 600 decisions.
    fn seeded() -> (TempDir, ProjectState) {
        let tmp = TempDir::new().unwrap();
        let state = Corpus::new(600)
            .write(tmp.path())
            .unwrap()
            .load_state()
            .unwrap();
        (tmp, state)
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
            QualityFault::Limit { .. }
            | QualityFault::GaveUp { .. }
            | QualityFault::CacheTotal { .. }
            | QualityFault::OriginRequired => panic!("{s:?}"),
        });
        Ok(hits.collect())
    }

    fn breach(breach: Breach) -> (Place, QualityFault) {
        match breach.broken {
            Broken::Quality(fault) => (breach.place, fault),
            Broken::Text { .. } | Broken::Work => panic!("{breach:?}"),
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
            let mut scan = Scan::of(&bar.patterns, usize::MAX, usize::MAX);

            let judged = bar.judge(decision, &DecisionField::IN_FILE_ORDER, &mut scan);
            let judged_origin = bar.judge_origin(decision.origin.as_deref(), &mut scan);

            assert_eq!(as_written(&fields), judged.map_err(breach), "{decision:?}");
            let origin_hits =
                as_written(&origin).map(|hits| hits.into_iter().map(|(_, hit)| hit).collect());
            assert_eq!(origin_hits, judged_origin.map_err(breach), "{decision:?}");
            let refused = |found: &[Shortfall]| found.iter().any(|s| s.level == Level::Refuse);
            let in_both = origin.iter().any(|o| {
                let index = |s: &Shortfall| match &s.fault {
                    QualityFault::Pattern(matched) => Some(matched.index),
                    QualityFault::Limit { .. }
                    | QualityFault::GaveUp { .. }
                    | QualityFault::CacheTotal { .. }
                    | QualityFault::OriginRequired => None,
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

    /// The patterns of the shipped table as they stood for the `regex`
    /// crate, by index, where a Unicode word boundary now reads `(?:^|\W)`
    /// and `(?:\W|$)`, or an ASCII one.
    const SHIPPED_FIRST_FORMS: &[(usize, &str)] = &[
        (0, r"(?i)\b(?:we|our)\b"),
        (1, r"(?i)\butili[sz](?:e|es|ed|ing)\b"),
        (
            2,
            r"(?i)\b(?:simply|basically|essentially|actually|obviously|leverages?|leveraged)\b",
        ),
        (
            3,
            r"\b\d+(?:[.,]\d+)?\s?(?:ns|us|ms|s|min|x|[KMGT]i?B)\b|\b\d+(?:[.,]\d+)?\s?%",
        ),
    ];

    /// This repository's own patterns as they stood for the `regex` crate.
    const OWN_FIRST_FORMS: &[(usize, &str)] = &[(
        1,
        r"(?i)\b(?:simply|basically|essentially|leverages?|leveraged|utili[sz]e[sd]?|robust|comprehensive|seamless|powerful|elegant)\b",
    )];

    /// This repository's own `[quality]` table.
    fn own() -> Quality {
        #[derive(Deserialize)]
        struct Project {
            quality: Quality,
        }
        let project: Project =
            toml::from_str(include_str!("../../../../.trurlic/project.toml")).unwrap();
        project.quality
    }

    /// Decisions whose choice, reason and alternative hold a word or a
    /// measurement the word rules read, between each pair of neighbours:
    /// none, a space, punctuation, an underscore, a letter or digit in
    /// ASCII and beyond it. A Unicode word boundary sees a word character
    /// on either side of each, so the rewrite must too.
    fn word_edges() -> Vec<Decision> {
        let words = [
            "we",
            "Our",
            "utilised",
            "simply",
            "leverages",
            "robust",
            "Elegant",
            "45ms",
            "3 %",
            "1.5 KiB",
        ];
        let sides = ["", " ", ".", "_", "x", "7", "\u{17c}", "\u{663}"];
        let mut decisions = Vec::new();
        for word in words {
            for left in sides {
                for right in sides {
                    let text = format!("{left}{word}{right}");
                    let mut decision = sample_decision("project");
                    decision.choice = format!("Keep {text} here");
                    decision.reason = format!("{text} and on.");
                    decision.alternatives = vec![text];
                    decisions.push(decision);
                }
            }
        }
        decisions
    }

    /// The shipped table and this repository's, rewritten for the lazy
    /// DFA, find what their first forms found on the `regex` crate, over
    /// the seeded corpus and every edge of a word: each pattern matches the
    /// same decisions in the same field and element. A pattern left as it
    /// was matches at the same byte; a rewritten word rule's match may start
    /// one character earlier, at the character before the word.
    #[test]
    fn the_rewritten_tables_find_what_their_first_forms_found() {
        let (_tmp, state) = seeded();
        let mut decisions: Vec<Decision> = state.decisions.into_values().collect();
        let corpus = decisions.len();
        decisions.extend(word_edges());
        let (shipped, own) = (shipped(), own());

        let in_corpus = first_form_findings(&decisions[..corpus], &shipped, SHIPPED_FIRST_FORMS);
        let shipped = first_form_findings(&decisions, &shipped, SHIPPED_FIRST_FORMS);
        let own = first_form_findings(&decisions, &own, OWN_FIRST_FORMS);

        assert!(in_corpus > 100, "{in_corpus}");
        assert!(shipped > in_corpus && own > 100, "{shipped} {own}");
    }

    /// The decisions of `decisions` each pattern of `quality` matches,
    /// checked against its first form in `first_forms`, or its own text
    /// where none is given, on the `regex` crate.
    fn first_form_findings(
        decisions: &[Decision],
        quality: &Quality,
        first_forms: &[(usize, &str)],
    ) -> usize {
        let bar = Bar::compile(quality).unwrap();
        let first: Vec<regex::Regex> = quality
            .patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                let before = first_forms.iter().find(|&&(at, _)| at == index);
                regex::Regex::new(before.map_or(pattern.pattern.as_str(), |&(_, text)| text))
                    .unwrap()
            })
            .collect();
        let mut scan = Scan::of(&bar.patterns, 0, usize::MAX);
        let mut matched = 0;
        for decision in decisions {
            let texts = audited(decision);
            let (found, stopped) = bar.audit_patterns(decision, &mut scan);
            assert_eq!(stopped, None, "{decision:?}");
            let found: Vec<(usize, Place, usize)> = found
                .iter()
                .map(|s| match &s.fault {
                    QualityFault::Pattern(found) => {
                        let place = Place {
                            field: s.field,
                            index: s.element,
                        };
                        (found.index, place, found.at)
                    }
                    QualityFault::Limit { .. }
                    | QualityFault::GaveUp { .. }
                    | QualityFault::CacheTotal { .. }
                    | QualityFault::OriginRequired => panic!("{s:?}"),
                })
                .collect();
            for (index, pattern) in quality.patterns.iter().enumerate() {
                let read = texts
                    .iter()
                    .filter(|(place, _)| pattern.fields.contains(&place.field));
                let before = read.clone().find_map(|(place, text)| {
                    let found = first[index].find(text)?;
                    Some((*place, found.start(), text))
                });
                let now: Vec<(Place, usize)> = found
                    .iter()
                    .filter(|&&(of, ..)| of == index)
                    .map(|&(_, place, at)| (place, at))
                    .collect();
                let Some((place, at, text)) = before else {
                    assert_eq!(now, [], "{index} {decision:?}");
                    continue;
                };
                matched += 1;
                let [(now_place, now_at)] = now[..] else {
                    panic!("{index} {now:?} {decision:?}")
                };
                assert_eq!(now_place, place, "{index} {decision:?}");
                let between = text.get(now_at..at).unwrap();
                assert!(between.chars().count() <= 1, "{index}: {between:?}");
                assert!(
                    between.is_empty() || first_forms.iter().any(|&(at, _)| at == index),
                    "{index}"
                );
            }
        }
        matched
    }

    /// A table of 32 ordinary patterns: the shipped table's 11, then word
    /// and phrase lists, word shapes, addresses and scripts.
    const ORDINARY: [&str; 32] = [
        r"(?i)(?:^|\W)(?:we|our)(?:\W|$)",
        r"(?i)(?:^|\W)utili[sz](?:e|es|ed|ing)(?:\W|$)",
        r"(?i)(?:^|\W)(?:simply|basically|essentially|actually|obviously|leverages?|leveraged)(?:\W|$)",
        r"(?:^|\W)\d+(?:[.,]\d+)?\s?(?:(?:ns|us|ms|s|min|x|[KMGT]i?B)(?:\W|$)|%)",
        r"^(?:[^ ]+ +){25}[^ ]",
        r";.*;",
        r"^\s*(?:[^ \t\n]+[ \t\n]+){120}[^ \t\n]",
        r"(?:[^ \t\n.;:!?]+[ \t\n]+){25}[^ \t\n.;:!?]",
        r"^[^:]*$",
        r"rejected:(?:[ \t\n]+[^ \t\n]+){16}",
        r"(?:^|/)tests?/",
        r"(?i)(?:^|\W)\w+ly(?:\W|$)",
        r"(?i)(?:^|\W)(?:we|our|us)(?:\W|$)",
        r"\w{3,}",
        r"(?i)(?:^|\W)(?:todo|fixme|xxx|tbd)(?:\W|$)",
        r"\p{Lu}{2,}\d+",
        r"(?:^|\W)\d+(?:[.,]\d+)?\s?(?:ms|s)(?:\W|$)",
        "(?i)(?:^|\\W)(?:bardzo|naprawd\u{119}|po prostu|w\u{142}a\u{15b}ciwie)(?:\\W|$)",
        r"[\p{Han}\p{Hiragana}\p{Katakana}]",
        r"(?i)(?:^|\W)\w+(?:ing|ed)(?:\W|$)",
        r"\w+@\w+\.\w+",
        r"https?://\S+",
        r"(?i)\w+ment\w*",
        "(?i)\\w+o\u{15b}\u{107}",
        r"(?i)[^\x00-\x7f]",
        r"\p{L}+\d+\p{L}+",
        r"(?i)(?:^|\W)(?:very|really|quite|rather|somewhat|fairly|pretty|just|simply|basically|essentially|actually|obviously|clearly|literally|totally|completely|absolutely|definitely|certainly|probably|possibly|perhaps|maybe|arguably|virtually|practically|seemingly|apparently|generally|usually|typically|often|sometimes|frequently|rarely|hardly|barely|merely|only|even|still|already|yet|again|always|never|ever)(?:\W|$)",
        r"(?i)(?:^|\W)(?:in order to|due to the fact that|at this point in time|in the event that|for the purpose of|with regard to|in terms of|it is important to note|as a matter of fact|a number of|the fact that|in spite of|in the case of|on the other hand|as well as|in addition to|by means of|for the most part)(?:\W|$)",
        r"(?i)(?:^|\W)(?:should|must|may|might|could)(?:\W|$)",
        r"(?:^|\W)[A-Z]{2,}(?:\W|$)",
        r"\s{2,}",
        r"(?i)(?:^|\W)(?:will|would)(?:\W|$)",
    ];

    /// [`ORDINARY`] as a table over the prose fields.
    fn ordinary() -> Quality {
        let prose = [
            DecisionField::Choice,
            DecisionField::Reason,
            DecisionField::Alternatives,
        ];
        Quality {
            patterns: ORDINARY
                .iter()
                .map(|text| pattern(text, &prose, Level::Warn))
                .collect(),
            ..Quality::default()
        }
    }

    /// Decisions of English prose, fixed: the choice, reason, alternatives
    /// and origin of each of this repository's decisions as they stood when
    /// the caches were sized, in name order.
    fn english() -> Vec<Decision> {
        #[derive(Deserialize)]
        struct Prose {
            choice: String,
            reason: String,
            alternatives: Vec<String>,
            origin: Option<String>,
        }
        let lines = include_str!("../../../../tests/fixtures/prose/decisions.jsonl").lines();
        lines
            .map(|line| {
                let prose: Prose = serde_json::from_str(line).unwrap();
                let mut decision = sample_decision("project");
                decision.choice = prose.choice;
                decision.reason = prose.reason;
                decision.alternatives = prose.alternatives;
                decision.origin = prose.origin;
                decision
            })
            .collect()
    }

    /// A reason of words in fifteen scripts, the letters of each drawn from
    /// its block in an order fixed by `seed`.
    fn many_scripts(seed: u64) -> String {
        let blocks: [(u32, u32); 15] = [
            (0x61, 26),
            (0xE0, 31),
            (0x100, 128),
            (0x3B1, 25),
            (0x430, 32),
            (0x5D0, 27),
            (0x627, 25),
            (0x905, 50),
            (0xE01, 46),
            (0x10D0, 33),
            (0x3041, 86),
            (0x30A1, 90),
            (0x4E00, 2000),
            (0xAC00, 2000),
            (0x531, 38),
        ];
        let mut state = seed;
        let mut next = |below: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u32::try_from(state >> 33).unwrap() % below
        };
        let mut words = Vec::new();
        for _ in 0..400 {
            let (start, len) = blocks[usize::try_from(next(15)).unwrap()];
            let letters = 3 + next(7);
            let word: String = (0..letters)
                .map(|_| char::from_u32(start + next(len)).unwrap())
                .collect();
            words.push(word);
        }
        words.join(" ")
    }

    /// The cache the patterns of `quality`, at `warn` so that none stops at
    /// a match, grow over `batch` as one write records it, within `total`;
    /// or what stopped them.
    fn grown(quality: &Quality, batch: &[Decision], total: usize) -> Result<usize, Broken> {
        let mut table = quality.clone();
        for pattern in &mut table.patterns {
            pattern.level = Level::Warn;
        }
        let bar = Bar::compile(&table).unwrap();
        let mut scan = Scan::of(&bar.patterns, usize::MAX, total);
        let origin = batch
            .first()
            .and_then(|decision| decision.origin.as_deref());
        let judged = bar.judge_origin(origin, &mut scan);
        judged.map_err(|breach| breach.broken)?;
        for decision in batch {
            let all = DecisionField::IN_FILE_ORDER;
            let held = all.into_iter().filter(|&f| f != DecisionField::Origin);
            let texts: Vec<_> = held.flat_map(|f| texts(decision, f)).collect();
            let judged = bar.judge_patterns(&texts, &mut scan);
            judged.map_err(|breach| breach.broken)?;
        }
        Ok(total - scan.cache_left)
    }

    /// Every full batch of fifty decisions of English prose, and the same
    /// with a reason in fifteen scripts beside it, fits each pattern's cache
    /// and the write's total under 32 ordinary patterns; so do the shipped
    /// table and this repository's.
    #[test]
    fn a_full_batch_of_prose_fits_the_caches_and_the_total_of_an_ordinary_table() {
        let decisions = english();
        let mut scripts = sample_decision("project");
        scripts.reason = many_scripts(5);
        let total = MAX_PATTERN_CACHE_TOTAL_BYTES;
        let mut most = 0;

        for batch in decisions.chunks(50) {
            let mut with_scripts = batch.to_vec();
            with_scripts.push(scripts.clone());
            for table in [ordinary(), shipped(), own()] {
                let plain = grown(&table, batch, total);
                let mixed = grown(&table, &with_scripts, total);
                assert!(plain.is_ok() && mixed.is_ok(), "{plain:?} {mixed:?}");
                most = most.max(mixed.unwrap_or(total));
            }
        }
        assert!(most > total / 4, "{most}");
    }

    /// Each of 32 ordinary patterns judges a whole graph of English prose,
    /// one page with no total, within its cache, and the one whose caches
    /// grow the most, a list of 48 words, fills more than half of it.
    #[test]
    fn each_ordinary_pattern_judges_a_whole_graph_of_prose_within_its_cache() {
        let decisions = english();
        let mut most = 0;

        for pattern in ordinary().patterns {
            let one = Quality {
                patterns: vec![pattern],
                ..Quality::default()
            };
            let bar = Bar::compile(&one).unwrap();
            let mut scan = Scan::of(&bar.patterns, 0, usize::MAX);
            for decision in &decisions {
                let (_, stopped) = bar.audit_patterns(decision, &mut scan);
                assert_eq!(stopped, None, "{:?}", one.patterns);
            }
            most = most.max(usize::MAX - scan.cache_left);
        }

        assert!(most > MAX_PATTERN_CACHE_BYTES / 2, "{most}");
    }

    /// The total of cache is exact: a write that may grow the caches by
    /// what a batch grows passes, and one byte less is refused as the
    /// total, naming the pattern that grew the most.
    #[test]
    fn a_write_grows_its_caches_by_their_total_and_no_more() {
        let decisions = english();
        let batch = decisions.get(..50).unwrap();
        let needed = grown(&ordinary(), batch, usize::MAX).unwrap();

        let enough = grown(&ordinary(), batch, needed);
        let short = grown(&ordinary(), batch, needed - 1);

        assert_eq!(enough, Ok(needed));
        let Err(Broken::Quality(QualityFault::CacheTotal { .. })) = short else {
            panic!("{short:?}")
        };
    }
}
