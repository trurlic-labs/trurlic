//! The quality bar through `Writer::apply`, the one write of every
//! surface: each rule refusing with its name, field and measure, each
//! `[quality]` key moving its limit, and a pattern reading only its fields
//! at its level.

use std::fs;

use tempfile::TempDir;

use super::{anchor, apply, batch, graph, named, record, refused, revise};
use crate::Error;
use crate::store::apply::{Applied, Merge, Op, Record};
use crate::store::canonical::canonical;
use crate::store::limits::{MAX_QUALITY_PATTERNS, MAX_WRITE_PATTERN_BYTES};
use crate::store::schema::Anchor;
use crate::store::state::NodeFile;
use crate::store::testing::{OUTGROWS_ITS_CACHE, coin_flips, sample_decision, write_node};
use crate::store::{ListEdit, PatternNote, QualityFault, Rule, Store, Warning, WarningKind};

/// `table` appended to `project.toml` as its `[quality]`.
fn with_quality(store: &Store, table: &str) {
    let path = store.project_path();
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\n[quality]\n{table}\n")).unwrap();
}

/// A store with the test graph, under `table` when one is given.
fn store_under(tmp: &TempDir, table: Option<&str>) -> Store {
    let store = graph(tmp.path());
    if let Some(table) = table {
        with_quality(&store, table);
    }
    store
}

fn words(count: usize) -> String {
    vec!["word"; count].join(" ")
}

/// `record("expire", "auth")` with `change` applied.
fn expire(change: impl FnOnce(&mut Record)) -> Vec<Op> {
    let mut op = record("expire", "auth");
    change(&mut op);
    vec![Op::Record(op)]
}

/// The built-in rules refuse under any table, with no `[quality]` at all.
#[test]
fn each_built_in_rule_refuses_naming_the_field_the_rule_and_the_measure() {
    let cases: [(Vec<Op>, &str, &str, &str); 4] = [
        (
            expire(|op| op.alternatives.clear()),
            "alternative_required",
            "ops[0].alternatives",
            "0 alternatives, under the minimum of 1 (built in)",
        ),
        (
            expire(|op| op.anchors.clear()),
            "anchor_required",
            "ops[0].anchors",
            "0 anchors, under the minimum of 1 (built in)",
        ),
        (
            expire(|op| op.choice = "c".repeat(201)),
            "invalid_text",
            "ops[0].choice",
            "must be at most 200 bytes (201 given)",
        ),
        (
            expire(|op| op.reason = "Too short".into()),
            "invalid_text",
            "ops[0].reason",
            "at least 10 bytes of reasoning",
        ),
    ];
    for (ops, rule, path, measure) in cases {
        let tmp = TempDir::new().unwrap();
        let store = store_under(&tmp, None);
        let (name, at, message) = named(&refused(&store, ops));
        assert_eq!((name, at.as_str()), (rule, path), "{message}");
        let prefix = format!("ops[0] (record `expire`), `{}`: ", &path[7..]);
        assert!(message.starts_with(&prefix), "{message}");
        assert!(message.contains(measure), "{measure:?} not in {message}");
    }
}

/// A `[quality]` key, `{}` standing for its limit, and a record that
/// measures `measured` against it.
struct KeyCase {
    key: &'static str,
    measured: usize,
    refused_at: usize,
    ops: Vec<Op>,
    rule: &'static str,
    path: &'static str,
    message: &'static str,
}

/// Each key: a record passes with no table and at a limit equal to what
/// it measures, and is refused one past it, naming the key.
#[test]
fn each_quality_key_moves_its_limit() {
    let cases = [
        KeyCase {
            key: "reason_words = { min = {} }",
            measured: 6,
            refused_at: 7,
            ops: expire(|op| op.reason = words(6)),
            rule: "reason_words_min",
            path: "ops[0].reason",
            message: "6 words, under the minimum of 7 (`[quality] reason_words.min`)",
        },
        KeyCase {
            key: "reason_words = { max = {} }",
            measured: 6,
            refused_at: 5,
            ops: expire(|op| op.reason = words(6)),
            rule: "reason_words_max",
            path: "ops[0].reason",
            message: "6 words, over the maximum of 5 (`[quality] reason_words.max`)",
        },
        KeyCase {
            key: "alternatives_max = {}",
            measured: 2,
            refused_at: 1,
            ops: expire(|op| op.alternatives = vec!["One".into(), "Two".into()]),
            rule: "alternatives_max",
            path: "ops[0].alternatives",
            message: "2 alternatives, over the maximum of 1 (`[quality] alternatives_max`)",
        },
        KeyCase {
            key: "alternative_words_max = {}",
            measured: 3,
            refused_at: 2,
            ops: expire(|op| op.alternatives = vec!["One".into(), words(3)]),
            rule: "alternative_words_max",
            path: "ops[0].alternatives[1]",
            message: "3 words, over the maximum of 2 (`[quality] alternative_words_max`)",
        },
        KeyCase {
            key: "anchors_max = {}",
            measured: 2,
            refused_at: 1,
            ops: expire(|op| op.anchors.push(anchor("src/b.rs", None))),
            rule: "anchors_max",
            path: "ops[0].anchors",
            message: "2 anchors, over the maximum of 1 (`[quality] anchors_max`)",
        },
    ];
    for case in cases {
        let at_limit = case.key.replace("{}", &case.measured.to_string());
        for table in [None, Some(at_limit.as_str())] {
            let tmp = TempDir::new().unwrap();
            let store = store_under(&tmp, table);
            apply(&store, case.ops.clone()).unwrap_or_else(|e| panic!("{table:?}: {e}"));
        }
        let tmp = TempDir::new().unwrap();
        let table = case.key.replace("{}", &case.refused_at.to_string());
        let store = store_under(&tmp, Some(&table));
        let (name, at, said) = named(&refused(&store, case.ops));
        assert_eq!((name, at.as_str()), (case.rule, case.path), "{table}");
        assert!(said.ends_with(case.message), "{said}");
    }
}

#[test]
fn require_origin_refuses_a_record_in_a_batch_without_an_origin() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(&tmp, Some("require_origin = true"));
    let without = |ops| {
        let mut batch = batch(ops);
        batch.origin = None;
        batch
    };

    let err = super::apply_batch(&store, without(expire(|_| {}))).unwrap_err();

    let Error::Refused(refusal) = err else {
        panic!("{err}")
    };
    assert_eq!(
        named(&refusal),
        (
            "origin_required",
            "origin".to_owned(),
            "`origin`: is required: the batch records a decision, and \
             `[quality] require_origin` is set"
                .to_owned()
        )
    );
    let mut tags = revise("sign");
    tags.tags = super::add(&["keys"]);
    let writer = super::apply_batch(&store, without(vec![Op::Revise(tags)]));
    writer.expect("a batch that records nothing needs no origin");
    apply(&store, expire(|_| {})).expect("a batch with an origin records");
}

/// A record whose every field holds `zz`, under one pattern for `zz` that
/// reads `field` at `level`.
fn under_pattern(tmp: &TempDir, field: &str, level: &str) -> (Store, Vec<Op>) {
    let table = format!(
        "[[quality.patterns]]\npattern = 'zz'\nfields = [\"{field}\"]\nlevel = \"{level}\"\n\
         message = \"No zz.\""
    );
    let store = store_under(tmp, Some(&table));
    let ops = expire(|op| {
        op.choice = "Choice zz".into();
        op.reason = "Reason that holds zz".into();
        op.alternatives = vec!["Plain way".into(), "Other zz way".into()];
        op.tags = vec!["plain".into(), "zz".into()];
        op.anchors = vec![anchor("src/a.rs", None), anchor("src/zz.rs", None)];
    });
    (store, ops)
}

fn with_origin(ops: Vec<Op>, origin: &str) -> crate::store::Batch {
    let mut batch = batch(ops);
    batch.origin = Some(origin.into());
    batch
}

/// Each field: a `refuse` pattern refuses at the first element of that
/// field it matches, a `warn` one writes and warns there, and neither
/// reads another field's text.
#[test]
fn a_pattern_matches_only_its_fields_at_its_level() {
    let cases = [
        ("choice", "ops[0].choice", 7),
        ("reason", "ops[0].reason", 18),
        ("alternatives", "ops[0].alternatives[1]", 6),
        ("tags", "ops[0].tags[1]", 0),
        ("anchors", "ops[0].anchors[1]", 4),
        ("origin", "origin", 0),
    ];
    for (field, path, at) in cases {
        let tmp = TempDir::new().unwrap();
        let (store, ops) = under_pattern(&tmp, field, "refuse");
        let err = super::apply_batch(&store, with_origin(ops, "zz row")).unwrap_err();
        let Error::Refused(refusal) = err else {
            panic!("{err}")
        };
        let (rule, refused_at, message) = named(&refusal);
        assert_eq!((rule, refused_at.as_str()), ("pattern", path));
        let said = format!("matches `[quality] patterns[0]` at byte {at} (`zz`): No zz.");
        assert!(message.ends_with(&said), "{message}");

        let tmp = TempDir::new().unwrap();
        let (store, ops) = under_pattern(&tmp, field, "warn");
        let applied: Applied = super::apply_batch(&store, with_origin(ops, "zz row")).unwrap();
        let [warning] = applied.warnings.as_slice() else {
            panic!("{field}: {:?}", applied.warnings)
        };
        assert_eq!(
            (
                warning.field.as_str(),
                warning.kind,
                warning.pattern,
                warning.at
            ),
            (path, WarningKind::Pattern, 0, at)
        );
        assert_eq!(warning.op.is_some(), field != "origin");
        assert_eq!(
            applied.patterns,
            [PatternNote {
                index: 0,
                message: "No zz.".into()
            }]
        );

        let tmp = TempDir::new().unwrap();
        let (store, ops) = under_pattern(&tmp, field, "refuse");
        let clean = ops_without_zz_in(ops, field);
        let origin = if field == "origin" { "row 7" } else { "zz row" };
        super::apply_batch(&store, with_origin(clean, origin))
            .unwrap_or_else(|e| panic!("{field} matched another field: {e}"));
    }
}

/// `ops`, one record, with `zz` gone from `field` alone.
fn ops_without_zz_in(mut ops: Vec<Op>, field: &str) -> Vec<Op> {
    let Some(Op::Record(op)) = ops.first_mut() else {
        return ops;
    };
    match field {
        "choice" => op.choice = "Choice".into(),
        "reason" => op.reason = "Reason that holds".into(),
        "alternatives" => op.alternatives = vec!["Plain way".into()],
        "tags" => op.tags = vec!["plain".into()],
        "anchors" => op.anchors = vec![anchor("src/a.rs", None)],
        _ => {}
    }
    ops
}

/// A revise is judged in the fields it changes alone: one below the bar
/// that it leaves as it is passes, and one it writes must meet the bar.
#[test]
fn a_revise_meets_the_bar_in_the_fields_it_changes_alone() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(
        &tmp,
        Some(
            "alternatives_max = 1\n[[quality.patterns]]\npattern = 'zz'\n\
             fields = [\"choice\", \"reason\"]\nlevel = \"refuse\"\nmessage = \"No zz.\"",
        ),
    );
    // Below the bar: no anchor, three alternatives, `zz` in its choice.
    let mut below = sample_decision("auth");
    below.choice = "Choice zz".into();
    below.alternatives = vec!["A".into(), "B".into(), "C".into()];
    write_node(&store, &NodeFile::Decision("below".into()), &below);

    let mut reason = revise("below");
    reason.reason = Some("A reason that says more".into());
    apply(&store, vec![Op::Revise(reason)]).unwrap();
    let mut tags = revise("below");
    tags.tags = super::add(&["keys"]);
    apply(&store, vec![Op::Revise(tags)]).unwrap();

    let mut alternatives = revise("below");
    alternatives.alternatives = ListEdit {
        remove: Some(vec!["C".into()]),
        ..ListEdit::keep()
    };
    let (rule, path, _) = named(&refused(&store, vec![Op::Revise(alternatives)]));
    assert_eq!(
        (rule, path.as_str()),
        ("alternatives_max", "ops[0].alternatives")
    );
    let mut choice = revise("below");
    choice.choice = Some("Choice zz again".into());
    let (rule, path, _) = named(&refused(&store, vec![Op::Revise(choice)]));
    assert_eq!((rule, path.as_str()), ("pattern", "ops[0].choice"));
    let mut anchors = revise("below");
    anchors.anchors = ListEdit {
        whole: Some(vec![]),
        ..ListEdit::keep()
    };
    let mut one = revise("below");
    one.anchors = ListEdit {
        add: Some(vec![anchor("src/a.rs", None)]),
        ..ListEdit::keep()
    };
    apply(&store, vec![Op::Revise(one)]).unwrap();
    let (rule, path, _) = named(&refused(&store, vec![Op::Revise(anchors)]));
    assert_eq!((rule, path.as_str()), ("anchor_required", "ops[0].anchors"));
}

/// A merge is judged in the anchors and tags it brings the survivor.
#[test]
fn a_merge_that_takes_the_survivor_past_a_limit_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(&tmp, Some("anchors_max = 1"));
    for (name, file) in [
        ("one", "src/one.rs"),
        ("two", "src/two.rs"),
        ("same", "src/one.rs"),
    ] {
        let mut decision = sample_decision("auth");
        decision.anchors = [Anchor::new(file.into(), None).unwrap()].into();
        write_node(&store, &NodeFile::Decision(name.into()), &decision);
    }
    let merge = |absorbed: &str| {
        vec![Op::Merge(Merge {
            survivor: "one".into(),
            absorbed: absorbed.into(),
        })]
    };

    let (rule, path, message) = named(&refused(&store, merge("two")));

    assert_eq!((rule, path.as_str()), ("anchors_max", "ops[0].anchors"));
    assert!(
        message.contains("2 anchors, over the maximum of 1"),
        "{message}"
    );
    apply(&store, merge("same")).expect("a merge that adds no anchor is not judged on them");
}

/// The batch is refused before any op when a pattern does not compile,
/// naming `project.toml`, the pattern's index and the engine's message.
#[test]
fn a_pattern_that_does_not_compile_refuses_every_batch() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(
        &tmp,
        Some(
            "[[quality.patterns]]\npattern = 'ok'\nfields = [\"choice\"]\nlevel = \"warn\"\n\
             message = \"M\"\n[[quality.patterns]]\npattern = '(unclosed'\n\
             fields = [\"choice\"]\nlevel = \"warn\"\nmessage = \"M\"",
        ),
    );
    let before = canonical(&store.load_state().unwrap().decisions["sign"]);

    let err = apply(&store, expire(|_| {})).unwrap_err();

    let Error::QualityPattern {
        path,
        index,
        message,
    } = &err
    else {
        panic!("{err}")
    };
    assert_eq!((path, *index), (&store.project_path(), 1));
    assert!(message.contains("unclosed group"), "{message}");
    assert!(
        err.to_string()
            .contains("`[quality] patterns[1].pattern` does not compile")
    );
    assert_eq!(
        fs::read_to_string(store.decision_path("sign")).unwrap(),
        before
    );
}

/// One warning per op and pattern, at its first match, in op order.
#[test]
fn a_warn_pattern_warns_once_per_op_at_its_first_match() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(
        &tmp,
        Some(
            "[[quality.patterns]]\npattern = 'zz'\nfields = [\"choice\", \"reason\"]\n\
             level = \"warn\"\nmessage = \"No zz.\"",
        ),
    );
    let mut second = record("second", "auth");
    second.reason = "A reason zz with zz twice".into();
    let ops = expire(|op| op.choice = "Choice zz".into())
        .into_iter()
        .chain([Op::Record(second)])
        .collect();

    let applied = apply(&store, ops).unwrap();

    let warnings: Vec<(Option<usize>, &str, usize, usize)> = applied
        .warnings
        .iter()
        .map(|warning: &Warning| {
            let Warning {
                op,
                field,
                pattern,
                at,
                ..
            } = warning;
            (*op, field.as_str(), *pattern, *at)
        })
        .collect();
    assert_eq!(
        warnings,
        [
            (Some(0), "ops[0].choice", 0, 7),
            (Some(1), "ops[1].reason", 0, 9)
        ]
    );
    assert_eq!(applied.patterns.len(), 1);
    assert_eq!(applied.ops[1].more_warnings, 0);
}

/// `tags` holding `zz`, written to the decision `name` by hand.
fn tagged_zz(store: &Store, name: &str) {
    let mut decision = store.load_state().unwrap().decisions.remove(name).unwrap();
    decision.tags.insert("zz".into());
    write_node(store, &NodeFile::Decision(name.into()), &decision);
}

/// A verdict of the bar on one element names the argument that brought
/// it and its index there, in every list field and in each form (a
/// record's list, a revise's whole list and its `add_*` list), whatever
/// the element's place in the decision's sorted set; an element no
/// argument brought (one held before, a merge's union) names the field.
/// A `refuse` and a `warn` pattern name it alike.
#[test]
fn a_verdict_on_an_element_names_the_argument_that_brought_it() {
    let strings = |items: &[&str]| -> Vec<String> { items.iter().map(|&i| i.into()).collect() };
    let files = |items: &[&str]| -> Vec<_> { items.iter().map(|&f| anchor(f, None)).collect() };
    let revised = |change: &dyn Fn(&mut crate::store::Revise)| {
        let mut op = revise("sign");
        change(&mut op);
        vec![Op::Revise(op)]
    };
    let whole = |items: Vec<String>| ListEdit {
        whole: Some(items),
        ..ListEdit::keep()
    };
    let added = |items: Vec<String>| ListEdit {
        add: Some(items),
        ..ListEdit::keep()
    };
    let before_twice = |op: &mut crate::store::Revise| {
        op.alternatives = whole(strings(&["Old way"]));
    };
    /// What a case writes to the graph before its batch.
    type Setup = fn(&Store);
    let mut cases: Vec<(&str, Setup, Vec<Op>, &str)> = vec![
        (
            "alternatives",
            |_| {},
            expire(|op| op.alternatives = strings(&["Plain way", "Other zz way"])),
            "ops[0].alternatives[1]",
        ),
        (
            "tags",
            |_| {},
            expire(|op| op.tags = strings(&["zz", "aa"])),
            "ops[0].tags[0]",
        ),
        (
            "anchors",
            |_| {},
            expire(|op| op.anchors = files(&["src/zz.rs", "src/a.rs"])),
            "ops[0].anchors[0]",
        ),
        (
            "alternatives",
            |_| {},
            revised(&|op| op.alternatives = whole(strings(&["Plain way", "zz way"]))),
            "ops[0].alternatives[1]",
        ),
        (
            "tags",
            |_| {},
            revised(&|op| op.tags = whole(strings(&["zz", "aa"]))),
            "ops[0].tags[0]",
        ),
        (
            "tags",
            |_| {},
            revised(&|op| op.tags = added(strings(&["zz", "aa"]))),
            "ops[0].add_tags[0]",
        ),
        (
            "anchors",
            |_| {},
            revised(&|op| {
                op.anchors = ListEdit {
                    whole: Some(files(&["src/zz.rs", "src/a.rs"])),
                    ..ListEdit::keep()
                }
            }),
            "ops[0].anchors[0]",
        ),
        (
            "anchors",
            |_| {},
            revised(&|op| {
                op.anchors = ListEdit {
                    add: Some(files(&["src/zz.rs", "src/a.rs"])),
                    ..ListEdit::keep()
                }
            }),
            "ops[0].add_anchors[0]",
        ),
        (
            "tags",
            |store| tagged_zz(store, "sign"),
            revised(&|op| op.tags = added(strings(&["aa"]))),
            "ops[0].tags",
        ),
        (
            "tags",
            |store| tagged_zz(store, "rotate"),
            vec![Op::Merge(Merge {
                survivor: "audit".into(),
                absorbed: "rotate".into(),
            })],
            "ops[0].tags",
        ),
    ];
    let mut twice = revised(&before_twice);
    twice.extend(revised(&|op| {
        op.alternatives = added(strings(&["Plain", "zz way"]));
    }));
    cases.push(("alternatives", |_| {}, twice, "ops[1].add_alternatives[1]"));
    for (field, setup, ops, path) in cases {
        for level in ["refuse", "warn"] {
            let tmp = TempDir::new().unwrap();
            let (store, _) = under_pattern(&tmp, field, level);
            setup(&store);
            let outcome = super::apply_batch(&store, with_origin(ops.clone(), "row"));
            let named = match outcome {
                Err(Error::Refused(refusal)) => refusal.path(),
                Ok(applied) => applied.warnings[0].field.clone(),
                Err(e) => panic!("{field} {path}: {e}"),
            };
            assert_eq!(named, path, "{field} at {level}");
        }
    }
}

/// A text rule on an element names the argument too: a blank tag added.
#[test]
fn a_text_fault_in_an_element_names_its_argument() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(&tmp, None);
    let mut op = revise("sign");
    op.tags = super::add(&["zz", " "]);

    let err = apply(&store, vec![Op::Revise(op)]).unwrap_err();

    let Error::Refused(refusal) = err else {
        panic!("{err}")
    };
    assert_eq!(
        named(&refusal),
        (
            "invalid_text",
            "ops[0].add_tags[1]".to_owned(),
            "ops[0] (revise `sign`), `add_tags[1]`: is blank".to_owned()
        )
    );
}

/// A table of a `refuse` pattern for `zz` over `choice` and the most
/// patterns more over `alternatives`, and the decision `sign` holding
/// alternatives of nearly a node file's bytes, written by hand. Each op
/// adds `sign` an alternative, so its patterns read all of them: the
/// batch's bytes pass the write bound within a few ops. The first op
/// holds `zz` in its choice when `zz_first`.
fn past_the_write_bytes(tmp: &TempDir) -> (Store, impl Fn(bool) -> Vec<Op>) {
    let zz = "[[quality.patterns]]\npattern = 'zz'\nfields = [\"choice\"]\nlevel = \"refuse\"\n\
              message = \"No zz.\"\n";
    let wide = "[[quality.patterns]]\npattern = 'zz'\nfields = [\"alternatives\"]\n\
                level = \"warn\"\nmessage = \"Wide.\"\n";
    let table = format!("{zz}\n{}", vec![wide; MAX_QUALITY_PATTERNS - 1].join("\n"));
    let store = store_under(tmp, Some(&table));
    let mut sign = store
        .load_state()
        .unwrap()
        .decisions
        .remove("sign")
        .unwrap();
    sign.alternatives = (0..900).map(|n| format!("{n:x<1000}")).collect();
    write_node(&store, &NodeFile::Decision("sign".into()), &sign);
    let bar = store
        .bar(&store.load_state().unwrap().project.quality)
        .unwrap();
    let count = MAX_WRITE_PATTERN_BYTES / bar.pattern_bytes(&sign) + 2;
    let ops = move |zz_first: bool| -> Vec<Op> {
        (0..count)
            .map(|index| {
                let mut op = revise("sign");
                op.alternatives = super::add(&[&format!("Way {index}")]);
                if index == 0 && zz_first {
                    op.choice = Some("Choice zz".into());
                }
                Op::Revise(op)
            })
            .collect()
    };
    (store, ops)
}

/// A batch whose patterns would read past the write bound is refused at
/// the op where the sum does, before the pattern that would pass it reads
/// a byte, after the patterns of the ops before it found nothing to
/// refuse; nothing is written.
#[test]
fn a_batch_past_the_write_bytes_is_refused_at_the_op_where_the_sum_passes() {
    let tmp = TempDir::new().unwrap();
    let (store, ops) = past_the_write_bytes(&tmp);
    let before = super::files(&store);

    let refusal = refused(&store, ops(false));

    let at = refusal.op.as_ref().map_or(0, |op| op.index);
    assert!(at > 0, "{refusal}");
    assert_eq!(refusal.path(), format!("ops[{at}].alternatives"));
    assert_eq!(refusal.rule, Rule::PatternWork);
    assert_eq!(super::files(&store), before);
}

/// A batch is refused at its first op at fault: a `refuse` match in an op
/// before the one where the bytes pass the bound refuses it there.
#[test]
fn a_refusal_before_the_op_past_the_bytes_bound_comes_first() {
    let tmp = TempDir::new().unwrap();
    let (store, ops) = past_the_write_bytes(&tmp);

    let (rule, at, message) = named(&refused(&store, ops(true)));

    assert_eq!(
        (rule, at.as_str()),
        ("pattern", "ops[0].choice"),
        "{message}"
    );
}

/// A pattern's refusal in an op comes before the fault of a later op.
#[test]
fn a_pattern_refusal_comes_before_a_later_op_fault() {
    let tmp = TempDir::new().unwrap();
    let (store, _) = under_pattern(&tmp, "choice", "refuse");
    let mut zz = record("zz", "auth");
    zz.choice = "Choice zz".into();
    let mut ghost = revise("ghost");
    ghost.choice = Some("Another choice".into());

    let (rule, at, message) = named(&refused(&store, vec![Op::Record(zz), Op::Revise(ghost)]));

    assert_eq!(
        (rule, at.as_str()),
        ("pattern", "ops[0].choice"),
        "{message}"
    );
}

/// `copies` copies of a pattern no cache holds over random letters, at
/// `warn` over the origin, which every batch of these tests holds, and
/// `reason`: an op whose caches only the origin grew is judged alone.
fn outgrowing(tmp: &TempDir, copies: usize) -> Store {
    let one = format!(
        "[[quality.patterns]]\npattern = '{OUTGROWS_ITS_CACHE}'\nfields = [\"origin\", \"reason\"]\n\
         level = \"warn\"\nmessage = \"Random.\"\n"
    );
    store_under(tmp, Some(&vec![one; copies].join("\n")))
}

/// A record whose reason is `letters` random letters.
fn random(index: u64, letters: usize) -> Op {
    let mut op = record(&format!("random-{index}"), "auth");
    op.reason = coin_flips(letters, index);
    Op::Record(op)
}

/// The caches of a batch's patterns are the batch's: two records whose
/// reasons each fit a pattern's cache, but not together, are refused at
/// the second as `pattern_gave_up`, naming the pattern whatever its level
/// and the ops before as the cause, and each alone is written.
#[test]
fn a_pattern_whose_cache_fills_over_the_batch_refuses_it_at_that_op() {
    let tmp = TempDir::new().unwrap();
    let store = outgrowing(&tmp, 1);
    let ops = vec![random(0, 2_500), random(1, 2_500)];

    let refusal = refused(&store, ops.clone());

    assert_eq!(refusal.path(), "ops[1].reason");
    let gave_up = QualityFault::GaveUp {
        index: 0,
        alone: false,
    };
    assert_eq!(refusal.rule, Rule::Quality(gave_up));
    assert!(refusal.to_string().contains("split the batch"), "{refusal}");
    for op in ops {
        apply(&store, vec![op]).unwrap();
    }
}

/// A pattern whose cache fills over one op's texts alone is refused there
/// with the remedy for that op, not the batch's.
#[test]
fn a_pattern_whose_cache_fills_over_one_op_names_that_op_alone() {
    let tmp = TempDir::new().unwrap();
    let store = outgrowing(&tmp, 1);

    let refusal = refused(&store, vec![random(0, 12_000)]);

    let gave_up = QualityFault::GaveUp {
        index: 0,
        alone: true,
    };
    assert_eq!(refusal.rule, Rule::Quality(gave_up));
    let message = refusal.to_string();
    assert!(!message.contains("split the batch"), "{message}");
}

/// Patterns whose caches each hold an op's texts but grow past
/// `MAX_PATTERN_CACHE_TOTAL_BYTES` together refuse the write as
/// `pattern_cache_total`, naming the pattern that grew the most, the first
/// of equals, and the one op alone; fewer of them write it.
#[test]
fn caches_that_grow_past_their_total_refuse_the_write() {
    let tmp = TempDir::new().unwrap();
    let store = outgrowing(&tmp, 8);

    let refusal = refused(&store, vec![random(0, 3_000)]);

    let total = QualityFault::CacheTotal {
        largest: 0,
        alone: true,
    };
    assert_eq!(refusal.rule, Rule::Quality(total));
    let tmp = TempDir::new().unwrap();
    apply(&outgrowing(&tmp, 4), vec![random(0, 3_000)]).unwrap();
}

/// A write holds its batch to the bar compiled before the lock while the
/// table it loads under the lock is the same, and compiles the table
/// again when it changed in between.
#[test]
fn a_bar_compiled_before_its_table_changed_is_compiled_again_under_the_lock() {
    let tmp = TempDir::new().unwrap();
    let store = store_under(&tmp, None);
    let before = store.prepare_bar().unwrap();
    with_quality(
        &store,
        "[[quality.patterns]]\npattern = 'zz'\nfields = [\"choice\"]\nlevel = \"refuse\"\n\
         message = \"No zz.\"",
    );

    let ops = expire(|op| op.choice = "Choice zz".into());
    let err = store
        .begin_write()
        .unwrap()
        .apply(super::batch(ops), &before);

    let Err(Error::Refused(refusal)) = err else {
        panic!("{err:?}")
    };
    assert_eq!(refusal.rule.as_str(), "pattern");
}
