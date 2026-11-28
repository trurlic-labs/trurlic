use std::path::Path;

use serde_json::{Map, Value};

use crate::tools::{FRAME, MAP, SEARCH, ToolError};

use super::json;

/// What `trurlic map` prints, by the argument or flag that names it, with
/// where its page starts.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MapUnit<'a> {
    /// The level below the node at an address, the crates for `None`, from
    /// the child at an address.
    Level {
        node: Option<&'a str>,
        from: Option<&'a str>,
    },
    /// The frame of the file at an address, from a position.
    Frame { file: &'a str, from: Option<usize> },
    /// The hits of a query, from a position.
    Search { query: &'a str, from: Option<usize> },
}

/// `trurlic map --json`: the `map` tool of `unit`, printed as the tool's
/// JSON, the one form a reader of the code tree has. It writes nothing but
/// the code index's cache, and runs no git.
pub(crate) fn map(cwd: &Path, unit: MapUnit<'_>) -> Result<(), ToolError> {
    match unit {
        MapUnit::Level { node, from } => {
            let node = node.map(Value::from);
            json(MAP.run(
                cwd,
                &arguments([("node", node), ("from", from.map(Value::from))]),
            ))
        }
        MapUnit::Frame { file, from } => {
            let file = Some(Value::from(file));
            json(FRAME.run(
                cwd,
                &arguments([("file", file), ("from_position", from.map(Value::from))]),
            ))
        }
        MapUnit::Search { query, from } => {
            let query = Some(Value::from(query));
            json(SEARCH.run(
                cwd,
                &arguments([("query", query), ("from_position", from.map(Value::from))]),
            ))
        }
    }
}

/// The arguments a call takes of `fields`, each key with its value, one
/// with none left out.
fn arguments<const N: usize>(fields: [(&str, Option<Value>); N]) -> Map<String, Value> {
    let given = fields.into_iter();
    given
        .filter_map(|(key, value)| Some((key.to_owned(), value?)))
        .collect()
}
