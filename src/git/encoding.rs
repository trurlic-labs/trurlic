//! Paths as git prints and reads them: bytes, never text.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The NUL-terminated paths of `-z` output.
pub(super) fn paths(stdout: &[u8]) -> Vec<PathBuf> {
    stdout
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(path)
        .collect()
}

/// The one path `rev-parse` prints, with its line break: everything but
/// the last byte, so a line break inside the path stays in it.
pub(super) fn line_path(mut stdout: Vec<u8>) -> PathBuf {
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    path(&stdout)
}

/// A path as git printed its bytes.
#[cfg(unix)]
pub(super) fn path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// A path as git printed its bytes, which git for Windows writes as UTF-8.
#[cfg(not(unix))]
pub(super) fn path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The bytes git reads for `path` on its stdin.
#[cfg(unix)]
pub(super) fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

/// The bytes git reads for `path` on its stdin: UTF-8 on Windows.
#[cfg(not(unix))]
pub(super) fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_path_drops_only_the_last_line_break() {
        assert_eq!(line_path(b"/a\nb\n".to_vec()), PathBuf::from("/a\nb"));
        assert_eq!(line_path(b"/a".to_vec()), PathBuf::from("/a"));
    }

    #[test]
    fn minus_z_output_splits_on_nul_only() {
        assert_eq!(
            paths(b"a\0b c\0d\ne\0"),
            [
                PathBuf::from("a"),
                PathBuf::from("b c"),
                PathBuf::from("d\ne")
            ]
        );
        assert_eq!(paths(b""), Vec::<PathBuf>::new());
    }
}
