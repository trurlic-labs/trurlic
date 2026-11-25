//! What judging the patterns costs, charged before they run: for each
//! pattern, the bytes of each text it reads, one more for each search,
//! times the states of its Thompson NFA, which its slowest engine walks per
//! byte. A search's time is linear in this work whatever the pattern, so
//! one bound on it (`MAX_PATTERN_WORK`) bounds the time of a report and of
//! a write.

use super::Bar;
use crate::store::quality::DecisionField;
use crate::store::schema::Decision;

impl Bar {
    /// The work of each pattern, in their order, when a field gives
    /// `bytes` to read: the bytes of the fields it reads times its states.
    fn costs(&self, bytes: impl Fn(DecisionField) -> usize) -> Vec<usize> {
        let cost = |pattern: &super::Compiled| {
            let read = pattern.fields.iter().map(|&field| bytes(field));
            read.fold(0, usize::saturating_add)
                .saturating_mul(pattern.states)
        };
        self.patterns.iter().map(cost).collect()
    }

    /// The work of the patterns over `decision` as stored, each its fields,
    /// the decision's origin among them: what a report pays to audit it.
    pub(crate) fn pattern_work(&self, decision: &Decision) -> usize {
        let costs = self.costs(|field| field_bytes(decision, field));
        costs.into_iter().fold(0, usize::saturating_add)
    }

    /// The work of each pattern over the fields of `held` in `decision`,
    /// as [`Bar::judge_patterns`] reads them: what a write pays to judge
    /// the decision an op writes. The origin is the batch's, charged once.
    pub(crate) fn judge_work(&self, decision: &Decision, held: &[DecisionField]) -> Vec<usize> {
        self.costs(|field| {
            let read = field != DecisionField::Origin && held.contains(&field);
            if read {
                field_bytes(decision, field)
            } else {
                0
            }
        })
    }

    /// The work of each pattern over a batch's `origin`, as
    /// [`Bar::judge_origin`] reads it.
    pub(crate) fn origin_work(&self, origin: Option<&str>) -> Vec<usize> {
        let bytes = origin.map_or(0, |origin| origin.len().saturating_add(1));
        self.costs(|field| match field {
            DecisionField::Origin => bytes,
            DecisionField::Choice
            | DecisionField::Reason
            | DecisionField::Alternatives
            | DecisionField::Tags
            | DecisionField::Anchors => 0,
        })
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
