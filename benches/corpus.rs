//! Benches over the seeded corpus at each of `Corpus::SIZES`: loading the
//! store from disk, validating the loaded graph, and, through the MCP
//! server in process, every read tool. The server loads the graph on every
//! call, so each tool bench includes a load.
//!
//! Read tools come from `tools/list`, so a new one is benched without editing
//! this file, and setup panics when no request here fits it.
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
use std::sync::LazyLock;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

use trurlic::bench::Server;
use trurlic::store::corpus::Corpus;
use trurlic::store::{ProjectState, Store};

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

/// What requests name: the component with the most decisions, and the
/// first file its anchors name.
struct Targets {
    component: String,
    file: String,
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
            .min_by_key(|name| Reverse(state.decisions_of(name).len()))
            .unwrap();
        let files: BTreeSet<&str> = state
            .decisions_of(component)
            .into_iter()
            .flat_map(|(_, decision)| &decision.anchors)
            .map(|anchor| anchor.file())
            .collect();
        Self {
            component: component.clone(),
            file: files.first().unwrap().to_string(),
        }
    }
}

/// A server over the project at `dir`, past `initialize`.
fn server_at(dir: PathBuf) -> Server {
    let mut server = Server::new(dir);
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
    let mut output = Vec::new();
    server.serve_one(&mut line.as_bytes(), &mut output).unwrap();
    let response: Value = serde_json::from_slice(&output).unwrap();
    let failed = response.get("error").is_some() || response["result"]["isError"] == true;
    assert!(!failed, "{line} -> {response}");
    response
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

/// Every tool `tools/list` marks read-only.
fn read_tools(server: &mut Server) -> Vec<String> {
    let listing = call(server, &request("tools/list", json!({})));
    listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

fn read_tool_arguments(tool: &str, targets: &Targets) -> Value {
    match tool {
        "get_context" => json!({ "component": targets.component }),
        "get_decisions_for_file" => json!({ "file": targets.file }),
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
    group.finish();
}

fn bench_validate(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate");
    for fixture in CORPORA.iter() {
        let state = fixture.store.load_state().unwrap();
        group.bench_function(BenchmarkId::from_parameter(fixture.size), |b| {
            b.iter(|| black_box(&state).validate());
        });
    }
    group.finish();
}

fn bench_read_tools(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_tool");
    for fixture in CORPORA.iter() {
        let project = fixture.store.root().parent().unwrap().to_path_buf();
        let mut server = server_at(project);
        for tool in read_tools(&mut server) {
            let line = tool_call(&tool, read_tool_arguments(&tool, &fixture.targets));
            let id = BenchmarkId::new(tool, fixture.size);
            bench_request(&mut group, id, &mut server, &line);
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_load_state, bench_validate, bench_read_tools
}
criterion_main!(benches);
