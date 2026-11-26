//! The code index's cache, `.state/symbols/`: one file per entry, named by
//! its key, holding bytes the store does not read. The cache is never
//! authoritative, so a read answers a miss for every fault, and nothing
//! in it is flushed: an entry a power loss tears is a miss the next read
//! replaces.
//!
//! A read holds no lock: an entry is written to a file of its own in the
//! directory and renamed over its name, so a reader sees a whole entry or
//! none. Every write and removal runs under the exclusive lock, which a
//! read takes only when it is free ([`Store::try_lock`]), and none uses
//! `.state/tmp/`, which belongs to the commit path: a file seen there
//! makes a reader run recovery.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Error, Result};

use super::{Store, StoreLock};

/// The directory of the cache, in `.state/`.
const SYMBOLS_DIR: &str = "symbols";

/// The entries the directory may hold for each one of the live set a
/// write is given, past [`PRUNE_SLACK`], before the rest are removed.
const PRUNE_FACTOR: usize = 2;

/// The entries outside the live set a directory may hold however small
/// the set is.
const PRUNE_SLACK: usize = 256;

/// The name of an entry: a 32-byte hash, written as 64 lowercase hex
/// digits, so no key can name a path outside the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolsKey([u8; 32]);

impl SymbolsKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The key a file name spells; `None` for any other name.
    fn parse(name: &str) -> Option<Self> {
        let lowercase = !name.bytes().any(|byte| byte.is_ascii_uppercase());
        let hash = blake3::Hash::from_hex(name).ok().filter(|_| lowercase)?;
        Some(Self(*hash.as_bytes()))
    }
}

impl fmt::Display for SymbolsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&blake3::Hash::from_bytes(self.0).to_hex())
    }
}

impl Store {
    fn symbols_dir(&self) -> PathBuf {
        self.state_dir().join(SYMBOLS_DIR)
    }

    /// The bytes of the entry `key` names; `None` when there is none, or
    /// it is no regular file, is over `max_bytes` (the largest entry the
    /// caller's file can have) or cannot be read.
    pub(crate) fn read_symbols(&self, key: SymbolsKey, max_bytes: usize) -> Option<Vec<u8>> {
        let path = self.symbols_dir().join(key.to_string());
        let metadata = fs::metadata(&path).ok()?;
        let cap = max_bytes as u64;
        if !metadata.is_file() || metadata.len() > cap {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
        File::open(&path)
            .ok()?
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= max_bytes).then_some(bytes)
    }

    /// Write each of `entries` over its key's name, and remove everything
    /// that is no entry (a write a crash stopped, a directory); then, when
    /// the directory holds more than [`PRUNE_FACTOR`] times `live` and
    /// [`PRUNE_SLACK`] besides, remove each entry outside `live`. The
    /// growth needs no clock: a call's own live set bounds it. One entry
    /// that fails is passed over and the rest written: the first failure
    /// is returned once all are tried.
    pub(crate) fn write_symbols(
        &self,
        _lock: &StoreLock,
        entries: &BTreeMap<SymbolsKey, Vec<u8>>,
        live: &BTreeSet<SymbolsKey>,
    ) -> Result<()> {
        let dir = self.symbols_dir();
        fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        let mut first = None;
        for (key, bytes) in entries {
            if let Err(error) = self.write_symbol(&dir.join(key.to_string()), bytes) {
                first.get_or_insert(error);
            }
        }
        if let Err(error) = self.prune_symbols(live) {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }

    /// Write `bytes` over `target`, removing whatever is there that is no
    /// file (a directory under an entry's name).
    fn write_symbol(&self, target: &Path, bytes: &[u8]) -> Result<()> {
        if fs::symlink_metadata(target).is_ok_and(|found| found.is_dir()) {
            fs::remove_dir_all(target).map_err(Error::io(target))?;
        }
        let staged = self.stage_symbols(bytes)?;
        fs::rename(&staged, target).map_err(|source| {
            let _ = fs::remove_file(&staged);
            Error::Io {
                path: target.to_path_buf(),
                source,
            }
        })
    }

    /// Write `bytes` to a fresh file in the cache's directory, named
    /// after this process and a counter, which no entry's name can be.
    fn stage_symbols(&self, bytes: &[u8]) -> Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = self
            .symbols_dir()
            .join(format!("tmp-{}-{n}", std::process::id()));
        let written = File::create_new(&path).and_then(|mut file| file.write_all(bytes));
        match written {
            Ok(()) => Ok(path),
            Err(source) => {
                let _ = fs::remove_file(&path);
                Err(Error::Io { path, source })
            }
        }
    }

    fn prune_symbols(&self, live: &BTreeSet<SymbolsKey>) -> Result<()> {
        let dir = self.symbols_dir();
        let mut keys = Vec::new();
        let mut strays = Vec::new();
        for entry in fs::read_dir(&dir).map_err(Error::io(&dir))? {
            let entry = entry.map_err(Error::io(&dir))?;
            match entry.file_name().to_str().and_then(SymbolsKey::parse) {
                Some(key) => keys.push(key),
                None => strays.push(entry.path()),
            }
        }
        let bound = live
            .len()
            .saturating_mul(PRUNE_FACTOR)
            .saturating_add(PRUNE_SLACK);
        if keys.len() > bound {
            let dead = keys.iter().filter(|key| !live.contains(key));
            strays.extend(dead.map(|key| dir.join(key.to_string())));
        }
        let mut first = None;
        for path in strays {
            let removed = match fs::symlink_metadata(&path) {
                Ok(found) if found.is_dir() => fs::remove_dir_all(&path),
                Ok(_) => fs::remove_file(&path),
                Err(e) => Err(e),
            };
            match removed {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(source) => {
                    first.get_or_insert(Error::Io { path, source });
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use tempfile::TempDir;

    fn key(n: u8) -> SymbolsKey {
        SymbolsKey::new([n; 32])
    }

    #[test]
    fn an_entry_reads_back_as_written_and_a_missing_one_is_a_miss() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        store
            .write_symbols(
                &lock,
                &BTreeMap::from([(key(1), b"one".to_vec())]),
                &BTreeSet::from([key(1)]),
            )
            .unwrap();

        assert_eq!(store.read_symbols(key(1), 64), Some(b"one".to_vec()));
        assert_eq!(store.read_symbols(key(2), 64), None);
    }

    #[test]
    fn a_key_spells_only_its_own_name() {
        let spelled = key(0xab).to_string();

        assert_eq!(spelled, "ab".repeat(32));
        assert_eq!(SymbolsKey::parse(&spelled), Some(key(0xab)));
        assert_eq!(SymbolsKey::parse(&spelled.to_uppercase()), None);
        assert_eq!(SymbolsKey::parse(&spelled[1..]), None);
        assert_eq!(SymbolsKey::parse("../../project.toml"), None);
    }

    #[test]
    fn an_entry_that_is_no_file_or_over_the_callers_cap_is_a_miss() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let dir = store.symbols_dir();
        fs::create_dir_all(dir.join(key(1).to_string())).unwrap();
        fs::write(dir.join(key(2).to_string()), [0; 65]).unwrap();

        assert_eq!(store.read_symbols(key(1), 64), None);
        assert_eq!(store.read_symbols(key(2), 64), None);
        assert_eq!(store.read_symbols(key(2), 65), Some(vec![0; 65]));
    }

    #[test]
    fn a_directory_in_the_cache_is_removed_and_stops_no_other_write() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let dir = store.symbols_dir();
        fs::create_dir_all(dir.join(key(1).to_string()).join("inner")).unwrap();
        fs::create_dir_all(dir.join("stray-dir/inner")).unwrap();

        store
            .write_symbols(
                &lock,
                &BTreeMap::from([(key(1), b"one".to_vec()), (key(2), b"two".to_vec())]),
                &BTreeSet::from([key(1), key(2)]),
            )
            .unwrap();

        assert_eq!(store.read_symbols(key(1), 64), Some(b"one".to_vec()));
        assert_eq!(store.read_symbols(key(2), 64), Some(b"two".to_vec()));
        assert!(!dir.join("stray-dir").exists());
    }

    #[test]
    fn a_write_removes_strays_and_prunes_past_the_bound() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let entries = |count: usize| -> BTreeMap<_, _> {
            (0..count)
                .map(|n| {
                    let mut bytes = [0; 32];
                    bytes[..8].copy_from_slice(&n.to_le_bytes());
                    (SymbolsKey::new(bytes), Vec::new())
                })
                .collect()
        };
        let first = SymbolsKey::new([0; 32]);
        let live = BTreeSet::from([first]);
        let count = || fs::read_dir(store.symbols_dir()).unwrap().count();

        // At the bound, twice the live set and the slack: nothing goes but
        // a stopped write's file.
        let at_bound = PRUNE_FACTOR + PRUNE_SLACK;
        fs::create_dir_all(store.symbols_dir()).unwrap();
        fs::write(store.symbols_dir().join("tmp-1-0"), b"stopped").unwrap();
        store
            .write_symbols(&lock, &entries(at_bound), &live)
            .unwrap();
        assert_eq!(count(), at_bound);

        // One past it: every entry outside the live set goes.
        store
            .write_symbols(&lock, &entries(at_bound + 1), &live)
            .unwrap();
        assert_eq!(count(), 1);
        assert_eq!(store.read_symbols(first, 64), Some(Vec::new()));
    }
}
