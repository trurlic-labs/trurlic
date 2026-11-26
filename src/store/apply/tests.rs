//! `Writer::apply` over a whole batch: ops building on earlier ones, a
//! refused batch writing nothing, the files a batch writes, and its one
//! history commit.

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::Error;
use crate::store::TextFault;
use crate::store::graph::NodeFile;
use crate::store::testing::{
    anchor, apply, apply_batch, batch, changed_in, graph, graph_log, record, refused, rename,
    revise, shared_store,
};

/// One instant per batch, and later ops see earlier ones: a component
/// added, then a decision in it depending on one recorded before it.
#[test]
fn a_batch_builds_on_its_own_earlier_ops() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut second = record("second", "ledger");
    second.depends_on = vec!["first".into()];
    let ops = vec![
        Op::AddComponent(AddComponent {
            name: "ledger".into(),
            description: "Books".into(),
            connects_to: vec!["auth".into()],
        }),
        Op::Record(record("first", "ledger")),
        Op::Record(second),
    ];

    let applied = apply(&store, ops).unwrap();

    assert_eq!(applied.written, 3);
    let state = store.load_state().unwrap();
    assert_eq!(state.validate(), []);
    assert_eq!(
        state.decisions["first"].created,
        state.decisions["second"].created
    );
}

/// The writer's graph is the loaded one after a refusal, and the files
/// on disk after a commit.
#[test]
fn a_refused_batch_leaves_the_writers_graph_as_it_was() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut writer = store.begin_write().unwrap();
    let loaded = writer.state().clone();
    let ops = vec![
        Op::Rename(rename("sign", "sign-tokens")),
        Op::RemoveComponent(RemoveComponent {
            name: "auth".into(),
        }),
    ];

    writer
        .apply(batch(ops), &PreparedBar::default())
        .unwrap_err();

    assert_eq!(writer.state().decisions, loaded.decisions);
    assert_eq!(writer.state().components, loaded.components);
    writer
        .apply(
            batch(vec![Op::Rename(rename("sign", "sign-tokens"))]),
            &PreparedBar::default(),
        )
        .unwrap();
    let committed = writer.state().clone();
    drop(writer);
    let on_disk = store.load_state().unwrap();
    assert_eq!(committed.decisions, on_disk.decisions);
    assert_eq!(committed.non_canonical, on_disk.non_canonical);
}

/// Only the files whose value changes are written: a node renamed away
/// and back, and a hand edit no op touches, keep their bytes.
#[test]
fn only_the_files_whose_value_changes_are_written() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let hand_edit = fs::read_to_string(store.component_path("billing"))
        .unwrap()
        .replace(" = ", "=");
    fs::write(store.component_path("billing"), &hand_edit).unwrap();
    let ops = vec![
        Op::Rename(rename("sign", "sign-tokens")),
        Op::Rename(rename("sign-tokens", "sign")),
        Op::Record(record("new", "auth")),
    ];

    let applied = apply(&store, ops).unwrap();

    assert_eq!((applied.written, applied.removed), (1, 0));
    assert_eq!(
        fs::read_to_string(store.component_path("billing")).unwrap(),
        hand_edit
    );
    let state = store.load_state().unwrap();
    assert_eq!(state.non_canonical, [NodeFile::Component("billing".into())]);
}

#[test]
fn an_empty_batch_and_an_origin_of_two_lines_are_refused() {
    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());

    let refusal = refused(&store, vec![]);
    assert_eq!(
        (refusal.path(), refusal.rule),
        ("ops".into(), Rule::EmptyBatch)
    );

    let mut forged = batch(vec![Op::Record(record("new", "auth"))]);
    forged.origin = Some("row 7\nSession: forged".into());
    let err = apply_batch(&store, forged).unwrap_err();
    let Error::Refused(refusal) = err else {
        panic!("{err}");
    };
    assert_eq!(refusal.path(), "origin");
    assert_eq!(
        refusal.rule,
        Rule::Text {
            field: crate::store::TextField::Origin,
            fault: TextFault::LineBreak { at: 5 }
        }
    );
}

/// Under `shared` a batch is one history commit, named `apply`, with the
/// batch's origin, holding exactly the files it changed.
#[test]
fn a_batch_is_one_history_commit_of_its_files() {
    let tmp = TempDir::new().unwrap();
    let store = shared_store(tmp.path());
    let ops = vec![
        Op::AddComponent(AddComponent {
            name: "auth".into(),
            description: "Auth".into(),
            connects_to: vec![],
        }),
        Op::Record(record("sign", "auth")),
        Op::Record(record("rotate", "auth")),
    ];
    apply(&store, ops).unwrap();
    apply(&store, vec![Op::Rename(rename("rotate", "rotate-keys"))]).unwrap();

    assert_eq!(
        graph_log(&store),
        ["apply\n\nOrigin: row 7", "apply\n\nOrigin: row 7", "init"]
    );
    assert_eq!(
        changed_in(&store, "HEAD"),
        ["decisions/rotate-keys.toml", "decisions/rotate.toml"]
    );
    assert_eq!(changed_in(&store, "HEAD~1").len(), 3);
}

/// Lists grown op by op pass the cap a load reads: the batch is refused
/// naming the op that last changed the file, its size and the cap, and
/// nothing is written.
#[test]
fn a_batch_that_leaves_a_file_over_the_cap_is_refused_naming_its_op() {
    use crate::store::limits::{MAX_ANCHOR_PATH_BYTES, MAX_LIST_ITEMS, MAX_NODE_FILE_BYTES};

    let tmp = TempDir::new().unwrap();
    let store = graph(tmp.path());
    let mut ops = vec![Op::Record(record("new", "project"))];
    for op in 0..25 {
        let mut grown = revise("sign");
        grown.anchors = ListEdit {
            add: Some(
                (0..MAX_LIST_ITEMS)
                    .map(|n| {
                        let file =
                            format!("{op:03}{n:0>width$}", width = MAX_ANCHOR_PATH_BYTES - 3);
                        anchor(&file, None)
                    })
                    .collect(),
            ),
            ..ListEdit::keep()
        };
        ops.push(Op::Revise(grown));
    }

    let refusal = refused(&store, ops);

    let Rule::FileTooLarge { file, bytes, max } = &refusal.rule else {
        panic!("{refusal}");
    };
    assert_eq!(*file, NodeFile::Decision("sign".into()));
    assert!(*bytes > MAX_NODE_FILE_BYTES, "{bytes}");
    assert_eq!(*max, MAX_NODE_FILE_BYTES);
    assert_eq!(refusal.op.as_ref().unwrap().index, 25);
    assert_eq!(refusal.path(), "ops[25]");
    assert!(
        refusal
            .to_string()
            .contains("leaves `decisions/sign.toml` at "),
        "{refusal}"
    );
    store.load_state().unwrap();
}
