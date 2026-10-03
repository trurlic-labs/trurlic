//! Benches over the seeded corpus at each of `Corpus::SIZES`: loading the
//! store from disk, building and validating its graph, one write end to end,
//! and, through the MCP server in process, `advance` for every task type and
//! every other read tool.
//!
//! Read tools and task types come from `tools/list`, so a new one is benched
//! without editing this file, and setup panics when no request here fits it.
//! Setup also sends each request once and panics on an error response, so no
//! bench times an error path.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a bench whose setup fails has nothing to measure"
)]

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, criterion_group, criterion_main,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use trurlic::bench::Server;
use trurlic::store::corpus::Corpus;
use trurlic::store::graph::InMemoryGraph;
use trurlic::store::{DecisionFile, ProjectState, STORE_DIR, Store};

/// One corpus per size, written under `target/tmp` once per run.
static CORPORA: LazyLock<Vec<Fixture>> = LazyLock::new(|| {
    Corpus::SIZES
        .iter()
        .map(|&size| Fixture::write(size))
        .collect()
});

struct Fixture {
    size: usize,
    store: Store,
    targets: Targets,
}

/// What requests name: the component with the most decisions, its decision
/// with the longest history, and the first files its code refs name.
struct Targets {
    component: String,
    decision: String,
    files: Vec<String>,
}

impl Fixture {
    fn write(size: usize) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("corpus-{size}"));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        let store = Corpus::new(size).write(&dir).unwrap();
        let targets = Targets::of(&store.load_state().unwrap());
        Self {
            size,
            store,
            targets,
        }
    }
}

impl Targets {
    fn of(state: &ProjectState) -> Self {
        let component = state
            .components
            .keys()
            .min_by_key(|name| Reverse(decisions_of(state, name).count()))
            .unwrap();
        let decision = decisions_of(state, component)
            .min_by_key(|(_, file)| Reverse(file.decision.history.len()))
            .unwrap()
            .0;
        let files: BTreeSet<&str> = decisions_of(state, component)
            .flat_map(|(_, file)| &file.decision.code_refs)
            .map(|code_ref| code_ref.file.as_str())
            .collect();
        Self {
            component: component.clone(),
            decision: decision.clone(),
            files: files.into_iter().take(3).map(str::to_owned).collect(),
        }
    }
}

fn decisions_of<'a>(
    state: &'a ProjectState,
    component: &'a str,
) -> impl Iterator<Item = (&'a String, &'a Arc<DecisionFile>)> {
    state
        .decisions
        .iter()
        .filter(move |(_, file)| file.decision.component == component)
}

/// A server over the store at `root`, past `initialize`.
fn server_at(root: PathBuf) -> Server {
    let store = Store::at(root);
    let state = store.load_state().unwrap();
    let mut server = Server::new(store, state);
    call(
        &mut server,
        &request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "bench", "version": "0" },
            }),
        ),
    );
    server
}

fn request(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
}

fn tool_call(tool: &str, arguments: Value) -> String {
    request(
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    )
}

/// Send `line` and return the response, panicking on an error response.
fn call(server: &mut Server, line: &str) -> Value {
    let response = send(server, line);
    assert!(succeeded(&response), "{line} -> {response}");
    response
}

fn send(server: &mut Server, line: &str) -> Value {
    let mut output = Vec::new();
    server.serve_one(&mut line.as_bytes(), &mut output).unwrap();
    serde_json::from_slice(&output).unwrap()
}

fn succeeded(response: &Value) -> bool {
    response.get("error").is_none() && response["result"]["isError"] != true
}

/// Bench one request on `server`, after checking that it succeeds.
fn bench_request(
    group: &mut BenchmarkGroup<'_, WallTime>,
    id: BenchmarkId,
    server: &mut Server,
    line: &str,
) {
    call(server, line);
    let mut output = Vec::new();
    group.bench_function(id, |b| {
        b.iter(|| {
            output.clear();
            server
                .serve_one(&mut black_box(line.as_bytes()), &mut output)
                .unwrap()
        });
    });
}

/// `(name, schema)` of every tool `tools/list` marks read-only.
fn read_tools(server: &mut Server) -> Vec<(String, Value)> {
    let listing = call(server, &request("tools/list", json!({})));
    listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_owned(),
                tool["inputSchema"].clone(),
            )
        })
        .collect()
}

fn read_tool_arguments(tool: &str, targets: &Targets) -> Value {
    let Targets {
        component,
        decision,
        files,
    } = targets;
    match tool {
        "get_context" => json!({ "component": component }),
        "check_pattern" => {
            json!({ "description": "retry the request after a timeout under the lock" })
        }
        "get_architecture" => json!({}),
        "get_decision_history" => json!({ "name": decision }),
        "get_decisions_for_file" => json!({ "file": files[0] }),
        "verify_against_decisions" => json!({ "component": component, "changed_files": files }),
        "get_step_prompt" => {
            json!({ "component": component, "step": "walk_decisions", "mode": "agent" })
        }
        other => panic!("no bench request for the read tool `{other}`"),
    }
}

fn bench_load_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("load_state");
    for fixture in CORPORA.iter() {
        group.bench_function(BenchmarkId::from_parameter(fixture.size), |b| {
            b.iter(|| fixture.store.load_state().unwrap());
        });
    }
}

fn bench_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph");
    for fixture in CORPORA.iter() {
        let state = fixture.store.load_state().unwrap();
        group.bench_function(BenchmarkId::new("build", fixture.size), |b| {
            b.iter(|| {
                InMemoryGraph::build(
                    black_box(&state.graph_index),
                    &state.components,
                    &state.decisions,
                    &state.patterns,
                )
            });
        });
        group.bench_function(BenchmarkId::new("validate", fixture.size), |b| {
            b.iter(|| black_box(state.graph()).validate());
        });
    }
}

/// `record_decision` from request to response: the reload under the lock,
/// validation, and the journaled commit. Each iteration writes into its own
/// copy of the corpus, so every write lands on a graph of the same size.
fn bench_record_decision(c: &mut Criterion) {
    let mut group = c.benchmark_group("record_decision");
    group.sample_size(10);
    for fixture in CORPORA.iter() {
        let Targets {
            component,
            decision,
            files,
        } = &fixture.targets;
        let line = tool_call(
            "record_decision",
            json!({
                "component": component,
                "choice": "Bench writes land through the journal like every other write",
                "reason": "A write bench that skipped the journal or the reload would time a path no user takes, so it goes through tools/call.",
                "attribution": "agent",
                "alternatives": ["Time Store::record_decision alone: misses argument parsing and the response"],
                "depends_on": [decision],
                "tags": ["performance"],
                "code_refs": [{ "file": files[0] }],
            }),
        );
        let copy = || {
            let dir = TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
            copy_dir(fixture.store.root(), &dir.path().join(STORE_DIR));
            let server = server_at(dir.path().join(STORE_DIR));
            (dir, server)
        };
        call(&mut copy().1, &line);
        let mut output = Vec::new();
        group.bench_function(BenchmarkId::from_parameter(fixture.size), |b| {
            b.iter_batched_ref(
                copy,
                |(_, server)| {
                    output.clear();
                    server.serve_one(&mut line.as_bytes(), &mut output).unwrap()
                },
                BatchSize::PerIteration,
            );
        });
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `advance` for every task type its schema lists, each in the first mode
/// of the schema that the task type accepts.
fn bench_advance(c: &mut Criterion) {
    let mut group = c.benchmark_group("advance");
    for fixture in CORPORA.iter() {
        let mut server = server_at(fixture.store.root().to_path_buf());
        let schema = read_tools(&mut server)
            .into_iter()
            .find_map(|(name, schema)| (name == "advance").then_some(schema))
            .unwrap();
        let properties = &schema["properties"];
        for task_type in properties["task_type"]["enum"].as_array().unwrap() {
            let line = properties["mode"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mode| {
                    let arguments = json!({
                        "component": fixture.targets.component,
                        "task_type": task_type,
                        "mode": mode,
                    });
                    tool_call("advance", arguments)
                })
                .find(|line| succeeded(&send(&mut server, line)))
                .unwrap_or_else(|| panic!("advance accepts `{task_type}` in no mode"));
            let id = BenchmarkId::new(task_type.as_str().unwrap(), fixture.size);
            bench_request(&mut group, id, &mut server, &line);
        }
    }
}

fn bench_read_tools(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_tool");
    for fixture in CORPORA.iter() {
        let mut server = server_at(fixture.store.root().to_path_buf());
        for (tool, _) in read_tools(&mut server) {
            if tool == "advance" {
                continue;
            }
            let line = tool_call(&tool, read_tool_arguments(&tool, &fixture.targets));
            let id = BenchmarkId::new(tool, fixture.size);
            bench_request(&mut group, id, &mut server, &line);
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_load_state, bench_graph, bench_record_decision, bench_advance, bench_read_tools
}
criterion_main!(benches);
