//! One source file's bytes and its index: the bounded read of the file, and
//! its index from the cache when an entry holds its key, else built by the
//! indexer's worker. What a read gave is [`Sourced`], which the call's
//! [`Reads`](super::reads::Reads) takes.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

use crate::code::{self, FileKind, FileMacros, Indexed, ReadFault};
use crate::store::SymbolsKey;

use super::cache::{self, Content};
use super::indexer::Worker;
use super::lexical::Cached;

/// The largest source file the shell reads: the largest of any kind the
/// index reads (`FileKind::max_bytes`), which refuses one over its kind's.
pub(super) const MAX_SOURCE_FILE_BYTES: u64 = code::MAX_SOURCE_BYTES as u64;

/// What reading a file gave.
pub(super) enum Sourced {
    Fault(ReadFault),
    /// Its index, and the cache entries it used: the index's and, for a C
    /// or C++ file, its lexical pass's.
    Indexed {
        indexed: Box<Indexed>,
        index: Used,
        lexed: Option<Used>,
    },
}

/// A cache entry a read used: its key, and its bytes to write when the
/// cache held none.
pub(super) struct Used {
    pub(super) key: SymbolsKey,
    pub(super) miss: Option<Vec<u8>>,
}

/// Read the file at `path` under `root`, of `kind`, and index it, from
/// the cache when it holds the index; `None` when the file is gone.
pub(super) fn read_and_index(
    cached: &Cached<'_>,
    worker: &Worker<'_>,
    (root, path, kind): (&Path, &str, FileKind),
) -> Option<Sourced> {
    match read_source(&root.join(path)) {
        Ok(bytes) => Some(index_bytes(cached, worker, (path, kind), &bytes)),
        Err(Gone) => None,
        Err(Faulty(fault)) => Some(Sourced::Fault(fault)),
    }
}

/// The index of `bytes`, the file at `path` of `kind`: the cache's, else
/// built.
pub(super) fn index_bytes(
    cached: &Cached<'_>,
    worker: &Worker<'_>,
    (path, kind): (&str, FileKind),
    bytes: &[u8],
) -> Sourced {
    let content = Content::of(bytes);
    let (macros, lexed) = match kind.blanks_macros() {
        true => {
            let found = cached.lexed_or_lex(content, bytes);
            let lexed = Used {
                key: found.key,
                miss: found.miss,
            };
            (worker.tree().of_file(path, &found.lexed), Some(lexed))
        }
        false => (FileMacros::default(), None),
    };
    let key = cache::key(kind, content, &macros);
    let (indexed, miss) = match cached.index(key, kind, bytes.len()) {
        Some(indexed) => (indexed, None),
        None => match worker.index(kind, bytes, &macros) {
            Ok(indexed) => {
                let miss = Some(cache::encode(key, &indexed));
                (indexed, miss)
            }
            Err(fault) => return Sourced::Fault(fault),
        },
    };
    Sourced::Indexed {
        indexed: Box::new(indexed),
        index: Used { key, miss },
        lexed,
    }
}

/// Why a source file gave no bytes.
pub(super) enum Unread {
    Gone,
    Faulty(ReadFault),
}
use Unread::{Faulty, Gone};

/// The bytes of the file at `path`, symlinks followed: refused unopened
/// when it is no regular file, so a FIFO never blocks the read, and
/// unread past [`MAX_SOURCE_FILE_BYTES`].
pub(super) fn read_source(path: &Path) -> Result<Vec<u8>, Unread> {
    let unread = |e: std::io::Error| match e.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => Gone,
        kind => Faulty(ReadFault::Unreadable(kind)),
    };
    let metadata = fs::metadata(path).map_err(unread)?;
    let too_large = |bytes| {
        Faulty(ReadFault::TooLarge {
            bytes,
            max: MAX_SOURCE_FILE_BYTES,
        })
    };
    if !metadata.is_file() {
        return Err(Faulty(ReadFault::NotAFile));
    }
    if metadata.len() > MAX_SOURCE_FILE_BYTES {
        return Err(too_large(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)
        .map_err(unread)?
        .take(MAX_SOURCE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unread)?;
    match u64::try_from(bytes.len()) {
        Ok(read) if read <= MAX_SOURCE_FILE_BYTES => Ok(bytes),
        Ok(read) => Err(too_large(read)),
        Err(_) => Err(too_large(u64::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_file_gone_before_its_read_is_absent_not_a_fault() {
        let tmp = TempDir::new().unwrap();

        assert!(matches!(
            read_source(&tmp.path().join("gone.rs")),
            Err(Gone)
        ));
        assert!(matches!(
            read_source(&tmp.path().join("gone.rs/x")),
            Err(Gone)
        ));
        assert!(matches!(
            read_source(tmp.path()),
            Err(Faulty(ReadFault::NotAFile))
        ));
    }
}
