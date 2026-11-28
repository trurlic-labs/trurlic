use std::path::Path;

use serde_json::{Map, Value};

use crate::tools::{MAP, ToolError};

use super::json;

/// The arguments of `map` that `trurlic map` takes as an argument and a
/// flag.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MapFlags<'a> {
    pub(crate) node: Option<&'a str>,
    pub(crate) from: Option<&'a str>,
}

/// `trurlic map --json`: the `map` tool, printed as the tool's JSON, the
/// one form a reader of the code tree has. It writes nothing but the code
/// index's cache, and runs no git.
pub(crate) fn map(cwd: &Path, flags: MapFlags<'_>) -> Result<(), ToolError> {
    let mut arguments = Map::new();
    for (key, flag) in [("node", flags.node), ("from", flags.from)] {
        if let Some(flag) = flag {
            arguments.insert(key.into(), Value::from(flag));
        }
    }
    json(MAP.run(cwd, &arguments))
}
