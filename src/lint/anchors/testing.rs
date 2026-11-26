//! Helpers the anchor tests share: the code facts of a tree of files, an
//! anchor from its spelling, and a decision anchored at some.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, Entry, FileFault, FileKind};
use crate::store::testing::sample_decision;
use crate::store::{Anchor, Decision, Status, StatusName};

/// The facts of a tree of `files`, each a path and its text, indexed by
/// the kind its name gives and taken by the walk, and of `other` entries
/// as they are, which only an anchor brought in.
pub(super) fn facts(files: &[(&str, &str)], other: &[(&str, Entry)]) -> CodeFacts {
    let walked: BTreeSet<String> = files.iter().map(|(path, _)| (*path).to_owned()).collect();
    let mut entries: BTreeMap<String, Entry> = files
        .iter()
        .map(|(path, text)| {
            let kind = FileKind::of(path).unwrap();
            let file = Entry::File {
                kind,
                index: crate::code::testing::index(kind, text.as_bytes()).map_err(FileFault::Index),
            };
            ((*path).to_owned(), file)
        })
        .collect();
    entries.extend(
        other
            .iter()
            .map(|(path, entry)| ((*path).to_owned(), entry.clone())),
    );
    CodeFacts::new(entries, walked)
}

pub(super) fn anchor(spelled: &str) -> Anchor {
    let (file, symbol) = match spelled.split_once("::") {
        Some((file, symbol)) => (file, Some(symbol.to_owned())),
        None => (spelled, None),
    };
    Anchor::new(file.to_owned(), symbol).unwrap()
}

/// A decision anchored at `anchors`, accepted or proposed.
pub(super) fn decision(anchors: &[&str], status: StatusName) -> Decision {
    let mut decision = sample_decision("project");
    decision.anchors = anchors.iter().map(|spelled| anchor(spelled)).collect();
    decision.status = match status {
        StatusName::Accepted => Status::Accepted { via: None },
        StatusName::Proposed => Status::Proposed {
            accepted_text: None,
        },
    };
    decision
}
