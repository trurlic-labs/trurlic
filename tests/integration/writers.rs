//! Two `trurlic serve` processes and one `trurlic map` server take turns on
//! one store, 1000 writes in all. Writes follow each other faster than a
//! watcher reloads, so most land on a server that has not seen the other
//! servers' last writes: the lost update. Every write must validate against
//! the store on disk and keep what the others committed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde_json::json;

use crate::harness::{MapServer, McpClient, Project};

const WRITES: usize = 1000;

/// A decision as the writes so far must have left it.
struct Expected {
    component: String,
    choice: String,
    reason: String,
    history: usize,
}

/// Everything the writes so far must have left on disk.
#[derive(Default)]
struct Model {
    components: BTreeSet<String>,
    decisions: BTreeMap<String, Expected>,
    /// `(from, to, kind)` for the edges a write asked for explicitly.
    edges: BTreeSet<(String, String, String)>,
    /// The last decision each MCP client recorded.
    last_recorded: [Option<String>; 2],
    revisions: usize,
}

impl Model {
    fn component(&self, index: usize) -> String {
        let components: Vec<&String> = self.components.iter().collect();
        components[index % components.len()].clone()
    }

    fn record(&mut self, client: usize, stem: &str, expected: Expected) {
        let previous = self.decisions.insert(stem.to_owned(), expected);
        assert!(previous.is_none(), "two decisions got the stem `{stem}`");
        self.edges
            .insert(edge(stem, &self.decisions[stem].component, "belongs_to"));
        self.last_recorded[client] = Some(stem.to_owned());
    }

    /// The next revision of `name`: a new choice and reason, one more
    /// history entry.
    fn revise(&mut self, name: &str) -> (String, String) {
        self.revisions += 1;
        let revised = self.decisions.get_mut(name).unwrap();
        revised.choice = format!("{} (revision {})", name, self.revisions);
        revised.reason = format!("Revision {} replaces the earlier reasoning", self.revisions);
        revised.history += 1;
        (revised.choice.clone(), revised.reason.clone())
    }

    /// The first pair of components with no connection yet.
    fn unconnected_pair(&self) -> Option<(String, String)> {
        self.components.iter().find_map(|from| {
            self.components
                .iter()
                .find(|to| from != *to && !self.edges.contains(&edge(from, to, "connects_to")))
                .map(|to| (from.clone(), to.clone()))
        })
    }
}

fn edge(from: &str, to: &str, kind: &str) -> (String, String, String) {
    (from.to_owned(), to.to_owned(), kind.to_owned())
}

/// One write through MCP client `client` (0 or 1): a decision that depends
/// on the other client's latest one, a revision of that one, or a new
/// connection.
fn mcp_write(mcp: &mut McpClient, client: usize, round: usize, model: &mut Model) {
    let other = model.last_recorded[1 - client].clone();
    match (round % 3, other) {
        (1, Some(target)) => {
            let (choice, reason) = model.revise(&target);
            mcp.call_tool(
                "update_decision",
                json!({ "name": target, "mode": "revise", "choice": choice, "reason": reason }),
            );
        }
        (2, _) if model.unconnected_pair().is_some() => {
            let (from, to) = model.unconnected_pair().unwrap();
            mcp.call_tool("add_connection", json!({ "from": from, "to": to }));
            model.edges.insert(edge(&from, &to, "connects_to"));
        }
        (_, depends_on) => record(mcp, client, round, depends_on, model),
    }
}

/// Both clients record "Rule <round>" in the same round, in different
/// components, so their choices slugify to the same stem and the second
/// must get another one.
fn record(
    mcp: &mut McpClient,
    client: usize,
    round: usize,
    depends_on: Option<String>,
    model: &mut Model,
) {
    let component = model.component(round + client);
    let choice = format!("Rule {round}{}", if client == 0 { "" } else { "!" });
    let reason = format!("Recorded by client {client} in round {round}");
    let recorded = mcp.call_tool(
        "record_decision",
        json!({
            "component": component,
            "choice": choice,
            "reason": reason,
            "attribution": "agent",
            "depends_on": depends_on.iter().collect::<Vec<_>>(),
        }),
    );
    let stem = recorded["name"].as_str().unwrap().to_owned();
    if let Some(target) = depends_on {
        model.edges.insert(edge(&stem, &target, "depends_on"));
    }
    model.record(
        client,
        &stem,
        Expected {
            component,
            choice,
            reason,
            history: 0,
        },
    );
}

/// One write through the map: a component, a revision of a decision an
/// MCP client recorded, or a connection.
fn map_write(map: &MapServer, round: usize, model: &mut Model) {
    let target = model.last_recorded[round % 2].clone();
    let (status, body) = match (round % 3, target) {
        (1, Some(target)) => {
            let (choice, reason) = model.revise(&target);
            map.request(
                "PUT",
                &format!("/api/decision/{target}"),
                Some(&json!({ "choice": choice, "reason": reason })),
            )
        }
        (2, _) if model.unconnected_pair().is_some() => {
            let (from, to) = model.unconnected_pair().unwrap();
            model.edges.insert(edge(&from, &to, "connects_to"));
            map.request(
                "POST",
                "/api/connection",
                Some(&json!({ "from": from, "to": to })),
            )
        }
        _ => {
            let name = format!("map-{round}");
            model.components.insert(name.clone());
            map.request("POST", "/api/component", Some(&json!({ "name": name })))
        }
    };
    assert_eq!(status, 200, "map write in round {round}: {body}");
}

fn read_toml(path: &std::path::Path) -> toml::Table {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_disk_matches(project: &Project, model: &Model) {
    let store = project.path().join(".trurlic");

    let mut decisions = BTreeMap::new();
    for entry in fs::read_dir(store.join("decisions")).unwrap() {
        let path = entry.unwrap().path();
        let stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
        decisions.insert(stem, read_toml(&path)["decision"].clone());
    }
    let stems: BTreeSet<&String> = decisions.keys().collect();
    assert_eq!(stems, model.decisions.keys().collect(), "decision files");
    for (stem, expected) in &model.decisions {
        let decision = &decisions[stem];
        assert_eq!(
            decision["component"].as_str(),
            Some(&*expected.component),
            "{stem}"
        );
        assert_eq!(
            decision["choice"].as_str(),
            Some(&*expected.choice),
            "{stem}"
        );
        assert_eq!(
            decision["reason"].as_str(),
            Some(&*expected.reason),
            "{stem}"
        );
        let history = decision
            .get("history")
            .map_or(0, |h| h.as_array().unwrap().len());
        assert_eq!(history, expected.history, "history of {stem}");
    }

    let components: BTreeSet<String> = fs::read_dir(store.join("components"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            path.file_stem().unwrap().to_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(components, model.components, "component files");

    let graph = read_toml(&store.join("graph.toml"));
    let edges: BTreeSet<(String, String, String)> = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let field = |key: &str| e[key].as_str().unwrap().to_owned();
            (field("from"), field("to"), field("kind"))
        })
        .collect();
    assert_eq!(edges, model.edges, "graph.toml edges");

    let check = project.run_ok(&["check"]);
    let stdout = String::from_utf8_lossy(&check.stdout);
    assert!(stdout.contains("is consistent"), "{stdout}");
}

#[test]
fn two_servers_and_a_map_lose_no_write() {
    let project = Project::init();
    for name in ["auth", "billing"] {
        project.run_ok(&["add", "component", name]);
    }
    let mut model = Model {
        components: ["auth", "billing"].map(String::from).into(),
        ..Model::default()
    };
    let mut clients = [
        McpClient::connect(&project).0,
        McpClient::connect(&project).0,
    ];
    let map = MapServer::start(&project);

    for step in 0..WRITES {
        let round = step / 3;
        match step % 3 {
            2 => map_write(&map, round, &mut model),
            client => mcp_write(&mut clients[client], client, round, &mut model),
        }
    }

    assert!(model.revisions > 100, "{} revisions", model.revisions);
    assert_disk_matches(&project, &model);
}
