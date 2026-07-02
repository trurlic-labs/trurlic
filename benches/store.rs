//! CodSpeed benchmarks for store hot paths.
//!
//! Five groups, 20 benchmark points covering graph construction,
//! validation, queries, serialisation, and string processing.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use criterion::{Criterion, criterion_group, criterion_main};

use trurlic::store::graph::InMemoryGraph;
use trurlic::store::schema::{
    Attribution, Component, ComponentFile, Decision, DecisionFile, EdgeEntry, EdgeKind, GraphIndex,
    NodeEntry, NodeKind, Pattern, PatternFile,
};
use trurlic::store::{has_control_chars, hash_bytes, is_valid_kebab_case, slugify};

// ── Fixture generation ──────────────────────────────────────────────────────

/// Complete fixture: graph index + Arc-wrapped content maps ready for
/// `InMemoryGraph::build`.
type Fixture = (
    GraphIndex,
    BTreeMap<String, Arc<ComponentFile>>,
    BTreeMap<String, Arc<DecisionFile>>,
    BTreeMap<String, Arc<PatternFile>>,
);

/// Fixed timestamp shared by all fixture data.
fn ts() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap()
}

/// Generate a structurally realistic graph fixture.
///
/// Produces:
/// - 1 virtual `"project"` node (no `ComponentFile`)
/// - 2 project-wide decisions
/// - `n_components` component nodes, each with 3 decisions
/// - `ConnectsTo` chain across components
/// - `DependsOn` chain within each component's decisions
/// - 1 pattern per 5 components (2 `MemberOf` + 1 `AppliesTo`)
/// - Tags: every 3rd component → `"core"`, first decision per component → `"architecture"`
/// - Deterministic hashes, sorted nodes/edges (mirrors real `load_state`)
fn generate_fixture(n_components: usize) -> Fixture {
    let created = ts();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut components = BTreeMap::new();
    let mut decisions = BTreeMap::new();
    let mut patterns = BTreeMap::new();

    // ── Virtual project node ─────────────────────────────────────────────

    nodes.push(NodeEntry {
        name: "project".into(),
        kind: NodeKind::Component,
        tags: vec![],
        hash: "hash-project".into(),
    });

    // ── Project-wide decisions ───────────────────────────────────────────

    for i in 0..2u32 {
        let name = format!("project-dec-{i}");
        nodes.push(NodeEntry {
            name: name.clone(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: format!("hash-project-dec-{i}"),
        });
        edges.push(EdgeEntry {
            from: name.clone(),
            to: "project".into(),
            kind: EdgeKind::BelongsTo,
        });
        decisions.insert(
            name,
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: format!("Project-wide choice {i}"),
                    reason: format!("Project-wide reason {i}"),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created,
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );
    }

    // ── Components and per-component decisions ──────────────────────────

    for c in 0..n_components {
        let comp_name = format!("comp-{c}");
        let comp_tags = if c % 3 == 0 {
            vec!["core".into()]
        } else {
            vec![]
        };

        nodes.push(NodeEntry {
            name: comp_name.clone(),
            kind: NodeKind::Component,
            tags: comp_tags,
            hash: format!("hash-comp-{c}"),
        });
        components.insert(
            comp_name.clone(),
            Arc::new(ComponentFile {
                component: Component {
                    name: comp_name.clone(),
                    description: format!("Component {c} of the system"),
                },
            }),
        );

        // ConnectsTo chain: comp-0 → comp-1 → comp-2 → …
        if c > 0 {
            edges.push(EdgeEntry {
                from: format!("comp-{}", c - 1),
                to: comp_name.clone(),
                kind: EdgeKind::ConnectsTo,
            });
        }

        // 3 decisions per component with DependsOn chain.
        for d in 0..3u32 {
            let dec_name = format!("dec-{c}-{d}");
            let dec_tags: Vec<String> = if d == 0 {
                vec!["architecture".into()]
            } else {
                vec![]
            };

            nodes.push(NodeEntry {
                name: dec_name.clone(),
                kind: NodeKind::Decision,
                tags: dec_tags.clone(),
                hash: format!("hash-dec-{c}-{d}"),
            });
            edges.push(EdgeEntry {
                from: dec_name.clone(),
                to: comp_name.clone(),
                kind: EdgeKind::BelongsTo,
            });

            // DependsOn chain: dec-N-0 → dec-N-1 → dec-N-2
            if d > 0 {
                edges.push(EdgeEntry {
                    from: format!("dec-{c}-{}", d - 1),
                    to: dec_name.clone(),
                    kind: EdgeKind::DependsOn,
                });
            }

            decisions.insert(
                dec_name,
                Arc::new(DecisionFile {
                    decision: Decision {
                        component: comp_name.clone(),
                        choice: format!("Choice {d} for component {c}"),
                        reason: format!("Reason {d} for component {c}"),
                        alternatives: vec![format!("Alternative for comp-{c} dec-{d}")],
                        tags: dec_tags,
                        attribution: Attribution::Agent,
                        created,
                        code_refs: vec![],
                        history: vec![],
                    },
                }),
            );
        }
    }

    // ── Patterns (1 per 5 components) ────────────────────────────────────

    let n_patterns = n_components / 5;
    for p in 0..n_patterns {
        let pat_name = format!("pattern-{p}");
        let base_comp = p * 5;

        nodes.push(NodeEntry {
            name: pat_name.clone(),
            kind: NodeKind::Pattern,
            tags: vec![],
            hash: format!("hash-pattern-{p}"),
        });

        // 2 MemberOf edges → first decision of two consecutive components.
        for m in 0..2 {
            edges.push(EdgeEntry {
                from: pat_name.clone(),
                to: format!("dec-{}-0", base_comp + m),
                kind: EdgeKind::MemberOf,
            });
        }

        // 1 AppliesTo edge → component.
        edges.push(EdgeEntry {
            from: pat_name.clone(),
            to: format!("comp-{base_comp}"),
            kind: EdgeKind::AppliesTo,
        });

        patterns.insert(
            pat_name,
            Arc::new(PatternFile {
                pattern: Pattern {
                    name: format!("Pattern {p}"),
                    description: format!("Cross-cutting pattern {p}"),
                },
            }),
        );
    }

    // ── Sort to match real load_state behaviour ──────────────────────────

    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

    let index = GraphIndex {
        version: 1,
        rebuilt: created,
        nodes,
        edges,
    };

    (index, components, decisions, patterns)
}

/// Build an [`InMemoryGraph`] at the given scale.
fn build_graph_at(n: usize) -> InMemoryGraph {
    let (index, components, decisions, patterns) = generate_fixture(n);
    InMemoryGraph::build(&index, &components, &decisions, &patterns)
}

/// Serialise a realistic [`DecisionFile`] to TOML (per-file cost in `load_state`).
fn sample_decision_toml() -> String {
    toml::to_string_pretty(&DecisionFile {
        decision: Decision {
            component: "auth".into(),
            choice: "JWT with DPoP binding, 15min lease".into(),
            reason: "Stateless, no session store needed. DPoP prevents token theft.".into(),
            alternatives: vec![
                "Session cookies — rejected: requires server-side state".into(),
                "Opaque tokens — rejected: requires token introspection endpoint".into(),
            ],
            tags: vec!["security".into(), "auth".into()],
            attribution: Attribution::User,
            created: ts(),
            code_refs: vec![],
            history: vec![],
        },
    })
    .unwrap()
}

// ── Benchmark group 1: graph_build ──────────────────────────────────────────
//
// The #1 hot path. Runs on every load_state, every mutation commit,
// every ProjectState::build_graph.

fn bench_graph_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_build");

    for n in [4, 50, 500] {
        let (index, components, decisions, patterns) = generate_fixture(n);
        group.bench_function(format!("{n}_components"), |b| {
            b.iter(|| InMemoryGraph::build(black_box(&index), &components, &decisions, &patterns));
        });
    }

    group.finish();
}

// ── Benchmark group 2: graph_validate ───────────────────────────────────────
//
// Full 11-check validation including DFS cycle detection. Two scale
// points catch accidental quadratic blowup.

fn bench_graph_validate(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_validate");

    for n in [50, 500] {
        let graph = build_graph_at(n);
        group.bench_function(format!("{n}_components"), |b| {
            b.iter(|| black_box(&graph).validate());
        });
    }

    group.finish();
}

// ── Benchmark group 3: graph_query ──────────────────────────────────────────
//
// MCP tool call hot paths. All on a single 50-component graph,
// targeting comp-25 (middle) to avoid boundary effects.

fn bench_graph_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_query");
    let graph = build_graph_at(50);

    group.bench_function("decisions_for", |b| {
        b.iter(|| graph.decisions_for(black_box("comp-25")));
    });

    group.bench_function("related_decisions", |b| {
        b.iter(|| graph.related_decisions(black_box("comp-25")));
    });

    group.bench_function("transitive_depends_on_depth3", |b| {
        b.iter(|| graph.transitive_depends_on(black_box(&["dec-0-0"]), 3));
    });

    group.bench_function("check_decision_cascade", |b| {
        b.iter(|| graph.check_decision_cascade(black_box("dec-25-2")));
    });

    group.bench_function("check_component_cascade", |b| {
        b.iter(|| graph.check_component_cascade(black_box("comp-25")));
    });

    group.finish();
}

// ── Benchmark group 4: graph_serialize ──────────────────────────────────────
//
// Serialisation paths used by load_state and commit_with_graph.

fn bench_graph_serialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_serialize");

    // to_index at two scale points to catch sort overhead.
    let graph_50 = build_graph_at(50);
    let graph_500 = build_graph_at(500);

    group.bench_function("to_index_50_components", |b| {
        b.iter(|| black_box(&graph_50).to_index());
    });

    group.bench_function("to_index_500_components", |b| {
        b.iter(|| black_box(&graph_500).to_index());
    });

    // TOML round-trip for a 50-component GraphIndex.
    let exported = graph_50.to_index();

    group.bench_function("toml_serialize_graph_index", |b| {
        b.iter(|| toml::to_string_pretty(black_box(&exported)));
    });

    let toml_str = toml::to_string_pretty(&exported).unwrap();
    group.bench_function("toml_deserialize_graph_index", |b| {
        b.iter(|| toml::from_str::<GraphIndex>(black_box(&toml_str)));
    });

    // Per-file unit cost: single realistic DecisionFile.
    let decision_toml = sample_decision_toml();
    group.bench_function("toml_deserialize_decision", |b| {
        b.iter(|| toml::from_str::<DecisionFile>(black_box(&decision_toml)));
    });

    group.finish();
}

// ── Benchmark group 5: string_ops ───────────────────────────────────────────
//
// String processing and hashing called on every mutation and file read.

fn bench_string_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_ops");

    // ── slugify ──────────────────────────────────────────────────────────

    let slugify_inputs: [&str; 10] = [
        "JWT with DPoP binding, 15min lease",
        "Result<T, AppError>",
        "429 + retry-after header",
        "Use PostgreSQL for persistent state",
        "gRPC between internal services",
        "Rate limit: 100 req/s per tenant",
        "Encrypt PII at rest (AES-256-GCM)",
        "Blue/green deployment via k8s",
        "OAuth 2.0 + PKCE for SPA clients",
        "WebSocket for real-time map updates",
    ];
    group.bench_function("slugify_batch", |b| {
        b.iter(|| {
            for input in &slugify_inputs {
                black_box(slugify(black_box(input)));
            }
        });
    });

    // ── is_valid_kebab_case ──────────────────────────────────────────────

    let kebab_inputs: [&str; 10] = [
        "auth",
        "rate-limiter",
        "database-pool",
        "use-jwt",
        "error-strategy",
        "Bad_Name",
        "-leading",
        "trailing-",
        "double--hyphen",
        "UPPERCASE",
    ];
    group.bench_function("is_valid_kebab_case_batch", |b| {
        b.iter(|| {
            for input in &kebab_inputs {
                black_box(is_valid_kebab_case(black_box(input)));
            }
        });
    });

    // ── has_control_chars ────────────────────────────────────────────────

    let control_inputs: [&str; 5] = [
        "A normal description with no special characters at all.",
        "Line one\nLine two\nLine three with newlines",
        "Indented\twith\ttabs throughout the text",
        "Unicode: café, naïve, résumé, 日本語テスト",
        "Mixed whitespace:\n\tindented line\n\tanother\r\nwindows",
    ];
    group.bench_function("has_control_chars_batch", |b| {
        b.iter(|| {
            for input in &control_inputs {
                black_box(has_control_chars(black_box(input)));
            }
        });
    });

    // ── hash_bytes ───────────────────────────────────────────────────────

    let payload_1kb = vec![0xABu8; 1024];
    group.bench_function("hash_bytes_1kb", |b| {
        b.iter(|| hash_bytes(black_box(&payload_1kb)));
    });

    let payload_64kb = vec![0xABu8; 65_536];
    group.bench_function("hash_bytes_64kb", |b| {
        b.iter(|| hash_bytes(black_box(&payload_64kb)));
    });

    group.finish();
}

// ── Harness ─────────────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_graph_build,
    bench_graph_validate,
    bench_graph_query,
    bench_graph_serialize,
    bench_string_ops,
);
criterion_main!(benches);
