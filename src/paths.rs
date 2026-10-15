// Relationships:
// None.

//! Helpers for repository paths, which are always relative to the repository
//! root and always use forward slashes.

/// Directory part of a path, or the empty string when the path has none.
pub(crate) fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Last segment of a path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

/// Splits a file name into its stem and its extension without the dot.
pub(crate) fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// Joins two path fragments, either of which may be empty.
pub(crate) fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else if rest.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// Parent of a directory, or `None` for the repository root.
pub(crate) fn parent(dir: &str) -> Option<&str> {
    if dir.is_empty() {
        None
    } else {
        Some(dir_of(dir))
    }
}

/// Joins `rest` onto `dir` and resolves `.` and `..` segments. Returns `None`
/// when the result would leave the repository.
pub(crate) fn normalize(dir: &str, rest: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for segment in rest.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Non-empty segments of a path.
pub(crate) fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_paths() {
        assert_eq!(dir_of("a/b/c.rs"), "a/b");
        assert_eq!(dir_of("c.rs"), "");
        assert_eq!(file_name("a/b/c.rs"), "c.rs");
        assert_eq!(split_ext("c.test.rs"), ("c.test", "rs"));
        assert_eq!(split_ext("Makefile"), ("Makefile", ""));
        assert_eq!(split_ext(".hidden"), (".hidden", ""));
    }

    #[test]
    fn joins_and_normalizes() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a", ""), "a");
        assert_eq!(join("a", "b"), "a/b");
        assert_eq!(normalize("a/b", "../c/./d.h").as_deref(), Some("a/c/d.h"));
        assert_eq!(normalize("a", "../../x"), None);
        assert_eq!(parent("a/b"), Some("a"));
        assert_eq!(parent("a"), Some(""));
        assert_eq!(parent(""), None);
    }
}
