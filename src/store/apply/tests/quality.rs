//! The quality bar through `Writer::apply`, the one write of every
//! surface: each rule refusing with its name, field and measure, each
//! `[quality]` key moving its limit, and a pattern reading only its fields
//! at its level.

use std::fs;

use tempfile::TempDir;

use super::{anchor, apply, batch, graph, record, refused, revise};
use crate::Error;
use crate::store::apply::{Applied, Merge, Op, Record, Refusal};
use crate::store::canonical::canonical;
use crate::store::schema::Anchor;
use crate::store::state::NodeFile;
use crate::store::testing::{sample_decision, write_node};
use crate::store::{ListEdit, Store, Warning, WarningKind};

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

/// The rule name, the path and the message of `refusal`.
fn named(refusal: &Refusal) -> (&'static str, String, String) {
    (refusal.rule.as_str(), refusal.path(), refusal.to_string())
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

    let err = store
        .begin_write()
        .unwrap()
        .apply(without(expire(|_| {})))
        .unwrap_err();

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
    let writer = store
        .begin_write()
        .unwrap()
        .apply(without(vec![Op::Revise(tags)]));
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
        let err = store
            .begin_write()
            .unwrap()
            .apply(with_origin(ops, "zz row"))
            .unwrap_err();
        let Error::Refused(refusal) = err else {
            panic!("{err}")
        };
        let (rule, refused_at, message) = named(&refusal);
        assert_eq!((rule, refused_at.as_str()), ("pattern", path));
        let said = format!("matches `[quality] patterns[0]` at byte {at} (`zz`): No zz.");
        assert!(message.ends_with(&said), "{message}");

        let tmp = TempDir::new().unwrap();
        let (store, ops) = under_pattern(&tmp, field, "warn");
        let applied: Applied = store
            .begin_write()
            .unwrap()
            .apply(with_origin(ops, "zz row"))
            .unwrap();
        let [warning] = applied.warnings.as_slice() else {
            panic!("{field}: {:?}", applied.warnings)
        };
        assert_eq!(
            (warning.field.as_deref(), warning.kind),
            (Some(path), WarningKind::Pattern)
        );
        assert_eq!(warning.op.is_some(), field != "origin");
        assert!(warning.message.ends_with(&said), "{}", warning.message);

        let tmp = TempDir::new().unwrap();
        let (store, ops) = under_pattern(&tmp, field, "refuse");
        let clean = ops_without_zz_in(ops, field);
        let origin = if field == "origin" { "row 7" } else { "zz row" };
        store
            .begin_write()
            .unwrap()
            .apply(with_origin(clean, origin))
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
/// naming `project.toml`, the pattern's index and regex's message.
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

    let warnings: Vec<(&str, Option<&str>)> = applied
        .warnings
        .iter()
        .map(|warning: &Warning| (warning.message.as_str(), warning.field.as_deref()))
        .collect();
    assert_eq!(
        warnings,
        [
            (
                "ops[0] (record `expire`), `choice`: matches `[quality] patterns[0]` at byte 7 \
                 (`zz`): No zz.",
                Some("ops[0].choice")
            ),
            (
                "ops[1] (record `second`), `reason`: matches `[quality] patterns[0]` at byte 9 \
                 (`zz`): No zz.",
                Some("ops[1].reason")
            ),
        ]
    );
}
