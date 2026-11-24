//! Benches over the seeded corpus at each of `Corpus::SIZES` (the `fmt` and
//! `apply` benches skip the smallest): loading the store from disk,
//! validating the loaded graph, through the MCP server in process every
//! read tool and `apply`, and `trurlic fmt` of one file. The
//! server loads the graph on every call, so each tool bench includes a
//! load. `fmt` is timed under `tracked`, under `shared` with nothing but
//! its own commit to make, and under `shared` after a hand edit, which is
//! a commit of its own: the differences are what the history costs.
//! `apply` is timed with one op and with fifty, under each placement and,
//! under `tracked`, with the shipped example `[quality]` (eleven patterns),
//! whose patterns each batch compiles; `load_state` is timed with that
//! table too, which a read parses and never compiles.
//!
//! The code index is timed over this repository's own `src/`, cold (its
//! cache emptied before each run) and warm (every file's index read from
//! the cache): resolving the anchors of the smallest corpus walks, reads
//! and indexes the whole tree, as `status` and `check` do.
//!
//! Read tools come from `tools/list`, so a new one is never skipped: setup
//! panics until a request here fits it.
//! Setup also sends each request once and panics on an error response, so no
//! bench times an error path.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a bench whose setup fails has nothing to measure"
)]

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, criterion_group, criterion_main,
};
use serde_json::{Value, json};

use trurlic::bench::{Corpus, Server, likely_duplicates};
use trurlic::store::{Placement, ProjectState, Store};

/// The example `[quality]` table the repository ships.
const QUALITY: &str = include_str!("../examples/quality.toml");

/// `store`'s `project.toml` with [`QUALITY`] appended.
fn add_quality(store: &Store) {
    let path = store.root().join("project.toml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\n{QUALITY}")).unwrap();
}

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
        "status" => json!({}),
        other => panic!("no bench request for the read tool `{other}`"),
    }
}

fn bench_load_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("load_state");
    for fixture in CORPORA.iter() {
        group.bench_function(BenchmarkId::from_parameter(fixture.size), |b| {
            b.iter(|| fixture.store.load_state().unwrap());
        });
        let dir =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("corpus-quality-{}", fixture.size));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        let store = Corpus::new(fixture.size).write(&dir).unwrap();
        add_quality(&store);
        group.bench_function(BenchmarkId::new("quality", fixture.size), |b| {
            b.iter(|| store.load_state().unwrap());
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

/// The pass over every pair of decisions that `status` makes for its
/// likely duplicates, from the loaded graph: the index and the pairs.
fn bench_duplicates(c: &mut Criterion) {
    let mut group = c.benchmark_group("duplicates");
    for fixture in CORPORA.iter() {
        let state = fixture.store.load_state().unwrap();
        group.bench_function(BenchmarkId::from_parameter(fixture.size), |b| {
            b.iter(|| likely_duplicates(black_box(&state)));
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

/// How the write bench leaves the graph before each `fmt`.
#[derive(Clone, Copy)]
enum Before {
    /// `tracked`: the project's git holds the history.
    Tracked,
    /// `shared`, the hand edit `fmt` rewrites committed in setup.
    Committed,
    /// `shared`, the hand edit left for `fmt` to commit first.
    HandEdit,
}

impl Before {
    const ALL: [Self; 3] = [Self::Tracked, Self::Committed, Self::HandEdit];

    const fn name(self) -> &'static str {
        match self {
            Self::Tracked => "fmt/tracked",
            Self::Committed => "fmt/shared",
            Self::HandEdit => "fmt/shared_after_hand_edit",
        }
    }

    const fn placement(self) -> Placement {
        match self {
            Self::Tracked => Placement::Tracked,
            Self::Committed | Self::HandEdit => Placement::Shared,
        }
    }
}

/// `git <args>` in the graph directory, reading no user configuration.
fn graph_git(store: &Store, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(store.root())
        .args([
            "-c",
            "user.name=bench",
            "-c",
            "user.email=bench@bench.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");
    group.sample_size(20);
    for &size in &Corpus::SIZES[1..] {
        for before in Before::ALL {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
                "corpus-write-{size}-{}",
                before.name().replace('/', "-")
            ));
            if dir.exists() {
                fs::remove_dir_all(&dir).unwrap();
            }
            let store = Corpus::new(size)
                .write_as(&dir, before.placement())
                .unwrap();
            let state = store.load_state().unwrap();
            let file = store.component_path(state.components.keys().next().unwrap());
            let canonical = fs::read_to_string(&file).unwrap();
            // A new description each time, so every commit writes new
            // objects, as a real write does.
            let edits = Cell::new(0_u64);
            let edit = || {
                edits.set(edits.get() + 1);
                let edited = canonical.replacen(
                    "description = \"",
                    &format!("description=\"{} ", edits.get()),
                    1,
                );
                fs::write(&file, edited).unwrap();
                if matches!(before, Before::Committed) {
                    graph_git(&store, &["commit", "--quiet", "--all", "-m", "edit"]);
                }
                // The setup's writes reach the disk before the write starts,
                // so its flushes wait for its own data alone.
                assert!(Command::new("sync").status().unwrap().success());
            };
            group.bench_function(BenchmarkId::new(before.name(), size), |b| {
                b.iter_batched(
                    edit,
                    |()| store.begin_write().unwrap().format().unwrap(),
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

/// `apply` end to end through the server, a batch of one op and one of
/// fifty, under each placement and under `tracked` with the example
/// `[quality]`: each op revises the reason of one decision, anew on every
/// run, so every batch writes and commits.
fn bench_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply");
    group.sample_size(20);
    let setups = [
        ("tracked", Placement::Tracked, false),
        ("shared", Placement::Shared, false),
        ("tracked+quality", Placement::Tracked, true),
    ];
    for &size in &Corpus::SIZES[1..] {
        for (setup, placement, quality) in setups {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("corpus-apply-{size}-{}", setup.replace('+', "-")));
            if dir.exists() {
                fs::remove_dir_all(&dir).unwrap();
            }
            let store = Corpus::new(size).write_as(&dir, placement).unwrap();
            if quality {
                add_quality(&store);
            }
            let names: Vec<String> = store.load_state().unwrap().decisions.into_keys().collect();
            let mut server = server_at(dir);
            let runs = Cell::new(0_u64);
            for ops in [1, 50] {
                let batch = || {
                    runs.set(runs.get() + 1);
                    let revisions: Vec<Value> = names[..ops]
                        .iter()
                        .map(|name| {
                            let reason = format!("Revised by run {} of the bench", runs.get());
                            json!({ "op": "revise", "name": name, "reason": reason })
                        })
                        .collect();
                    let line = tool_call("apply", json!({ "ops": revisions, "origin": "bench" }));
                    // The setup's writes reach the disk before the batch
                    // starts, so its flushes wait for its own data alone.
                    assert!(Command::new("sync").status().unwrap().success());
                    line
                };
                call(&mut server, &batch());
                let id = BenchmarkId::new(format!("{setup}/{ops}_ops"), size);
                let mut output = Vec::new();
                group.bench_function(id, |b| {
                    b.iter_batched(
                        batch,
                        |line| {
                            output.clear();
                            server.serve_one(&mut line.as_bytes(), &mut output).unwrap()
                        },
                        BatchSize::PerIteration,
                    );
                });
            }
        }
    }
    group.finish();
}

fn bench_code_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("code_index");
    let fixture = CORPORA.first().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let store = Store::reached_from(fixture.store.root().to_path_buf(), source);
    let state = store.load_state().unwrap();
    let cache = store.root().join(".state/symbols");
    group.bench_function("src/cold", |b| {
        b.iter_batched(
            || {
                if cache.exists() {
                    fs::remove_dir_all(&cache).unwrap();
                }
            },
            |()| trurlic::bench::resolve_anchors(&store, &state),
            BatchSize::PerIteration,
        );
    });
    trurlic::bench::resolve_anchors(&store, &state);
    group.bench_function("src/warm", |b| {
        b.iter(|| trurlic::bench::resolve_anchors(&store, &state));
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_load_state, bench_validate, bench_duplicates, bench_read_tools, bench_write,
        bench_apply, bench_code_index
}
criterion_main!(benches);
