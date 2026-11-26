//! The `apply` tool: its spec, its arguments, and the handler that reads
//! what the write needs holding no lock, then hands the batch to the
//! store's one write.

use crate::failpoint::{self, Site};
use crate::sources::{self, Seen};
use crate::store::limits::MAX_TRAILER_BYTES;
use crate::store::{Batch, Op, PreparedBar, RepairAnchors, Store, Writer};
use crate::tools::spec::kinds::Text;
use crate::tools::spec::tool::{Access, Destructive, Outcome, ToolSpec};
use crate::tools::spec::{Args, Fields};

use super::duplicates;
use super::ops::OPS;
use super::repairs::{self, Unrepaired};
use super::result::{self, Answer};

pub(crate) static APPLY: ToolSpec<ApplyArgs, Answer> = ToolSpec {
    name: "apply",
    title: "Apply",
    description: DESCRIPTION,
    destructive: Destructive::Yes,
    access: Access::Write {
        read: prepare,
        write: apply,
    },
};

const DESCRIPTION: &str = "The one write of the decision graph: a batch of ops, \
applied in order and committed together, or refused whole with nothing written. \
Each op is checked against the graph the earlier ops left, so add a component \
before its decisions and a decision before those that depend on it. A refusal \
names the first op at fault (`op`), the argument (`field`) and the `rule`.\n\
Ops (`op` tag): record a decision (always `proposed`; carries `origin`); revise \
any of its fields, a list whole or by `add_*`/`remove_*` (never both for one list); \
rename (rewrites every edge to it); merge `absorbed` into `survivor` (its edges, \
anchors and tags join the survivor, its text is dropped); remove (refused while \
decisions depend on it, unless `repoint_to` names their new target); \
add_component, revise_component, rename_component, remove_component (refused while \
in use); repair_anchors (stale anchors replaced: the `repairs` given, or with none \
every repair `trurlic check` shows but one the bar refuses or cannot judge, in every \
decision ever accepted that holds the stale anchor; the result lists them and the \
stale anchors left).\n\
Refused too: a revise that changes nothing, an edge to a missing node or to itself, \
an edge that closes a depends_on cycle. Revising the choice, reason or alternatives \
of an accepted decision proposes the new text and keeps the accepted one; revising \
back accepts it again. A repair leaves the status alone.\n\
Quality: each field a record, revise, merge or repair changes meets the bar: built \
in, a one-line choice of at most 200 bytes, a reason of real text, an alternative \
and an anchor at least, no hidden character in any text; then `[quality]` in \
project.toml. A `warn` pattern and a likely duplicate write the batch and warn in \
the result.\n\
The result names each op's node and what it changed, never the texts.";

/// The arguments of `apply`: what a batch file holds too, each op with no
/// `repairs` given the map the tree shows before the write, the code each
/// of those repairs named then, the stale anchors that read left without a
/// repair, and the bar compiled before the write.
#[derive(Debug)]
pub(crate) struct ApplyArgs {
    ops: Vec<Op>,
    origin: Option<String>,
    seen: Seen,
    unrepaired: Unrepaired,
    bar: PreparedBar,
}

impl Args for ApplyArgs {
    fn declare(fields: &mut Fields<'_, '_>) -> Self {
        Self {
            ops: fields.required("ops", "The changes, applied in order.", &OPS),
            origin: fields.optional(
                "origin",
                "The task or plan row behind the batch: stored on each decision it records \
                 and named in the graph's history.",
                &Text {
                    max_bytes: MAX_TRAILER_BYTES,
                },
            ),
            seen: Seen::default(),
            unrepaired: Unrepaired::new(),
            bar: PreparedBar::default(),
        }
    }
}

/// Read and compile, holding no lock, what the write needs: the bar of the
/// project's `[quality]`, and the repairs an op with no `repairs` writes,
/// the tree walk and git run before the write lock is taken.
fn prepare(store: &Store, mut arguments: ApplyArgs) -> Outcome<ApplyArgs> {
    arguments.bar = store.prepare_bar()?;
    (arguments.seen, arguments.unrepaired) = repairs::fill(store, &mut arguments.ops)?;
    failpoint::hit(Site::RepairsRead);
    Ok(arguments)
}

fn apply(mut writer: Writer<'_>, arguments: ApplyArgs) -> Outcome<Answer> {
    let ApplyArgs {
        mut ops,
        origin,
        seen,
        mut unrepaired,
        bar,
    } = arguments;
    // The tree may have changed since the repairs were read: each one is
    // held again under the lock to the code it named then.
    for op in &mut ops {
        if let Op::RepairAnchors(RepairAnchors::Every(map)) = op {
            unrepaired.extend(sources::still_holding(writer.store(), map, &seen));
        }
    }
    let batch = Batch {
        ops,
        origin,
        // No call names the agent's session: the arguments hold none, and
        // what a surface holds of its caller names at most the program.
        session: None,
    };
    let applied = writer.apply(batch, &bar)?;
    let left = repairs::left(writer.state(), unrepaired, &applied.left);
    // The warnings read the graph the write left, the lock released.
    let state = writer.into_state();
    let warned = duplicates::warned(&applied.ops, &state);
    Ok(result::fit(applied, warned, left))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::limits::MAX_BATCH_OPS;
    use crate::tools::spec::{At, parse};

    #[test]
    fn a_batch_over_its_limit_is_refused_by_the_spec() {
        let one = json!({ "op": "remove", "name": "a" });
        let too_many = json!({ "ops": vec![one; MAX_BATCH_OPS + 1] });
        let refused = parse::<ApplyArgs>(too_many.as_object().unwrap(), At::Root).unwrap_err();
        assert_eq!(refused.field, "ops");
    }
}
