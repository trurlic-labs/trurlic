//! Named sites in the store's lock-holding paths and the MCP server's tool
//! calls, for crash-consistency and failure-mode tests.
//!
//! With the `failpoints` feature, `TRURLIC_FAILPOINT=<site>:<n>` acts the
//! `n`-th time execution reaches `<site>`: at a [`hit`] the process aborts,
//! at a [`fail`] the call returns an injected I/O error, at a [`panic()`] the
//! thread panics, and at a [`triggers`] site the caller acts in its place.
//! Without the feature the first three are empty inline functions, the
//! last does not exist, and release builds carry no trace of them. The
//! feature is enabled only in the test job.
//!
//! `abort` rather than `panic` or `exit`: a crash skips destructors and
//! buffered writes, which is the state the tests need to reproduce.
//!
//! With `TRURLIC_FAILPOINT_PAUSE=<file>` also set, a hit pauses instead:
//! it creates `<file>` and waits until the test deletes it, which lets a
//! test act while the process holds whatever lock the site runs under.

#[cfg(feature = "failpoints")]
use crate::console::diag;

/// A point where a test may abort or pause the process, fail the call,
/// panic the thread, or have the caller act in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    /// Files staged and flushed; no journal yet.
    Staged,
    /// Under `shared`, the staged files written to the graph's repository
    /// as blobs; no journal yet.
    Blobs,
    /// Journal in place, so the commit lands; nothing applied yet.
    Journaled,
    /// Before each rename of a staged file; a [`fail`] site.
    Rename,
    /// After each journal entry applied: the `n`-th hit follows the `n`-th
    /// rename, then the removals count on.
    Applied,
    /// The write's files in place and flushed; its history commit, under
    /// `shared`, not made.
    Placed,
    /// An index of the graph's repository set for a commit not made yet:
    /// the hand edits staged before the commit point, or the write's
    /// history commit staged after it.
    HistoryStaged,
    /// The history commit made; the journal not deleted.
    HistoryCommitted,
    /// The history commit failed, and the journal is narrowed to it.
    Narrowed,
    /// The deletion of the journal of a finished write; a [`fail`] site.
    Forget,
    /// The journal deleted, before the flush of its directory; a [`fail`]
    /// site.
    Forgotten,
    /// The journal deleted and its deletion flushed; the lock still held.
    Finished,
    /// A graph loaded under the shared or the exclusive lock, before its
    /// caller uses it; a [`panic()`] site.
    Loaded,
    /// The repairs of an op with no `repairs` read from the tree, the
    /// write lock not taken yet.
    RepairsRead,
    /// A tool call about to run; a [`triggers`] site, where the MCP server
    /// sends the client a request of its own instead.
    #[cfg(feature = "failpoints")]
    ServerRequest,
}

// Naming and parsing exist only where a spec can be read: the feature
// build and the unit tests.
#[cfg(any(test, feature = "failpoints"))]
impl Site {
    const ALL: &[Self] = &[
        Self::Staged,
        Self::Blobs,
        Self::Journaled,
        Self::Rename,
        Self::Applied,
        Self::Placed,
        Self::HistoryStaged,
        Self::HistoryCommitted,
        Self::Narrowed,
        Self::Forget,
        Self::Forgotten,
        Self::Finished,
        Self::Loaded,
        Self::RepairsRead,
        #[cfg(feature = "failpoints")]
        Self::ServerRequest,
    ];

    /// The name used in `TRURLIC_FAILPOINT`.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "commit.staged",
            Self::Blobs => "history.blobs",
            Self::Journaled => "commit.journaled",
            Self::Rename => "commit.rename",
            Self::Applied => "commit.applied",
            Self::Placed => "commit.placed",
            Self::HistoryStaged => "history.staged",
            Self::HistoryCommitted => "history.committed",
            Self::Narrowed => "commit.narrowed",
            Self::Forget => "commit.forget",
            Self::Forgotten => "commit.forgotten",
            Self::Finished => "commit.finished",
            Self::Loaded => "graph.loaded",
            Self::RepairsRead => "apply.repairs_read",
            #[cfg(feature = "failpoints")]
            Self::ServerRequest => "mcp.server_request",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|site| site.as_str() == name)
    }
}

/// Parse a `<site>:<n>` spec. `n` counts hits from 1.
#[cfg(any(test, feature = "failpoints"))]
fn parse_spec(spec: &str) -> Option<(Site, u64)> {
    let (name, n) = spec.rsplit_once(':')?;
    let n: u64 = n.parse().ok().filter(|&n| n >= 1)?;
    Some((Site::from_name(name)?, n))
}

/// `Some(n)` when `TRURLIC_FAILPOINT` names this site and this is its
/// `n`-th hit.
#[cfg(feature = "failpoints")]
fn reached(site: Site) -> Option<u64> {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static ARMED: OnceLock<Option<(Site, u64)>> = OnceLock::new();
    static HITS: AtomicU64 = AtomicU64::new(0);

    let armed = ARMED.get_or_init(|| {
        let spec = std::env::var("TRURLIC_FAILPOINT").ok()?;
        let parsed = parse_spec(&spec);
        if parsed.is_none() {
            diag!("trurlic: ignoring malformed TRURLIC_FAILPOINT={spec:?}");
        }
        parsed
    });
    let (armed_site, n) = (*armed)?;
    (armed_site == site && HITS.fetch_add(1, Ordering::SeqCst) + 1 == n).then_some(n)
}

/// Abort or pause here if this is the armed hit.
#[cfg(feature = "failpoints")]
pub(crate) fn hit(site: Site) {
    let Some(n) = reached(site) else {
        return;
    };
    match std::env::var_os("TRURLIC_FAILPOINT_PAUSE") {
        Some(marker) => pause(site, n, std::path::Path::new(&marker)),
        None => {
            diag!("trurlic: failpoint {}:{n} hit, aborting", site.as_str());
            std::process::abort();
        }
    }
}

/// Create `marker`, then wait until it is gone. A marker that cannot be
/// created aborts, so the test fails instead of waiting for it forever.
#[cfg(feature = "failpoints")]
fn pause(site: Site, n: u64, marker: &std::path::Path) {
    diag!("trurlic: failpoint {}:{n} hit, pausing", site.as_str());
    if let Err(e) = std::fs::write(marker, b"") {
        diag!("trurlic: cannot create {}: {e}", marker.display());
        std::process::abort();
    }
    while marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Return an injected I/O error here if this is the armed hit.
#[cfg(feature = "failpoints")]
pub(super) fn fail(site: Site) -> std::io::Result<()> {
    match reached(site) {
        Some(n) => {
            diag!("trurlic: failpoint {}:{n} hit, failing", site.as_str());
            Err(std::io::Error::other(format!(
                "injected failure at {}:{n}",
                site.as_str()
            )))
        }
        None => Ok(()),
    }
}

/// Panic here if this is the armed hit.
#[cfg(feature = "failpoints")]
pub(super) fn panic(site: Site) {
    if let Some(n) = reached(site) {
        #[expect(
            clippy::panic,
            reason = "a panic site exists to prove the unwind boundary catches one"
        )]
        {
            panic!("failpoint {}:{n} hit, panicking", site.as_str());
        }
    }
}

/// `true` if this is the armed hit, where the caller does what the site
/// stands for instead of its usual work.
#[cfg(feature = "failpoints")]
pub(crate) fn triggers(site: Site) -> bool {
    let reached = reached(site);
    if let Some(n) = reached {
        diag!("trurlic: failpoint {}:{n} hit, triggering", site.as_str());
    }
    reached.is_some()
}

/// No-op without the `failpoints` feature.
#[cfg(not(feature = "failpoints"))]
#[inline]
pub(crate) fn hit(_site: Site) {}

/// No-op without the `failpoints` feature.
#[cfg(not(feature = "failpoints"))]
#[inline]
pub(super) fn panic(_site: Site) {}

/// No-op without the `failpoints` feature.
#[cfg(not(feature = "failpoints"))]
#[inline]
pub(super) fn fail(_site: Site) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_site_name_round_trips() {
        for &site in Site::ALL {
            assert_eq!(Site::from_name(site.as_str()), Some(site));
        }
    }

    #[test]
    fn parse_spec_accepts_site_and_hit_count() {
        assert_eq!(parse_spec("commit.applied:2"), Some((Site::Applied, 2)));
    }

    #[test]
    fn parse_spec_rejects_unknown_site_zero_and_missing_count() {
        assert_eq!(parse_spec("commit.nowhere:1"), None);
        assert_eq!(parse_spec("commit.applied:0"), None);
        assert_eq!(parse_spec("commit.applied"), None);
        assert_eq!(parse_spec("commit.applied:x"), None);
    }
}
