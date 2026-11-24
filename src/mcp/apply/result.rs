//! What `apply` returns, declared once for its `outputSchema` and its
//! JSON: each op's node and what it changed, the warnings, the repairs
//! written and the stale anchors left, the likely duplicates, and the
//! files written. The lists a batch cannot bound fill the room the rest
//! leaves ([`fit`]). The tests below hold the longest result and the
//! longest refusal of a batch to the result budget.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::mcp::spec::output::{Count, Items, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown, to_value};
use crate::mcp::truncate::json_len;
use crate::store::{Applied, Done, Field, OpKind, PatternNote, StatusName, Warning, WarningKind};

use super::duplicates::Duplicates;
use super::repairs::{self, Left, Repaired};

/// What `apply` returns: what the store's write did, the repairs it wrote
/// and the stale anchors it left, and the likely duplicates of the
/// decisions whose text it wrote, each list as many as the budget holds.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) applied: Applied,
    pub(crate) repaired: Vec<Repaired>,
    /// The repairs written and not listed.
    pub(crate) more_repaired: usize,
    pub(crate) left: Vec<Left>,
    /// The stale anchors left and not listed.
    pub(crate) more_left: usize,
    /// The warnings the result budget holds, in the order of the ops.
    pub(crate) duplicates: Vec<Duplicates>,
    /// The decisions with likely duplicates not listed.
    pub(crate) more_duplicates: usize,
}

impl Output for Answer {
    fn declare(fields: &mut Shown<'_, Self>) {
        let ops = Items {
            item: Nested::<Done>::new(),
        };
        fields.required("ops", "What each op did, in order.", &ops, |answer| {
            &answer.applied.ops
        });
        declare_warnings(fields);
        repairs::declare(fields);
        declare_duplicates(fields);
        fields.required(
            "written",
            "Files written, a rename's new file included.",
            &Count,
            |answer| &answer.applied.written,
        );
        fields.required(
            "removed",
            "Files removed, a rename's old file included.",
            &Count,
            |answer| &answer.applied.removed,
        );
    }
}

fn declare_warnings(fields: &mut Shown<'_, Answer>) {
    let warnings = Items {
        item: Nested::<Warning>::new(),
    };
    let about = "What the batch wrote but a `warn` pattern of `[quality]` warns of: each \
                 pattern's first match per op, and the first of the origin's and of each \
                 op's, in the order of the ops and the patterns; an op's `more_warnings` \
                 counts the rest.";
    fields.optional("warnings", about, &warnings, |answer| {
        let warnings = &answer.applied.warnings;
        (!warnings.is_empty()).then_some(warnings)
    });
    let more = "The origin's warnings not listed.";
    fields.optional("more_origin_warnings", more, &Count, |answer| {
        let more = &answer.applied.more_origin_warnings;
        (*more > 0).then_some(more)
    });
    let patterns = Items {
        item: Nested::<PatternNote>::new(),
    };
    let about = "The message of each pattern a warning names, once.";
    fields.optional("patterns", about, &patterns, |answer| {
        let patterns = &answer.applied.patterns;
        (!patterns.is_empty()).then_some(patterns)
    });
}

fn declare_duplicates(fields: &mut Shown<'_, Answer>) {
    let duplicates = Items {
        item: Nested::<Duplicates>::new(),
    };
    let about = "Each decision the batch recorded or whose choice or reason it revised that \
                 has likely duplicates, decisions whose choice and reason have a TF-IDF \
                 cosine of 0.5 or more with its own, in the order of the ops: a warning, \
                 never a refusal.";
    fields.optional("duplicates", about, &duplicates, |answer| {
        (!answer.duplicates.is_empty()).then_some(&answer.duplicates)
    });
    let more = "Decisions with likely duplicates not listed in `duplicates`. `status` ranks the \
                pairs under `lint`: the section lists the best pairs; the ones left out rank at \
                or below the last listed, and come into view as the listed ones are settled \
                (merged, or revised apart).";
    fields.optional("more_duplicates", more, &Count, |answer| {
        (answer.more_duplicates > 0).then_some(&answer.more_duplicates)
    });
}

/// The answer of `applied` listing the leading items of `repaired`,
/// `left` and `warned` that fit the result budget beside it, in that
/// order, each list cut where its next item does not fit and the rest
/// counted.
pub(super) fn fit(mut applied: Applied, warned: Vec<Duplicates>, left: Vec<Left>) -> Answer {
    let repaired: Vec<Repaired> = std::mem::take(&mut applied.repaired)
        .into_iter()
        .map(Repaired::from)
        .collect();
    let mut answer = Answer {
        applied,
        repaired: Vec::new(),
        more_repaired: repaired.len(),
        left: Vec::new(),
        more_left: left.len(),
        duplicates: Vec::new(),
        more_duplicates: warned.len(),
    };
    // Measured with every item counted, the most the counts can take, and
    // the frames of the three lists.
    let taken = json_len(&to_value(&answer)).saturating_add(3 * LIST_FRAME);
    let mut room = MAX_TOOL_RESULT_BYTES.saturating_sub(taken);
    answer.repaired = fitting(repaired, &mut room);
    answer.more_repaired -= answer.repaired.len();
    answer.left = fitting(left, &mut room);
    answer.more_left -= answer.left.len();
    answer.duplicates = fitting(warned, &mut room);
    answer.more_duplicates -= answer.duplicates.len();
    answer
}

/// The bytes of a list's key and brackets beyond its items, at the
/// longest key: `,"duplicates":[]`.
const LIST_FRAME: usize = 16;

/// The leading `items` whose JSON fits `room`, each with the comma before
/// it, taken from `room`.
fn fitting<T: Output>(items: Vec<T>, room: &mut usize) -> Vec<T> {
    let mut kept = Vec::new();
    for item in items {
        let bytes = json_len(&to_value(&item)).saturating_add(1);
        if bytes > *room {
            break;
        }
        *room -= bytes;
        kept.push(item);
    }
    kept
}

const OP_KIND: Tag<OpKind> = Tag {
    all: &OpKind::ALL,
    name: OpKind::as_str,
};
const WARNING_KIND: Tag<WarningKind> = Tag {
    all: &WarningKind::ALL,
    name: WarningKind::as_str,
};

/// A warning: the argument and the rule, as a refusal names them, and the
/// pattern by its index, whose message `patterns` carries. The op is the
/// one `ops` names at the index the argument path gives.
impl Output for Warning {
    fn declare(fields: &mut Shown<'_, Self>) {
        let field = "The argument, as `ops[2].reason` or `origin`.";
        fields.required("field", field, &Str, |warning| &warning.field);
        fields.required("rule", "The rule's name.", &WARNING_KIND, |warning| {
            &warning.kind
        });
        let pattern = "The pattern's index in `[quality] patterns`.";
        fields.required("pattern", pattern, &Count, |warning| &warning.pattern);
        let at = "The byte offset of the match in the argument's text.";
        fields.required("at", at, &Count, |warning| &warning.at);
    }
}

impl Output for PatternNote {
    fn declare(fields: &mut Shown<'_, Self>) {
        let index = "The pattern's index in `[quality] patterns`.";
        fields.required("index", index, &Count, |note| &note.index);
        fields.required("message", "What the pattern says.", &Str, |note| {
            &note.message
        });
    }
}

const CHANGED: Items<Tag<Field>> = Items {
    item: Tag {
        all: &Field::CHANGEABLE,
        name: Field::as_str,
    },
};
const STATUS: Tag<StatusName> = Tag {
    all: &StatusName::ALL,
    name: StatusName::as_str,
};

impl Output for Done {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("op", "The op's kind.", &OP_KIND, |done| &done.op);
        let name = "The node it names first; a merge's survivor; the first decision a \
                    repair changed, empty when it changed none.";
        fields.required("name", name, &Str, |done| &done.name);
        fields.optional("to", "A rename's new name.", &Str, |done| {
            done.to.as_deref()
        });
        let absorbed = "A merge's removed decision.";
        fields.optional("absorbed", absorbed, &Str, |done| done.absorbed.as_deref());
        let heir = "Where a remove pointed the edges to its decision.";
        fields.optional("repoint_to", heir, &Str, |done| done.repoint_to.as_deref());
        let changed = "A revise's changed fields.";
        fields.optional("changed", changed, &CHANGED, |done| {
            (!done.changed.is_empty()).then_some(&done.changed)
        });
        let status = "The decision's new status, when the op changed it.";
        fields.optional("status", status, &STATUS, |done| done.status.as_ref());
        let rewrote = "Files of other nodes it rewrote: edges repointed, a component's \
                       decisions and connections renamed, the decisions a repair changed \
                       after its first.";
        fields.optional("rewrote", rewrote, &Count, |done| {
            (done.rewrote > 0).then_some(&done.rewrote)
        });
        let more = "Its warnings not listed in `warnings`.";
        fields.optional("more_warnings", more, &Count, |done| {
            (done.more_warnings > 0).then_some(&done.more_warnings)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::MAX_TOOL_RESULT_BYTES;
    use crate::mcp::apply::duplicates::{NEAREST, Nearest};
    use crate::store::Anchor;
    use crate::store::limits::{
        MAX_ALTERNATIVE_BYTES, MAX_BATCH_OPS, MAX_LIST_ITEMS, MAX_NAME_BYTES, MAX_TEXT_FIELD_BYTES,
    };

    /// The longest result a batch can have fits the result budget whole.
    ///
    /// The arithmetic. An op's entry is longest for a merge or a rename,
    /// two names of `MAX_NAME_BYTES` and a count of 20 digits, with the
    /// count of its warnings not listed: about 350 bytes, 17.5 KB for
    /// fifty. A `warn` pattern warns once per op, so fifty ops can draw
    /// 1,600 warnings, which no result lists beside the ops: each op, and
    /// the batch's origin, lists `MAX_WARNINGS_PER_OP` and counts the
    /// rest. A listed warning names its argument, the rule, the pattern by
    /// its index and the offset, at most about 82 bytes: 4.2 KB for fifty
    /// ops and the origin. The message of each pattern a warning names is
    /// carried once: 32 frames of about 25 bytes and at most
    /// `MAX_WARN_MESSAGES_BYTES` of escaped messages, 2.3 KB. A likely
    /// duplicate's warning names a decision and its three nearest with
    /// their scores, about 700 bytes at `MAX_NAME_BYTES`. Fifty take 35 KB,
    /// which no result holds beside the rest. The repairs written and the
    /// stale anchors left, as many as the graph holds, take more still. So
    /// these lists fill what room is left, in that order, and three counts
    /// of at most 20 digits count the others, about 80 bytes. About 24.3
    /// KB of the 24 KiB, with no room left for one item: the longest batch
    /// counts them all.
    #[test]
    fn the_longest_result_of_a_full_batch_fits_the_budget() {
        use crate::store::limits::{
            MAX_ANCHOR_PATH_BYTES, MAX_ANCHOR_SYMBOL_BYTES, MAX_QUALITY_PATTERNS,
            MAX_WARN_MESSAGES_BYTES, MAX_WARNINGS_PER_OP,
        };

        let name = || "n".repeat(MAX_NAME_BYTES);
        let unlisted = MAX_QUALITY_PATTERNS - MAX_WARNINGS_PER_OP;
        let longest = OpKind::ALL
            .into_iter()
            .map(|op| {
                let mut done = Done {
                    op,
                    name: name(),
                    to: None,
                    absorbed: None,
                    repoint_to: None,
                    changed: Vec::new(),
                    status: None,
                    rewrote: usize::MAX,
                    more_warnings: unlisted,
                };
                match op {
                    OpKind::Rename | OpKind::RenameComponent => done.to = Some(name()),
                    OpKind::Merge => done.absorbed = Some(name()),
                    OpKind::Remove => done.repoint_to = Some(name()),
                    OpKind::Revise => {
                        done.changed = Field::DECISION_CHANGEABLE.to_vec();
                        done.status = Some(StatusName::Proposed);
                    }
                    OpKind::ReviseComponent => {
                        done.changed = Field::COMPONENT_CHANGEABLE.to_vec();
                    }
                    OpKind::RepairAnchors => done.changed = vec![Field::Anchors],
                    OpKind::Record | OpKind::AddComponent | OpKind::RemoveComponent => {}
                }
                done
            })
            .max_by_key(|done| to_value(done).to_string().len())
            .unwrap();
        let warning = |op: Option<usize>| Warning {
            field: match op {
                Some(index) => format!("ops[{index}].add_alternatives[{}]", MAX_LIST_ITEMS - 1),
                None => "origin".into(),
            },
            op,
            kind: WarningKind::Pattern,
            pattern: MAX_QUALITY_PATTERNS - 1,
            at: MAX_TEXT_FIELD_BYTES,
        };
        let listed = (0..MAX_BATCH_OPS).map(Some).chain([None]);
        // Every `"` is written escaped, two bytes, as the bound counts it.
        let quotes = MAX_WARN_MESSAGES_BYTES / 2 / MAX_QUALITY_PATTERNS;
        let applied = Applied {
            ops: vec![longest; MAX_BATCH_OPS],
            warnings: listed
                .flat_map(|op| vec![warning(op); MAX_WARNINGS_PER_OP])
                .collect(),
            more_origin_warnings: unlisted,
            patterns: (0..MAX_QUALITY_PATTERNS)
                .map(|index| PatternNote {
                    index,
                    message: "\"".repeat(quotes),
                })
                .collect(),
            written: usize::MAX,
            removed: usize::MAX,
            repaired: Vec::new(),
        };
        let anchor = || {
            let file = "f".repeat(MAX_ANCHOR_PATH_BYTES);
            Anchor::new(file, Some("s".repeat(MAX_ANCHOR_SYMBOL_BYTES))).unwrap()
        };
        let written = crate::store::Repaired {
            decision: name(),
            from: anchor(),
            to: anchor(),
        };
        let left = Left {
            decision: name(),
            anchor: anchor().to_string(),
            cause: "its file defines nothing its symbol names".into(),
        };
        let nearest = Nearest {
            decision: name(),
            score: "1.000000".into(),
        };
        let warned = Duplicates {
            decision: name(),
            nearest: vec![nearest; NEAREST],
        };
        // An op asking for every repair repairs every decision that holds
        // a stale anchor, as many as the graph holds.
        let repairs = 10_000;
        let applied = Applied {
            repaired: vec![written; repairs],
            ..applied
        };
        let answer = fit(applied, vec![warned; MAX_BATCH_OPS], vec![left; repairs]);

        let bytes = to_value(&answer).to_string().len();
        assert!(bytes <= MAX_TOOL_RESULT_BYTES, "{bytes} bytes");
        assert_eq!(answer.more_repaired, repairs);
        assert_eq!(answer.more_left, repairs);
        assert_eq!(answer.more_duplicates, MAX_BATCH_OPS);
    }

    /// The longest refusal of every rule whose message a batch bounds fits
    /// the budget uncut: names of `MAX_NAME_BYTES`, the longest argument
    /// path, an element and a quoted match at their limits. An issue's
    /// message lists what `check` names, a cycle every member, and the
    /// refusal is cut like any long payload.
    #[test]
    fn the_longest_refusal_fits_the_budget() {
        use crate::mcp::result::ToolError;
        use crate::store::limits::{MAX_PATTERN_MESSAGE_BYTES, MAX_QUOTED_MATCH_BYTES};
        use crate::store::{
            NodeFile, NodeKind, OpAt, PatternMatch, QualityFault, Refusal, Rule, TextFault,
            TextField,
        };

        let name = || "n".repeat(MAX_NAME_BYTES);
        let names = || vec![name(); 1_000];
        let rules = [
            Rule::NameTaken {
                name: name(),
                holder: NodeKind::Component,
            },
            Rule::Dependents { names: names() },
            Rule::InUse {
                decisions: names(),
                components: names(),
            },
            Rule::Cycle { members: names() },
            Rule::NotHeld {
                element: "\"".repeat(MAX_ALTERNATIVE_BYTES),
            },
            Rule::Text {
                field: TextField::Reason,
                fault: TextFault::ToolMarkup {
                    found: "</function_calls".into(),
                    at: MAX_TEXT_FIELD_BYTES,
                },
            },
            Rule::Quality(QualityFault::Pattern(PatternMatch {
                index: 31,
                at: MAX_TEXT_FIELD_BYTES,
                found: format!("{}...", "\"".repeat(MAX_QUOTED_MATCH_BYTES)),
                message: "\"".repeat(MAX_PATTERN_MESSAGE_BYTES),
            })),
            Rule::FileTooLarge {
                file: NodeFile::Decision(name()),
                bytes: usize::MAX,
                max: usize::MAX,
            },
            Rule::PatternWork {
                work: usize::MAX,
                bound: usize::MAX,
                costliest: Some(usize::MAX),
            },
        ];
        for rule in rules {
            let refusal = Refusal {
                op: Some(OpAt {
                    index: MAX_BATCH_OPS - 1,
                    kind: OpKind::RenameComponent,
                    target: name(),
                }),
                field: Some(format!("add_alternatives[{}]", MAX_LIST_ITEMS - 1)),
                rule,
            };
            let error = ToolError::from(refusal);
            let bytes = serde_json::json!({ "error": error }).to_string().len();
            assert!(bytes <= MAX_TOOL_RESULT_BYTES, "{bytes} bytes");
        }
    }
}
