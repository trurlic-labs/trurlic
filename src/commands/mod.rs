mod component;
mod decision;
mod design;
mod init;
mod query;
mod serve;

pub use component::{add_component, add_connection, remove_component, rename_component};
pub use decision::{decide, remove_decision};
pub use design::design;
pub use init::init;
pub use query::{check, status};
pub use serve::serve;

use std::path::Path;

use crate::store::{self, DecisionFile, ProjectState, Store};
use crate::{Error, Result};

// ── Helpers ──────────────────────────────────────────────────────────────────

pub(crate) fn discover_store(cwd: &Path) -> Result<Store> {
    let store = Store::discover(cwd)?;
    store.check_version()?;
    let stale = store.clean_stale_tmp()?;
    if stale > 0 {
        eprintln!("warning: cleaned {stale} stale temp file(s) from interrupted write");
    }
    Ok(store)
}

pub(crate) fn warn_on_issues(state: &ProjectState) {
    let issues = state.validate();
    if !issues.is_empty() {
        eprintln!(
            "warning: .trurl/ has {} consistency issue(s) — run `trurl check` for details",
            issues.len()
        );
    }
}

pub(crate) fn open_store(cwd: &Path) -> Result<(Store, ProjectState)> {
    let store = discover_store(cwd)?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, state))
}

pub(crate) fn open_store_mut(cwd: &Path) -> Result<(Store, store::StoreLock, ProjectState)> {
    let store = discover_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, lock, state))
}

pub(crate) fn validate_mutation(state: &ProjectState) -> Result<()> {
    let issues = state.validate();
    if issues.is_empty() {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "operation would create inconsistent state: {}",
            issues.join("; ")
        )))
    }
}

const MAX_SLUG_LEN: usize = 60;

pub(crate) fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut prev_hyphen = true;

    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            slug.push('-');
            prev_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        if let Some(last_hyphen) = slug.rfind('-') {
            slug.truncate(last_hyphen);
        }
        while slug.ends_with('-') {
            slug.pop();
        }
    }

    if slug.is_empty() {
        slug.push_str("decision");
    }

    slug
}

const MAX_DEDUP_SUFFIX: u32 = 10_000;

pub(crate) fn unique_decision_stem(
    decisions: &std::collections::BTreeMap<String, DecisionFile>,
    base: &str,
) -> Result<String> {
    if !decisions.contains_key(base) {
        return Ok(base.to_string());
    }
    for n in 2..=MAX_DEDUP_SUFFIX {
        let candidate = format!("{base}-{n}");
        if !decisions.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Validation(format!(
        "too many decisions with stem `{base}` (limit: {MAX_DEDUP_SUFFIX})"
    )))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Use Redis"), "use-redis");
        assert_eq!(slugify("JWT with DPoP binding"), "jwt-with-dpop-binding");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Result<T, AppError>"), "result-t-apperror");
        assert_eq!(
            slugify("429 + retry-after header"),
            "429-retry-after-header"
        );
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("one   two---three"), "one-two-three");
        assert_eq!(slugify("---leading"), "leading");
        assert_eq!(slugify("trailing---"), "trailing");
    }

    #[test]
    fn slugify_truncates_at_word_boundary() {
        let long = "a ".repeat(100);
        let slug = slugify(&long);
        assert!(slug.len() <= MAX_SLUG_LEN);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "decision");
        assert_eq!(slugify("!!!"), "decision");
    }

    #[test]
    fn unique_stem_no_collision() {
        let decisions = std::collections::BTreeMap::new();
        assert_eq!(
            unique_decision_stem(&decisions, "use-redis").unwrap(),
            "use-redis"
        );
    }

    #[test]
    fn unique_stem_appends_suffix_on_collision() {
        let mut decisions = std::collections::BTreeMap::new();
        decisions.insert(
            "use-redis".into(),
            crate::store::testing::sample_decision("use-redis", "project"),
        );
        assert_eq!(
            unique_decision_stem(&decisions, "use-redis").unwrap(),
            "use-redis-2"
        );
    }

    #[test]
    fn unique_stem_skips_taken_suffixes() {
        let mut decisions = std::collections::BTreeMap::new();
        for name in &["use-redis", "use-redis-2", "use-redis-3"] {
            decisions.insert(
                name.to_string(),
                crate::store::testing::sample_decision(name, "project"),
            );
        }
        assert_eq!(
            unique_decision_stem(&decisions, "use-redis").unwrap(),
            "use-redis-4"
        );
    }
}
