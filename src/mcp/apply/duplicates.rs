//! The likely duplicates `apply` warns of: for each decision whose choice
//! or reason the batch wrote, its nearest decisions in the graph the batch
//! left, at the threshold of `lint::duplicates` or above. A warning, never
//! a refusal: the batch is written before they are looked for.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::lint::duplicates::Similarity;
use crate::mcp::spec::output::{Items, Nested, Str};
use crate::mcp::spec::{Output, Shown, to_value};
use crate::mcp::truncate::json_len;
use crate::store::{Applied, Done, Field, OpKind, ProjectState};

use super::result::Answer;

/// The likely duplicates listed per decision, best first.
pub(super) const NEAREST: usize = 3;

/// Bytes of `,"duplicates":[]`, which the first listed warning brings.
const LIST_FRAME: usize = 16;

/// A decision the batch wrote the text of, and its likely duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Duplicates {
    /// Its name after the batch.
    pub(crate) decision: String,
    /// At most [`NEAREST`], best first.
    pub(crate) nearest: Vec<Nearest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Nearest {
    pub(crate) decision: String,
    /// The cosine to six decimals.
    pub(crate) score: String,
}

impl Output for Duplicates {
    fn declare(fields: &mut Shown<'_, Self>) {
        let decision = "The decision the batch recorded or whose choice or reason it revised, \
                        by its name after the batch.";
        fields.required("decision", decision, &Str, |warned| &warned.decision);
        let nearest = Items {
            item: Nested::<Nearest>::new(),
        };
        let about = "Its likely duplicates, best first, at most three.";
        fields.required("nearest", about, &nearest, |warned| &warned.nearest);
    }
}

impl Output for Nearest {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("decision", "The other decision.", &Str, |nearest| {
            &nearest.decision
        });
        let score = "The TF-IDF cosine of the two choices and reasons, to six decimals.";
        fields.required("score", score, &Str, |nearest| &nearest.score);
    }
}

/// `applied` with the likely duplicates of each decision whose text it
/// wrote, in `state`, the graph it left: as many as the result budget
/// holds, in the order of the ops, the rest counted.
pub(super) fn answer(applied: Applied, state: &ProjectState) -> Answer {
    let written = text_written(&applied.ops);
    if written.is_empty() {
        return fit(applied, Vec::new());
    }
    let names: Vec<&str> = written.iter().map(String::as_str).collect();
    let nearest = Similarity::of(state).nearest(&names, NEAREST);
    let warned = names
        .into_iter()
        .zip(nearest)
        .filter(|(_, nearest)| !nearest.is_empty())
        .map(|(decision, nearest)| Duplicates {
            decision: decision.to_owned(),
            nearest: nearest
                .into_iter()
                .map(|neighbour| Nearest {
                    decision: neighbour.name.to_owned(),
                    score: neighbour.score.to_string(),
                })
                .collect(),
        })
        .collect();
    fit(applied, warned)
}

/// The decisions whose choice or reason `ops` wrote, by their names after
/// the batch, in the order of the ops that first wrote them: a rename
/// carries one, and a remove, or a merge that absorbs it, drops it.
pub(super) fn text_written(ops: &[Done]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let text = |field: &Field| matches!(field, Field::Choice | Field::Reason);
    for done in ops {
        let name = &done.name;
        match done.op {
            OpKind::Record => names.push(name.to_owned()),
            OpKind::Revise if done.changed.iter().any(text) && !names.contains(name) => {
                names.push(name.to_owned());
            }
            OpKind::Rename => {
                let to = done.to.as_ref();
                if let (Some(held), Some(to)) = (names.iter_mut().find(|held| *held == name), to) {
                    to.clone_into(held);
                }
            }
            OpKind::Merge => names.retain(|held| Some(held) != done.absorbed.as_ref()),
            OpKind::Remove => names.retain(|held| held != name),
            // A revise of other fields, and every component op, leaves each
            // decision's text and name as they were.
            OpKind::Revise
            | OpKind::AddComponent
            | OpKind::ReviseComponent
            | OpKind::RenameComponent
            | OpKind::RemoveComponent => {}
        }
    }
    names
}

/// The answer of `applied` listing the leading warnings of `warned` that
/// fit the result budget beside it, and counting the rest.
pub(super) fn fit(applied: Applied, warned: Vec<Duplicates>) -> Answer {
    let mut answer = Answer {
        applied,
        duplicates: Vec::with_capacity(warned.len()),
        more_duplicates: warned.len(),
    };
    // Measured with every warning counted, the most the count can take.
    let taken = json_len(&to_value(&answer)).saturating_add(LIST_FRAME);
    let mut room = MAX_TOOL_RESULT_BYTES.saturating_sub(taken);
    for duplicates in warned {
        // The warning and the comma before it.
        let bytes = json_len(&to_value(&duplicates)).saturating_add(1);
        if bytes > room {
            break;
        }
        room -= bytes;
        answer.duplicates.push(duplicates);
        answer.more_duplicates -= 1;
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::limits::{MAX_BATCH_OPS, MAX_NAME_BYTES};

    fn done(op: OpKind, name: &str) -> Done {
        Done {
            op,
            name: name.into(),
            to: None,
            absorbed: None,
            repoint_to: None,
            changed: Vec::new(),
            status: None,
            rewrote: 0,
            more_warnings: 0,
        }
    }

    fn revise(name: &str, changed: &[Field]) -> Done {
        Done {
            changed: changed.to_vec(),
            ..done(OpKind::Revise, name)
        }
    }

    /// A record and a revise of the choice or the reason write a text; a
    /// revise of anything else does not; a rename carries the name and a
    /// remove or an absorbing merge drops it.
    #[test]
    fn the_texts_written_follow_the_batch_to_its_last_names() {
        let ops = [
            done(OpKind::Record, "new"),
            revise("old", &[Field::Anchors, Field::Tags]),
            revise("reworded", &[Field::Reason]),
            revise("new", &[Field::Choice]),
            Done {
                to: Some("renamed".into()),
                ..done(OpKind::Rename, "new")
            },
            done(OpKind::Record, "gone"),
            done(OpKind::Remove, "gone"),
            revise("folded", &[Field::Choice]),
            Done {
                absorbed: Some("folded".into()),
                ..done(OpKind::Merge, "reworded")
            },
            done(OpKind::RenameComponent, "renamed"),
        ];

        assert_eq!(text_written(&ops), ["renamed", "reworded"]);
    }

    /// The warnings the budget holds are listed in order and the rest
    /// counted, and the answer fits.
    #[test]
    fn the_warnings_fill_the_room_the_batch_leaves_and_count_the_rest() {
        let name = |index: usize| format!("{index:n>width$}", width = MAX_NAME_BYTES);
        let warned: Vec<Duplicates> = (0..MAX_BATCH_OPS)
            .map(|index| Duplicates {
                decision: name(index),
                nearest: (0..NEAREST)
                    .map(|_| Nearest {
                        decision: name(index),
                        score: "0.500000".into(),
                    })
                    .collect(),
            })
            .collect();
        let applied = Applied {
            ops: vec![done(OpKind::Record, "one")],
            warnings: Vec::new(),
            more_origin_warnings: 0,
            patterns: Vec::new(),
            written: 1,
            removed: 0,
        };

        let answer = fit(applied, warned.clone());

        let listed = answer.duplicates.len();
        assert!((20..MAX_BATCH_OPS).contains(&listed), "{listed}");
        assert_eq!(answer.duplicates, warned[..listed]);
        assert_eq!(answer.more_duplicates, MAX_BATCH_OPS - listed);
        let bytes = json_len(&to_value(&answer));
        assert!(bytes <= MAX_TOOL_RESULT_BYTES, "{bytes}");
        assert!(bytes + 700 > MAX_TOOL_RESULT_BYTES, "{bytes}");
    }
}
