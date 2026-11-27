//! The paths of a tree: `/`-separated from its code root, the root
//! itself `""`, no segment `.` or `..` and none empty.

/// The last segment of `path`.
pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The directory of `path`; `""` for the code root.
pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The directories above `path`, nearest first, the code root (`""`)
/// last.
pub(super) fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(path);
    std::iter::from_fn(move || {
        let current = rest?;
        rest = (!current.is_empty()).then(|| parent(current));
        rest
    })
}

/// `dir` and `rest` joined by a `/`; `rest` alone at the root.
pub(super) fn join(dir: &str, rest: &str) -> String {
    match dir {
        "" => rest.to_owned(),
        dir => format!("{dir}/{rest}"),
    }
}

/// `relative`, a path as a file writes it, read from `dir`: its `.` and
/// empty segments dropped and each `..` taking the segment before it.
/// `None` when it is absolute or a `..` leaves the tree.
pub(super) fn resolve(dir: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/') {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in dir.split('/').chain(relative.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment => segments.push(segment),
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_resolves_within_the_tree_and_none_leaves_it() {
        assert_eq!(resolve("crates/a", "../b").as_deref(), Some("crates/b"));
        assert_eq!(resolve("", "./x//y/").as_deref(), Some("x/y"));
        assert_eq!(resolve("a", "..").as_deref(), Some(""));
        assert_eq!(resolve("a", "../.."), None);
        assert_eq!(resolve("a", "/abs/b"), None);
    }
}
