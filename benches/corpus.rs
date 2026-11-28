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
//! table too, which a load parses and never compiles. `status` is timed
//! with it whole, which judges the limits alone, and with its `lint`
//! section alone, which compiles the patterns and judges them too.
//!
//! `code_index` times the code index alone: the anchor findings of each
//! corpus as `status` gathers them. They walk, read and index the crate
//! the corpus writes and run no git. Each size is timed cold, the cache
//! emptied before each run, and warm, every file's index read from the
//! cache.
//!
//! `check_git` times `check`'s anchor findings, the index warm, over a
//! repository the bench builds from the smallest corpus: two commits and a
//! linked worktree. The bench's git makes them in an empty environment,
//! with a fixed identity and date. `check` reads that worktree and that
//! history, and setup panics unless some anchors come out pending and some
//! repaired. The git `check` runs reads the configuration the environment
//! names: `make bench` names none, and a run outside it reads the user's.
//!
//! Neither group reads the tree the bench is built in, so two builds of
//! one commit time the same work, whatever that tree's history, untracked
//! files or worktrees.
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
use std::env;
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

use trurlic::bench::{
    Corpus, Placement, ProjectState, Server, Store, checked_findings, likely_duplicates,
    tree_findings,
};

/// The example `[quality]` table the repository ships.
const QUALITY: &str = include_str!("../examples/quality.toml");

/// `store`'s `project.toml` with [`QUALITY`] appended.
fn add_quality(store: &Store) {
    let path = store.root().join("project.toml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\n{QUALITY}")).unwrap();
}

/// The directory `name` under `CARGO_TARGET_TMPDIR`, emptied of what an
/// earlier run left there; not created.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    dir
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
        let dir = fresh_dir(&format!("corpus-{size}"));
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
        let dir = fresh_dir(&format!("corpus-quality-{}", fixture.size));
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

/// `status` over a store with the shipped `[quality]`: the whole report,
/// which judges the bar's limits alone, and the `lint` section alone,
/// which compiles the patterns and judges them too.
fn bench_status_lint(c: &mut Criterion) {
    let mut group = c.benchmark_group("status_quality");
    for &size in &Corpus::SIZES {
        let dir = fresh_dir(&format!("corpus-lint-{size}"));
        add_quality(&Corpus::new(size).write(&dir).unwrap());
        let mut server = server_at(dir);
        for (name, arguments) in [("all", json!({})), ("lint", json!({ "section": "lint" }))] {
            let line = tool_call("status", arguments);
            let id = BenchmarkId::new(name, size);
            bench_request(&mut group, id, &mut server, &line);
        }
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

/// The environment of the bench's git but `PATH`: no system or user
/// configuration, and the author, committer and date of every commit.
const GIT_ENVIRONMENT: [(&str, &str); 8] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_AUTHOR_NAME", "bench"),
    ("GIT_AUTHOR_EMAIL", "bench@bench.invalid"),
    ("GIT_AUTHOR_DATE", "2025-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "bench"),
    ("GIT_COMMITTER_EMAIL", "bench@bench.invalid"),
    ("GIT_COMMITTER_DATE", "2025-01-01T00:00:00Z"),
];

/// `git <args>` in `dir`, in an empty environment but the caller's `PATH`,
/// which finds git, and [`GIT_ENVIRONMENT`]. So no variable of the caller
/// points it at another repository, a template or a configuration, and a
/// commit of the same tree is the same object on every run.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .envs(env::var_os("PATH").map(|path| ("PATH", path)))
        .envs(GIT_ENVIRONMENT)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");
    group.sample_size(20);
    for &size in &Corpus::SIZES[1..] {
        for before in Before::ALL {
            let dir = fresh_dir(&format!(
                "corpus-write-{size}-{}",
                before.name().replace('/', "-")
            ));
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
                    git(store.root(), &["commit", "--quiet", "--all", "-m", "edit"]);
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
            let dir = fresh_dir(&format!("corpus-apply-{size}-{}", setup.replace('+', "-")));
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
    group.sample_size(20);
    for fixture in CORPORA.iter() {
        let store = &fixture.store;
        let state = store.load_state().unwrap();
        let cache = store.root().join(".state/symbols");
        group.bench_function(BenchmarkId::new("cold", fixture.size), |b| {
            b.iter_batched(
                || {
                    if cache.exists() {
                        fs::remove_dir_all(&cache).unwrap();
                    }
                },
                |()| tree_findings(store, &state),
                BatchSize::PerIteration,
            );
        });
        let _warm = tree_findings(store, &state);
        group.bench_function(BenchmarkId::new("warm", fixture.size), |b| {
            b.iter(|| tree_findings(store, &state));
        });
    }
    group.finish();
}

/// A repository at `repository-<size>/main` under `CARGO_TARGET_TMPDIR`,
/// whose first commit holds the corpus of `size` decisions but one
/// anchored file of the second component. A linked worktree at
/// `repository-<size>/side` forks there, and its own commit adds that
/// file. On `main`, a second commit moves the first component's sources.
/// So `check` in `main` follows the moved files through history and finds
/// the missing one in `side`.
fn checked_repository(size: usize) -> Store {
    let dir = fresh_dir(&format!("repository-{size}"));
    let main = dir.join("main");
    let store = Corpus::new(size).write(&main).unwrap();
    fs::write(main.join(".gitignore"), ".trurlic/.state/\n").unwrap();
    let state = store.load_state().unwrap();
    let mut components = state.components.keys();
    let (moved, held) = (components.next().unwrap(), components.next().unwrap());
    let anchored: BTreeSet<&str> = state
        .decisions
        .values()
        .flat_map(|decision| &decision.anchors)
        .map(|anchor| anchor.file())
        .collect();
    let pending = anchored
        .into_iter()
        .find(|file| file.starts_with(&format!("src/{held}/")))
        .unwrap();
    let text = fs::read_to_string(main.join(pending)).unwrap();
    fs::remove_file(main.join(pending)).unwrap();
    git(&main, &["init", "--quiet", "--initial-branch=main"]);
    git(&main, &["add", "--all"]);
    git(&main, &["commit", "--quiet", "-m", "corpus"]);
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "side", "../side"],
    );
    let side = dir.join("side");
    fs::write(side.join(pending), text).unwrap();
    git(&side, &["add", pending]);
    git(&side, &["commit", "--quiet", "-m", "add"]);
    let from = format!("src/{moved}");
    git(&main, &["mv", &from, &format!("{from}-moved")]);
    git(&main, &["commit", "--quiet", "-m", "move"]);
    store
}

fn bench_check_git(c: &mut Criterion) {
    let mut group = c.benchmark_group("check_git");
    let size = Corpus::SIZES[0];
    let store = checked_repository(size);
    let state = store.load_state().unwrap();
    let verdicts = checked_findings(&store, &state);
    assert!(
        verdicts.repaired > 0 && verdicts.pending > 0,
        "{verdicts:?}"
    );
    group.bench_function(BenchmarkId::from_parameter(size), |b| {
        b.iter(|| checked_findings(&store, &state));
    });
    group.finish();
}

fn bench_query_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_compile");
    for name in ["c", "cpp", "python", "typescript", "tsx"] {
        group.bench_function(name, |b| {
            b.iter(|| assert!(trurlic::bench::query_compiles(name)));
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = bench_load_state, bench_validate, bench_duplicates, bench_status_lint,
        bench_read_tools, bench_write, bench_apply, bench_code_index, bench_check_git,
        bench_query_compile
}
criterion_main!(benches);
