//! Helpers the anchor tests share: an anchor from its spelling, and a
//! decision anchored at some.

use crate::store::testing::sample_decision;
use crate::store::{Anchor, Decision, Status, StatusName};

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
