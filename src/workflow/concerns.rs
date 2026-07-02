//! Concern tracking and coverage computation.
//!
//! Architectural concern areas with keyword matching for determining which
//! areas of a component's design are covered by existing decisions. Used by
//! the advance state machine, context assembly, and prompt builders.
//!
//! **Array order IS priority order.** Priority determines the `focus` field
//! in `CoverConcerns` steps — the most dangerous gaps are addressed first.

use crate::store::schema::DecisionFile;

// ── Concern areas ─────────────────────────────────────────────────────────

pub const CONCERNS: &[(&str, &[&str])] = &[
    (
        "Security boundaries",
        &[
            "security",
            "auth",
            "authentication",
            "authorization",
            "token",
            "permission",
            "trust",
            "encrypt",
            "encryption",
            "secret",
            "credential",
            "tls",
            "certificate",
            "zeroize",
            "sanitize",
            "injection",
            "vulnerability",
        ],
    ),
    (
        "Error handling & failure modes",
        &[
            "error",
            "errors",
            "fail",
            "failure",
            "panic",
            "result",
            "recovery",
            "retry",
            "graceful",
            "crash",
            "fault",
            "fallback",
            "timeout",
            "overflow",
            "abort",
            "exception",
            "hang",
        ],
    ),
    (
        "Concurrency & locking",
        &[
            "lock",
            "locking",
            "concurrent",
            "concurrency",
            "mutex",
            "rwlock",
            "atomic",
            "thread",
            "async",
            "parallel",
            "race",
            "deadlock",
            "flock",
            "channel",
            "contention",
        ],
    ),
    (
        "Integrity & validation",
        &[
            "hash",
            "hashing",
            "validate",
            "validation",
            "integrity",
            "verify",
            "blake3",
            "sha256",
            "checksum",
            "corrupt",
            "consistency",
            "invariant",
            "assertion",
        ],
    ),
    (
        "Performance constraints",
        &[
            "performance",
            "latency",
            "throughput",
            "cache",
            "caching",
            "memory",
            "speed",
            "budget",
            "target",
            "millisecond",
            "benchmark",
            "optimize",
            "timeout",
            "leak",
            "allocation",
            "slow",
        ],
    ),
    (
        "External interfaces & APIs",
        &[
            "api",
            "interface",
            "endpoint",
            "protocol",
            "http",
            "rpc",
            "mcp",
            "rest",
            "grpc",
            "websocket",
            "boundary",
            "stdio",
        ],
    ),
    (
        "Storage & persistence",
        &[
            "storage",
            "file",
            "disk",
            "persist",
            "persistence",
            "write",
            "read",
            "database",
            "redis",
            "save",
            "load",
            "filesystem",
        ],
    ),
    (
        "Data format & serialization",
        &[
            "format",
            "toml",
            "json",
            "yaml",
            "serialize",
            "deserialize",
            "schema",
            "encoding",
            "parse",
            "marshal",
            "protobuf",
        ],
    ),
    (
        "Dependencies & coupling",
        &[
            "dependency",
            "dependencies",
            "crate",
            "library",
            "coupling",
            "vendor",
            "package",
            "module",
        ],
    ),
    (
        "Migration & versioning",
        &[
            "migration",
            "migrate",
            "version",
            "versioning",
            "upgrade",
            "backward",
            "compatibility",
            "breaking",
        ],
    ),
];

// ── Matching ──────────────────────────────────────────────────────────────

/// Extract lowercased words from a decision for keyword matching.
fn decision_words(dec: &DecisionFile) -> Vec<String> {
    let text = format!(
        "{} {} {}",
        dec.decision.choice,
        dec.decision.reason,
        dec.decision.tags.join(" "),
    );
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .collect()
}

fn words_match_keywords(words: &[String], keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| words.iter().any(|w| w == kw))
}

/// Check if a decision's content matches any keyword for a concern area.
///
/// Uses word-boundary matching to avoid substring false positives
/// (e.g. "format" does not match inside "information").
#[cfg(test)]
pub fn decision_covers_concern(dec: &DecisionFile, keywords: &[&str]) -> bool {
    words_match_keywords(&decision_words(dec), keywords)
}

// ── Coverage computation ──────────────────────────────────────────────────

/// Structured concern coverage for a set of decisions.
///
/// Returns `(covered, uncovered)` concern names. Used by `advance`,
/// `get_context`, and `get_architecture` to surface per-component
/// design gaps.
pub fn compute_concern_coverage(
    decisions: &[&DecisionFile],
) -> (Vec<&'static str>, Vec<&'static str>) {
    let word_sets: Vec<Vec<String>> = decisions.iter().map(|d| decision_words(d)).collect();
    let mut covered = Vec::with_capacity(CONCERNS.len());
    let mut uncovered = Vec::with_capacity(CONCERNS.len());

    for &(name, keywords) in CONCERNS {
        if word_sets
            .iter()
            .any(|words| words_match_keywords(words, keywords))
        {
            covered.push(name);
        } else {
            uncovered.push(name);
        }
    }

    (covered, uncovered)
}

/// Formatted concern status for inclusion in prompts.
///
/// Shows covered areas (with the decision choices that cover them) and
/// uncovered areas (for the agent to systematically explore).
pub fn concern_status(decisions: &[&DecisionFile]) -> String {
    let word_sets: Vec<Vec<String>> = decisions.iter().map(|d| decision_words(d)).collect();
    let mut covered: Vec<(&str, Vec<&str>)> = Vec::with_capacity(CONCERNS.len());
    let mut uncovered: Vec<&str> = Vec::with_capacity(CONCERNS.len());

    for &(concern_name, keywords) in CONCERNS {
        let matching: Vec<&str> = decisions
            .iter()
            .zip(&word_sets)
            .filter(|(_, words)| words_match_keywords(words, keywords))
            .map(|(d, _)| d.decision.choice.as_str())
            .collect();

        if matching.is_empty() {
            uncovered.push(concern_name);
        } else {
            covered.push((concern_name, matching));
        }
    }

    let mut out = String::with_capacity(512);

    if !covered.is_empty() {
        out.push_str("COVERED (decisions exist — do not re-ask):\n");
        for (name, choices) in &covered {
            out.push_str(&format!("  ✓ {name}: \"{}\"\n", choices.join("\", \"")));
        }
        out.push('\n');
    }

    if !uncovered.is_empty() {
        out.push_str("UNCOVERED (systematically ask about each):\n");
        for name in &uncovered {
            out.push_str(&format!("  □ {name}\n"));
        }
        out.push('\n');
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{Attribution, Decision};
    use chrono::{TimeZone, Utc};

    fn make_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        }
    }

    #[test]
    fn security_keywords_match() {
        let dec = make_decision(
            "auth",
            "JWT with DPoP binding",
            "Token security",
            &["security"],
        );
        let security_kw = CONCERNS
            .iter()
            .find(|(name, _)| *name == "Security boundaries")
            .map(|(_, kw)| *kw)
            .unwrap();
        assert!(decision_covers_concern(&dec, security_kw));
    }

    #[test]
    fn no_false_positives_across_concerns() {
        let dec = make_decision("auth", "JWT tokens", "Stateless", &[]);
        let concurrency_kw = CONCERNS
            .iter()
            .find(|(name, _)| *name == "Concurrency & locking")
            .map(|(_, kw)| *kw)
            .unwrap();
        assert!(!decision_covers_concern(&dec, concurrency_kw));
    }

    #[test]
    fn coverage_partitions_all_concerns() {
        let security_dec = make_decision("auth", "JWT with DPoP", "Token security", &["security"]);
        let error_dec = make_decision(
            "project",
            "Result<T, AppError>",
            "Consistent error propagation",
            &[],
        );
        let (covered, uncovered) = compute_concern_coverage(&[&security_dec, &error_dec]);

        assert!(covered.contains(&"Security boundaries"));
        assert!(covered.contains(&"Error handling & failure modes"));
        assert!(uncovered.contains(&"Concurrency & locking"));
        assert_eq!(covered.len() + uncovered.len(), CONCERNS.len());
    }

    #[test]
    fn concern_status_shows_both_sections() {
        let dec = make_decision("store", "BLAKE3 content hashing", "Fast integrity", &[]);
        let output = concern_status(&[&dec]);

        assert!(output.contains("COVERED"));
        assert!(output.contains("Integrity"));
        assert!(output.contains("UNCOVERED"));
        assert!(output.contains("Concurrency"));
    }

    #[test]
    fn empty_decisions_all_uncovered() {
        let (covered, uncovered) = compute_concern_coverage(&[]);
        assert!(covered.is_empty());
        assert_eq!(uncovered.len(), CONCERNS.len());
    }

    #[test]
    fn word_boundary_prevents_substring_match() {
        // "format" in "information" must not match Data format concern.
        let dec = make_decision("api", "Return information payload", "User data", &[]);
        let format_kw = CONCERNS
            .iter()
            .find(|(name, _)| *name == "Data format & serialization")
            .map(|(_, kw)| *kw)
            .unwrap();
        // "information" splits into ["information"] — not "format".
        assert!(!decision_covers_concern(&dec, format_kw));
    }

    // ── Expanded keyword coverage ─────────────────────────────────────

    /// Helper: find keywords for a concern area by substring match on name.
    fn keywords_for(concern_substr: &str) -> &'static [&'static str] {
        CONCERNS
            .iter()
            .find(|(name, _)| name.contains(concern_substr))
            .map(|(_, kw)| *kw)
            .unwrap_or_else(|| panic!("no concern area containing '{concern_substr}'"))
    }

    #[test]
    fn expanded_security_keywords() {
        let kw = keywords_for("Security");
        for term in &["sanitize", "injection", "vulnerability"] {
            let dec = make_decision("web", term, "Security fix", &[]);
            assert!(
                decision_covers_concern(&dec, kw),
                "'{term}' should match Security boundaries"
            );
        }
    }

    #[test]
    fn expanded_error_handling_keywords() {
        let kw = keywords_for("Error handling");
        for term in &["timeout", "overflow", "abort", "exception", "hang"] {
            let dec = make_decision("api", term, "Error fix", &[]);
            assert!(
                decision_covers_concern(&dec, kw),
                "'{term}' should match Error handling"
            );
        }
    }

    #[test]
    fn expanded_concurrency_keywords() {
        let kw = keywords_for("Concurrency");
        for term in &["channel", "contention"] {
            let dec = make_decision("bus", term, "Concurrency fix", &[]);
            assert!(
                decision_covers_concern(&dec, kw),
                "'{term}' should match Concurrency"
            );
        }
    }

    #[test]
    fn expanded_integrity_keywords() {
        let kw = keywords_for("Integrity");
        for term in &["invariant", "assertion"] {
            let dec = make_decision("store", term, "Integrity fix", &[]);
            assert!(
                decision_covers_concern(&dec, kw),
                "'{term}' should match Integrity"
            );
        }
    }

    #[test]
    fn expanded_performance_keywords() {
        let kw = keywords_for("Performance");
        for term in &["timeout", "leak", "allocation", "slow"] {
            let dec = make_decision("api", term, "Performance fix", &[]);
            assert!(
                decision_covers_concern(&dec, kw),
                "'{term}' should match Performance"
            );
        }
    }

    #[test]
    fn timeout_matches_both_error_and_performance() {
        let dec = make_decision("api", "Request timeout handling", "Timeout fix", &[]);
        let error_kw = keywords_for("Error handling");
        let perf_kw = keywords_for("Performance");
        assert!(decision_covers_concern(&dec, error_kw));
        assert!(decision_covers_concern(&dec, perf_kw));
    }

    #[test]
    fn expanded_keywords_no_cross_contamination() {
        // "injection" is Security, not Concurrency.
        let dec = make_decision("web", "SQL injection", "Security", &[]);
        let concurrency_kw = keywords_for("Concurrency");
        assert!(!decision_covers_concern(&dec, concurrency_kw));

        // "channel" is Concurrency, not Security.
        let dec = make_decision("bus", "Message channel", "Messaging", &[]);
        let security_kw = keywords_for("Security");
        assert!(!decision_covers_concern(&dec, security_kw));
    }
}
