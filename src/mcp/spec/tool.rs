//! Tools built on a spec, and the registry a call is looked up in.
//!
//! A tool's [`Access`] states what it runs on, and the call takes that
//! access for it after its arguments parse: a read the graph loaded under
//! the shared lock, a write a [`Writer`] under the exclusive one, which a
//! write may precede with a read of its own that holds no lock. So no
//! write handler locks, and its `readOnlyHint` is the lock it gets. Its
//! result is an [`Output`], which gives both its `outputSchema` and the
//! JSON a call returns.

use serde_json::{Map, Value, json};

use super::{Args, At, Output, input_schema, output_schema, parse, to_value};
use crate::mcp::result::ToolError;
use crate::store::{ProjectState, Store, Writer};

/// A tool's outcome: its result, or why it refused.
pub(crate) type Outcome<O> = Result<O, ToolError>;

/// What a tool's handler runs on.
pub(crate) enum Access<A, O> {
    /// The graph loaded under the shared lock, and the store it came from.
    Read(fn(&Store, &ProjectState, A) -> Outcome<O>),
    /// A writer, `write`: the graph loaded under the exclusive lock, which
    /// the handler may let go of once it has written, for a read of the
    /// graph its write left that needs no lock. Before
    /// it, `read` adds to the arguments what the write needs of the code
    /// tree or the project's git, read holding no lock, so a slow git holds
    /// no other writer and the index's cache can fill. The graph can change
    /// between the two: what `read` adds must hold against whatever graph
    /// `write` loads.
    Write {
        read: fn(&Store, A) -> Outcome<A>,
        write: fn(Writer<'_>, A) -> Outcome<O>,
    },
}

/// Whether a write can remove what it does not replace, MCP's
/// `destructiveHint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destructive {
    No,
    Yes,
}

/// One tool built on a spec: what `tools/list` shows of it and the handler
/// its parsed arguments reach.
pub(crate) struct ToolSpec<A, O> {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub destructive: Destructive,
    pub access: Access<A, O>,
}

impl<A: Args, O: Output> ToolSpec<A, O> {
    /// Parse `arguments`, then run the tool on its access to `store`. An
    /// argument the spec refuses touches no file.
    pub(crate) fn run(&self, store: &Store, arguments: &Map<String, Value>) -> Outcome<O> {
        let arguments = parse::<A>(arguments, At::Root)?;
        match self.access {
            Access::Read(run) => run(store, &store.load_state()?, arguments),
            Access::Write { read, write } => {
                let arguments = read(store, arguments)?;
                write(store.begin_write()?, arguments)
            }
        }
    }
}

/// A [`ToolSpec`] with its types erased, as the registry holds it.
pub(crate) trait SpecTool: Sync {
    fn name(&self) -> &'static str;

    /// The tool's entry in `tools/list`.
    fn definition(&self) -> Value;

    /// [`ToolSpec::run`], its result as JSON.
    fn call(&self, store: &Store, arguments: &Map<String, Value>) -> Outcome<Value>;
}

impl<A: Args, O: Output> SpecTool for ToolSpec<A, O>
where
    Self: Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn definition(&self) -> Value {
        let read_only = matches!(self.access, Access::Read(_));
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "annotations": {
                "title": self.title,
                "readOnlyHint": read_only,
                "destructiveHint": self.destructive == Destructive::Yes,
                "idempotentHint": read_only,
                "openWorldHint": false,
            },
            "inputSchema": input_schema::<A>(),
            "outputSchema": result_schema::<O>(),
        })
    }

    fn call(&self, store: &Store, arguments: &Map<String, Value>) -> Outcome<Value> {
        self.run(store, arguments).map(|result| to_value(&result))
    }
}

/// The `outputSchema` of a result of `O`: its declared fields, and the
/// `truncated` notice a result cut to the budget carries.
fn result_schema<O: Output>() -> Map<String, Value> {
    let mut schema = output_schema::<O>();
    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        properties.insert(
            "truncated".into(),
            json!({
                "type": "array",
                "items": { "type": "object" },
                "description": "Present only when the result was cut to fit the result \
                    budget: each cut, by JSON pointer, with what it omitted.",
            }),
        );
    }
    schema
}

/// Every tool built on a spec. A call is looked up here first, and
/// `tools/list` lists these before the tools that predate specs.
pub(crate) static TOOLS: &[&dyn SpecTool] =
    &[&crate::mcp::apply::APPLY, &crate::mcp::status::STATUS];

pub(crate) fn find(name: &str) -> Option<&'static dyn SpecTool> {
    TOOLS.iter().copied().find(|tool| tool.name() == name)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::mcp::spec::Fields;
    use crate::mcp::spec::Shown;
    use crate::mcp::spec::kinds::Text;
    use crate::mcp::spec::output::{Count, Str};

    #[derive(Debug)]
    struct Named {
        name: String,
    }

    impl Args for Named {
        fn declare(fields: &mut Fields<'_, '_>) -> Self {
            Self {
                name: fields.required("name", "A component.", &Text { max_bytes: 16 }),
            }
        }
    }

    struct Probed {
        name: String,
        count: usize,
    }

    impl Output for Probed {
        fn declare(fields: &mut Shown<'_, Self>) {
            fields.required("name", "The name given.", &Str, |probed| &probed.name);
            fields.required("count", "What was counted.", &Count, |probed| &probed.count);
        }
    }

    fn count(_: &Store, state: &ProjectState, named: Named) -> Outcome<Probed> {
        Ok(Probed {
            name: named.name,
            count: state.components.len(),
        })
    }

    fn format(mut writer: Writer<'_>, named: Named) -> Outcome<Probed> {
        let formatted = writer.format()?;
        Ok(Probed {
            name: named.name,
            count: formatted.len(),
        })
    }

    /// Take the exclusive lock and let it go: a read before a write holds
    /// none, else this would wait out the lock's timeout and fail.
    fn unlocked(store: &Store, named: Named) -> Outcome<Named> {
        drop(store.lock()?);
        Ok(Named {
            name: format!("{}:free", named.name),
        })
    }

    fn spec(destructive: Destructive, access: Access<Named, Probed>) -> ToolSpec<Named, Probed> {
        ToolSpec {
            name: "probe",
            title: "Probe",
            description: "Probes the store.",
            destructive,
            access,
        }
    }

    /// Run `tool` on a fresh store with `arguments`.
    fn call(tool: &ToolSpec<Named, Probed>, store: &Store, arguments: &Value) -> Outcome<Value> {
        tool.call(store, arguments.as_object().unwrap())
    }

    fn store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::create(tmp.path(), "demo", crate::store::Placement::Tracked).unwrap();
        (tmp, store)
    }

    #[test]
    fn the_definition_follows_the_access_and_carries_both_schemas() {
        let cases = [
            (spec(Destructive::No, Access::Read(count)), true, false),
            (spec(Destructive::No, write(unread)), false, false),
            (spec(Destructive::Yes, write(unread)), false, true),
            (spec(Destructive::No, write(unlocked)), false, false),
        ];
        for (tool, read_only, destructive) in cases {
            let definition = tool.definition();
            let annotations = &definition["annotations"];
            assert_eq!(annotations["readOnlyHint"], read_only, "{definition}");
            assert_eq!(annotations["idempotentHint"], read_only, "{definition}");
            assert_eq!(annotations["destructiveHint"], destructive, "{definition}");
            assert_eq!(annotations["openWorldHint"], false, "{definition}");
            assert_eq!(
                definition["inputSchema"],
                Value::Object(input_schema::<Named>())
            );
            let output = &definition["outputSchema"];
            assert_eq!(output["required"], json!(["name", "count"]), "{output}");
            assert_eq!(output["properties"]["truncated"]["type"], "array");
            assert_eq!(output["additionalProperties"], false);
            assert_eq!(definition["name"], "probe");
            assert_eq!(definition["description"], "Probes the store.");
        }
    }

    #[test]
    fn each_access_runs_its_handler_on_what_it_names() {
        let (_tmp, store) = store();
        let arguments = json!({ "name": "auth" });
        std::fs::write(store.component_path("auth"), "description='Auth'\n").unwrap();

        let formatted = call(&spec(Destructive::No, write(unread)), &store, &arguments);
        assert_eq!(formatted.unwrap(), json!({ "name": "auth", "count": 1 }));
        assert_eq!(store.load_state().unwrap().non_canonical, []);

        let counted = call(
            &spec(Destructive::No, Access::Read(count)),
            &store,
            &arguments,
        );
        assert_eq!(counted.unwrap(), json!({ "name": "auth", "count": 1 }));

        let read_then_written = call(&spec(Destructive::No, write(unlocked)), &store, &arguments);
        assert_eq!(
            read_then_written.unwrap(),
            json!({ "name": "auth:free", "count": 0 })
        );
    }

    /// The arguments as they came: a write that needs nothing read first.
    fn unread(_: &Store, named: Named) -> Outcome<Named> {
        Ok(named)
    }

    /// `format`, after `read`.
    fn write(read: fn(&Store, Named) -> Outcome<Named>) -> Access<Named, Probed> {
        Access::Write {
            read,
            write: format,
        }
    }

    /// A refused argument never reaches the store: with none there, the
    /// refusal is still the argument's.
    #[test]
    fn a_refused_argument_is_refused_before_the_store_is_opened() {
        let store = Store::at("/nonexistent/.trurlic".into());
        let tool = spec(Destructive::No, write(unread));
        let refused = call(&tool, &store, &json!({ "name": 1 })).unwrap_err();
        let payload = serde_json::to_value(&refused).unwrap();
        assert_eq!(payload["kind"], "invalid_argument");
        assert_eq!(payload["field"], "name");

        let failed = call(&tool, &store, &json!({ "name": "auth" })).unwrap_err();
        assert_eq!(
            serde_json::to_value(&failed).unwrap()["kind"],
            "store_error"
        );
    }
}
