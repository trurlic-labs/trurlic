//! The bar's judgement of a stored decision for a report: every rule it
//! breaks, where a write stops at the first. The limits are judged apart
//! from the patterns, and each pattern apart from the others, so a report
//! chooses which to pay for; a pattern reads a decision as a write reads
//! an op and its batch's origin.

use std::borrow::Cow;

use super::{Bar, Breach, Broken, PatternCache, PatternFault, Place, searched, texts};
use crate::store::quality::{DecisionField, Level, Quality, QualityFault};
use crate::store::schema::Decision;

/// A rule of the bar a stored decision breaks, and where: what a report
/// shows of a graph below its own bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    pub field: DecisionField,
    /// The element of a list field, by its position in the decision's list.
    pub element: Option<usize>,
    /// `refuse` for a limit and a give-up, the pattern's level for a match.
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
            let read = texts
                .iter()
                .filter(|(place, _)| pattern.fields.contains(&place.field));
            searched(read.map(|(_, text)| text.as_ref()))
        };
        self.patterns
            .iter()
            .map(read)
            .fold(0, usize::saturating_add)
    }

    /// The pattern at `index` of `[quality] patterns` against `decision` as
    /// stored, at the pattern's level, searched over `cache`. First its
    /// match in the stored `origin`, when it reads `origin`, as
    /// [`Bar::judge_origin`] reads a batch's. Then its first match over the
    /// other fields it reads, as [`Bar::judge_patterns`] reads an op's. A
    /// write that recorded the decision would give both. A cache that gives
    /// up ends the pattern's findings with that one, at `refuse`.
    /// `require_origin` binds a batch that records, which a stored decision
    /// is no longer.
    pub(crate) fn audit_pattern(
        &self,
        index: usize,
        decision: &Decision,
        cache: &mut PatternCache,
    ) -> Vec<Shortfall> {
        let Some(pattern) = self.patterns.get(index) else {
            return Vec::new();
        };
        let texts = audited(decision);
        let (origin, fields): (Vec<_>, Vec<_>) = texts
            .iter()
            .filter(|(place, _)| pattern.fields.contains(&place.field))
            .map(|(place, text)| (*place, text.as_ref()))
            .partition(|(place, _)| place.field == DecisionField::Origin);
        let mut found = Vec::new();
        for part in [origin, fields] {
            match pattern.first_match(cache, part) {
                Ok(Some((place, matched))) => found.push(Shortfall {
                    field: place.field,
                    element: place.index,
                    level: pattern.level,
                    fault: QualityFault::Pattern(pattern.matched(index, &matched)),
                }),
                Ok(None) => {}
                Err(place) => {
                    found.push(Shortfall {
                        field: place.field,
                        element: place.index,
                        level: Level::Refuse,
                        fault: QualityFault::GaveUp { index },
                    });
                    break;
                }
            }
        }
        found
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
        Broken::Quality(fault) => Some(Shortfall {
            field: breach.place.field,
            element: breach.place.index,
            level: Level::Refuse,
            fault,
        }),
        Broken::Text { .. } | Broken::Work { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::*;
    use crate::store::corpus::Corpus;
    use crate::store::limits::{MAX_WRITE_PATTERN_BYTES, MAX_WRITE_PATTERN_CACHE_BYTES};
    use crate::store::quality::{Hit, Pattern};
    use crate::store::schema::Anchor;
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

    /// Every rule `bar` finds `decision` breaks, the limits, then each
    /// pattern in its order, each over a fresh cache.
    fn audit(bar: &Bar, decision: &Decision) -> Vec<Shortfall> {
        let mut caches = bar.caches();
        let patterns = caches
            .iter_mut()
            .enumerate()
            .flat_map(|(index, cache)| bar.audit_pattern(index, decision, cache));
        let mut found = bar.audit_limits(decision);
        found.extend(patterns);
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
        assert!(limits.caches().is_empty());
        assert_eq!(audit(&limits, &decision).len(), 5);
    }

    /// A pattern whose cache gives up in a decision's origin gives that
    /// finding, at `refuse` whatever its level, and judges no field after.
    #[test]
    fn a_give_up_is_a_refuse_finding_and_the_pattern_reads_no_further() {
        let fields = [DecisionField::Origin, DecisionField::Choice];
        let table = Quality {
            patterns: vec![pattern(OUTGROWS_ITS_CACHE, &fields, Level::Warn)],
            ..Quality::default()
        };
        let bar = Bar::compile(&table).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = format!("a{}", "b".repeat(20));
        decision.origin = Some(coin_flips(4_000, 3));
        let mut cache = bar.caches().remove(0);

        let found = bar.audit_pattern(0, &decision, &mut cache);

        let first = found.first().map(|s| (s.field, s.level, &s.fault));
        let gave_up = QualityFault::GaveUp { index: 0 };
        assert_eq!(
            (found.len(), first),
            (1, Some((DecisionField::Origin, Level::Refuse, &gave_up)))
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
            let mut scan = bar.scan(usize::MAX, usize::MAX);
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
            | QualityFault::OriginRequired => panic!("{s:?}"),
        });
        Ok(hits.collect())
    }

    fn breach(breach: Breach) -> (Place, QualityFault) {
        match breach.broken {
            Broken::Quality(fault) => (breach.place, fault),
            Broken::Text { .. } | Broken::Work { .. } => panic!("{breach:?}"),
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
            let mut scan = bar.scan(usize::MAX, usize::MAX);

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
        let mut caches = bar.caches();
        let mut matched = 0;
        for decision in decisions {
            let texts = audited(decision);
            for (index, (pattern, cache)) in quality.patterns.iter().zip(&mut caches).enumerate() {
                let read = texts
                    .iter()
                    .filter(|(place, _)| pattern.fields.contains(&place.field));
                let before = read.clone().find_map(|(place, text)| {
                    let found = first[index].find(text)?;
                    Some((*place, found.start(), text))
                });
                let now = bar.audit_pattern(index, decision, cache);
                let now: Vec<(Place, usize)> = now
                    .iter()
                    .map(|s| match &s.fault {
                        QualityFault::Pattern(found) => (
                            Place {
                                field: s.field,
                                index: s.element,
                            },
                            found.at,
                        ),
                        QualityFault::Limit { .. }
                        | QualityFault::GaveUp { .. }
                        | QualityFault::OriginRequired => panic!("{s:?}"),
                    })
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

    /// The limits of a bar a batch's records are written at.
    struct AtLimits {
        reason_words: usize,
        alternatives: usize,
        alternative_words: usize,
        anchors: usize,
    }

    /// The shipped table's limits.
    const SHIPPED_LIMITS: AtLimits = AtLimits {
        reason_words: 180,
        alternatives: 4,
        alternative_words: 30,
        anchors: 8,
    };

    /// This repository's limits.
    const OWN_LIMITS: AtLimits = AtLimits {
        reason_words: 300,
        alternatives: 5,
        alternative_words: 50,
        anchors: 10,
    };

    /// The fifty records of a full batch at `limits`, their prose the
    /// words of the seeded corpus's reasons in turn.
    fn full_batch(state: &ProjectState, limits: &AtLimits) -> Vec<Decision> {
        let words: Vec<&str> = state
            .decisions
            .values()
            .flat_map(|decision| decision.reason.split_whitespace())
            .collect();
        let mut at = 0;
        let mut take = |count: usize| {
            let taken: Vec<&str> = (0..count).map(|n| words[(at + n) % words.len()]).collect();
            at += count;
            taken.join(" ")
        };
        (0..50)
            .map(|index| {
                let mut decision = sample_decision("project");
                let choice = take(15);
                decision.choice = choice.chars().take(190).collect();
                decision.reason = take(limits.reason_words);
                decision.alternatives = (0..limits.alternatives)
                    .map(|_| {
                        let option = take(4);
                        format!(
                            "{option} - rejected: {}",
                            take(limits.alternative_words - 6)
                        )
                    })
                    .collect();
                decision.tags = ["quality".into(), "apply".into()].into();
                decision.anchors = (0..limits.anchors)
                    .map(|k| {
                        let file = format!("src/store/module_{index}/file_{k}.rs");
                        Anchor::new(file, Some(format!("function_name_{k}"))).unwrap()
                    })
                    .collect();
                decision.origin = Some("a full batch".into());
                decision
            })
            .collect()
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
        let mut scan = bar.scan(MAX_WRITE_PATTERN_BYTES, total);
        for decision in batch {
            bar.judge_origin(decision.origin.as_deref(), &mut scan)
                .and_then(|_| {
                    let all = DecisionField::IN_FILE_ORDER;
                    let texts: Vec<_> = all.into_iter().flat_map(|f| texts(decision, f)).collect();
                    bar.judge_patterns(&texts, &mut scan)
                })
                .map_err(|breach| breach.broken)?;
        }
        Ok(total - scan.cache_left)
    }

    /// A full batch at its limits fits the write's total of cache four
    /// times over under the shipped table, and passes under this
    /// repository's table, and under the shipped patterns with this
    /// repository's limits.
    #[test]
    fn a_full_batch_of_an_ordinary_table_fits_the_write_total_of_cache() {
        let (_tmp, state) = seeded();
        let (shipped, own) = (shipped(), own());
        let total = MAX_WRITE_PATTERN_CACHE_BYTES;

        let at_its_limits = grown(&shipped, &full_batch(&state, &SHIPPED_LIMITS), total);
        let own_batch = full_batch(&state, &OWN_LIMITS);

        let grown_shipped = at_its_limits.unwrap();
        assert!(4 * grown_shipped <= total, "{grown_shipped}");
        grown(&own, &own_batch, total).unwrap();
        grown(&shipped, &own_batch, total).unwrap();
    }

    /// The total of cache is exact: a write that may grow the caches by
    /// what a batch grows passes, and one byte less gives up as a full
    /// cache does, naming the pattern.
    #[test]
    fn one_byte_of_cache_less_than_a_batch_grows_gives_up() {
        let (_tmp, state) = seeded();
        let (shipped, batch) = (shipped(), full_batch(&state, &SHIPPED_LIMITS));
        let needed = grown(&shipped, &batch, usize::MAX).unwrap();

        let enough = grown(&shipped, &batch, needed);
        let short = grown(&shipped, &batch, needed - 1);

        assert_eq!(enough, Ok(needed));
        let Err(Broken::Quality(QualityFault::GaveUp { .. })) = short else {
            panic!("{short:?}")
        };
    }
}
