//! Tools built on a spec, and the registry a call is looked up in.
//!
//! A tool's [`Access`] states what it runs on, and the call takes that
//! access for it after its arguments parse: a read the graph loaded under
//! the shared lock, a write a [`Writer`] under the exclusive one. So no
//! handler loads or locks, and its `readOnlyHint` is the lock it gets.

use serde_json::{Map, Value, json};

use super::{Args, At, input_schema, parse};
use crate::mcp::peer::Peer;
use crate::mcp::result::ToolError;
use crate::store::{ProjectState, Store, Writer};

/// The tool's outcome: its payload, or why it refused.
type Outcome = Result<Value, ToolError>;

/// What a tool's handler runs on.
pub(crate) enum Access<A> {
    /// The graph loaded under the shared lock.
    Read(fn(&ProjectState, A) -> Outcome),
    /// A writer: the graph loaded under the exclusive lock.
    Write(fn(&mut Writer<'_>, A) -> Outcome),
    /// The store and the client: a handler that asks the client and waits
    /// for the answer loads or writes the store itself, so it holds no
    /// lock while a person fills in a form.
    Interactive(fn(&Store, &mut Peer<'_>, A) -> Outcome),
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
pub(crate) struct ToolSpec<A> {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub destructive: Destructive,
    pub access: Access<A>,
}

/// A [`ToolSpec`] with its argument type erased, as the registry holds it.
pub(crate) trait SpecTool: Sync {
    fn name(&self) -> &'static str;

    /// The tool's entry in `tools/list`.
    fn definition(&self) -> Value;

    /// Parse `arguments`, then run the tool on its access to `store`. An
    /// argument the spec refuses touches no file.
    fn call(&self, store: &Store, peer: &mut Peer<'_>, arguments: &Map<String, Value>) -> Outcome;
}

impl<A: Args> SpecTool for ToolSpec<A> {
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
        })
    }

    fn call(&self, store: &Store, peer: &mut Peer<'_>, arguments: &Map<String, Value>) -> Outcome {
        let arguments = parse::<A>(arguments, At::Root)?;
        match self.access {
            Access::Read(run) => run(&store.load_state()?, arguments),
            Access::Write(run) => run(&mut store.begin_write()?, arguments),
            Access::Interactive(run) => run(store, peer, arguments),
        }
    }
}

/// Every tool built on a spec. A call is looked up here first, and
/// `tools/list` lists these before the tools that predate specs.
pub(crate) static TOOLS: &[&dyn SpecTool] = &[];

pub(crate) fn find(name: &str) -> Option<&'static dyn SpecTool> {
    TOOLS.iter().copied().find(|tool| tool.name() == name)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::mcp::peer::Client;
    use crate::mcp::spec::Fields;
    use crate::mcp::spec::kinds::Text;

    #[derive(Debug, Default)]
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

    fn count(state: &ProjectState, named: Named) -> Outcome {
        Ok(json!({ "name": named.name, "components": state.components.len() }))
    }

    fn format(writer: &mut Writer<'_>, named: Named) -> Outcome {
        let formatted = writer
            .format()
            .map_err(|e| ToolError::refused(e.to_string()))?;
        Ok(json!({ "name": named.name, "formatted": formatted.len() }))
    }

    fn ask(_: &Store, peer: &mut Peer<'_>, _: Named) -> Outcome {
        Ok(json!({ "cancelled": peer.cancelled() }))
    }

    fn spec(destructive: Destructive, access: Access<Named>) -> ToolSpec<Named> {
        ToolSpec {
            name: "probe",
            title: "Probe",
            description: "Probes the store.",
            destructive,
            access,
        }
    }

    /// Run `tool` on a fresh store with `arguments`.
    fn run(tool: &ToolSpec<Named>, store: &Store, arguments: &Value) -> Outcome {
        let mut client = Client::default();
        let (mut reader, mut writer) = (&b""[..], Vec::new());
        let id = json!(1);
        let mut peer = Peer::new(&mut reader, &mut writer, &mut client, &id);
        tool.call(store, &mut peer, arguments.as_object().unwrap())
    }

    fn store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::create(tmp.path(), "demo", crate::store::Placement::Tracked).unwrap();
        (tmp, store)
    }

    #[test]
    fn the_annotations_follow_the_access_and_the_destructive_mark() {
        let cases = [
            (spec(Destructive::No, Access::Read(count)), true, false),
            (spec(Destructive::No, Access::Write(format)), false, false),
            (spec(Destructive::Yes, Access::Write(format)), false, true),
            (
                spec(Destructive::No, Access::Interactive(ask)),
                false,
                false,
            ),
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
            assert_eq!(definition["name"], "probe");
            assert_eq!(definition["description"], "Probes the store.");
        }
    }

    #[test]
    fn each_access_runs_its_handler_on_what_it_names() {
        let (_tmp, store) = store();
        let arguments = json!({ "name": "auth" });
        std::fs::write(store.component_path("auth"), "description='Auth'\n").unwrap();

        let formatted = run(
            &spec(Destructive::No, Access::Write(format)),
            &store,
            &arguments,
        );
        assert_eq!(
            formatted.unwrap(),
            json!({ "name": "auth", "formatted": 1 })
        );
        assert_eq!(store.load_state().unwrap().non_canonical, []);

        let counted = run(
            &spec(Destructive::No, Access::Read(count)),
            &store,
            &arguments,
        );
        assert_eq!(counted.unwrap(), json!({ "name": "auth", "components": 1 }));

        let asked = run(
            &spec(Destructive::No, Access::Interactive(ask)),
            &store,
            &arguments,
        );
        assert_eq!(asked.unwrap(), json!({ "cancelled": false }));
    }

    /// A refused argument never reaches the store: with none there, the
    /// refusal is still the argument's.
    #[test]
    fn a_refused_argument_is_refused_before_the_store_is_opened() {
        let store = Store::at("/nonexistent/.trurlic".into());
        let tool = spec(Destructive::No, Access::Write(format));
        let refused = run(&tool, &store, &json!({ "name": 1 })).unwrap_err();
        let payload = serde_json::to_value(&refused).unwrap();
        assert_eq!(payload["kind"], "invalid_argument");
        assert_eq!(payload["field"], "name");

        let failed = run(&tool, &store, &json!({ "name": "auth" })).unwrap_err();
        assert_eq!(
            serde_json::to_value(&failed).unwrap()["kind"],
            "store_error"
        );
    }
}
