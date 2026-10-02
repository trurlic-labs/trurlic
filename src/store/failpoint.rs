//! Named abort sites for crash-consistency tests.
//!
//! With the `failpoints` feature, `TRURLIC_FAILPOINT=<site>:<n>` aborts the
//! process the `n`-th time execution reaches `<site>`. Without the feature,
//! [`hit`] is an empty inline function and release builds carry no trace of
//! it. The feature is enabled only in the test job.
//!
//! `abort` rather than `panic` or `exit`: a crash skips destructors and
//! buffered writes, which is the state the tests need to reproduce.
//!
//! With `TRURLIC_FAILPOINT_PAUSE=<file>` also set, the hit pauses instead:
//! it creates `<file>` and waits until the test deletes it, which lets a
//! test act while the process holds whatever lock the site runs under.

/// A point in the store's lock-holding paths where a test may abort or
/// pause the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    /// Temp files written and verified, generation raised; nothing renamed yet.
    Staged,
    /// Node files renamed into place; `graph.toml` (the commit point) not yet.
    NodesRenamed,
    /// `graph.toml` renamed; removed nodes' files still on disk.
    GraphRenamed,
    /// A watcher holds the shared lock and has read nothing yet.
    WatcherReload,
}

// Naming and parsing exist only where a spec can be read: the feature
// build and the unit tests.
#[cfg(any(test, feature = "failpoints"))]
impl Site {
    const ALL: [Self; 4] = [
        Self::Staged,
        Self::NodesRenamed,
        Self::GraphRenamed,
        Self::WatcherReload,
    ];

    /// The name used in `TRURLIC_FAILPOINT`.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "commit.staged",
            Self::NodesRenamed => "commit.nodes_renamed",
            Self::GraphRenamed => "commit.graph_renamed",
            Self::WatcherReload => "watcher.reload",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|site| site.as_str() == name)
    }
}

/// Parse a `<site>:<n>` spec. `n` counts hits from 1.
#[cfg(any(test, feature = "failpoints"))]
fn parse_spec(spec: &str) -> Option<(Site, u64)> {
    let (name, n) = spec.rsplit_once(':')?;
    let n: u64 = n.parse().ok().filter(|&n| n >= 1)?;
    Some((Site::from_name(name)?, n))
}

/// Abort or pause here if `TRURLIC_FAILPOINT` names this site and this is
/// its `n`-th hit.
#[cfg(feature = "failpoints")]
pub(crate) fn hit(site: Site) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static ARMED: OnceLock<Option<(Site, u64)>> = OnceLock::new();
    static HITS: AtomicU64 = AtomicU64::new(0);

    let armed = ARMED.get_or_init(|| {
        let spec = std::env::var("TRURLIC_FAILPOINT").ok()?;
        let parsed = parse_spec(&spec);
        if parsed.is_none() {
            eprintln!("trurlic: ignoring malformed TRURLIC_FAILPOINT={spec:?}");
        }
        parsed
    });
    let Some((armed_site, n)) = *armed else {
        return;
    };
    if armed_site != site || HITS.fetch_add(1, Ordering::SeqCst) + 1 != n {
        return;
    }
    match std::env::var_os("TRURLIC_FAILPOINT_PAUSE") {
        Some(marker) => pause(site, n, std::path::Path::new(&marker)),
        None => {
            eprintln!("trurlic: failpoint {}:{n} hit, aborting", site.as_str());
            std::process::abort();
        }
    }
}

/// Create `marker`, then wait until it is gone. A marker that cannot be
/// created aborts, so the test fails instead of waiting for it forever.
#[cfg(feature = "failpoints")]
fn pause(site: Site, n: u64, marker: &std::path::Path) {
    eprintln!("trurlic: failpoint {}:{n} hit, pausing", site.as_str());
    if let Err(e) = std::fs::write(marker, b"") {
        eprintln!("trurlic: cannot create {}: {e}", marker.display());
        std::process::abort();
    }
    while marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// No-op without the `failpoints` feature.
#[cfg(not(feature = "failpoints"))]
#[inline]
pub(crate) fn hit(_site: Site) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_site_name_round_trips() {
        for site in Site::ALL {
            assert_eq!(Site::from_name(site.as_str()), Some(site));
        }
    }

    #[test]
    fn parse_spec_accepts_site_and_hit_count() {
        assert_eq!(
            parse_spec("commit.nodes_renamed:2"),
            Some((Site::NodesRenamed, 2))
        );
    }

    #[test]
    fn parse_spec_rejects_unknown_site_zero_and_missing_count() {
        assert_eq!(parse_spec("commit.nowhere:1"), None);
        assert_eq!(parse_spec("commit.nodes_renamed:0"), None);
        assert_eq!(parse_spec("commit.nodes_renamed"), None);
        assert_eq!(parse_spec("commit.nodes_renamed:x"), None);
    }
}
