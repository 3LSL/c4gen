// Relationships:
// - `external:source-repository`: Reads the directory tree and source files
// - `src/error.rs`: Reports unreadable paths and missing repository roots
// - `src/paths.rs`: Splits and joins repository paths

//! The scanned repository: every source file in a supported language, with
//! its language, its role and its text.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::paths::{dir_of, file_name, join, split_ext};

/// Index of a file in [`Repo::files`].
pub(crate) type FileId = usize;

/// Language family of a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Language {
    Rust,
    Python,
    /// C and C++, which share comment syntax and `#include`.
    C,
}

/// Whether a file is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// Program code, which becomes a component.
    Component,
    /// Test code, which is outside the diagrams.
    Test,
    /// A build script, which is outside the diagrams.
    Build,
}

pub(crate) struct SourceFile {
    /// Path relative to the repository root, with forward slashes.
    pub path: String,
    pub language: Language,
    pub role: Role,
    pub text: String,
}

pub(crate) struct Repo {
    pub root: PathBuf,
    /// Source files sorted by path.
    pub files: Vec<SourceFile>,
    pub by_path: BTreeMap<String, FileId>,
    /// Path and text of every `Cargo.toml`, sorted by path.
    pub manifests: Vec<(String, String)>,
    excludes: Excludes,
}

/// Where a module sits in its repository.
pub(crate) struct Location {
    pub root: PathBuf,
    /// Module path relative to the root; empty for the root itself.
    pub module: String,
    pub is_file: bool,
}

const TEST_DIRS: &[&str] = &["tests", "test", "benches", "__tests__"];
const ROOT_MARKERS: &[&str] = &[".git", ".hg", ".svn"];
const C_HEADER_EXTS: &[&str] = &["h", "hh", "hpp", "hxx", "h++"];
const CPP_ONLY_EXTS: &[&str] = &[
    "cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++", "ipp", "tpp", "inl", "tcc",
];

/// Resolves the module path and the repository root to absolute paths and
/// expresses the module relative to the root.
pub(crate) fn locate(module: &Path, root: Option<&Path>) -> Result<Location, Error> {
    let canonical = |path: &Path| {
        fs::canonicalize(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    };
    let module_abs = canonical(module)?;
    let root_abs = match root {
        Some(root) => canonical(root)?,
        None => find_root(&module_abs).ok_or_else(|| Error::RootNotFound {
            module: module_abs.clone(),
        })?,
    };
    let relative = module_abs
        .strip_prefix(&root_abs)
        .map_err(|_| Error::OutsideRoot {
            module: module_abs.clone(),
            root: root_abs.clone(),
        })?;
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok(Location {
        is_file: module_abs.is_file(),
        module: parts.join("/"),
        root: root_abs,
    })
}

fn find_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(dir) = current {
        if ROOT_MARKERS.iter().any(|marker| dir.join(marker).exists()) {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

impl Repo {
    /// Reads every source file under `root`, visiting directories in sorted
    /// order so the result does not depend on the file system's order.
    pub(crate) fn scan(root: &Path, exclude: &[String]) -> Result<Repo, Error> {
        let excludes = Excludes::new(exclude);
        let mut found = Vec::new();
        let mut manifest_paths = Vec::new();
        walk(root, "", &excludes, &mut found, &mut manifest_paths)?;
        found.sort();
        manifest_paths.sort();

        let mut manifests = Vec::new();
        for path in manifest_paths {
            let text = read(root, &path)?;
            manifests.push((path, text));
        }
        let manifest_dirs: BTreeSet<&str> = manifests.iter().map(|(p, _)| dir_of(p)).collect();

        let mut files = Vec::with_capacity(found.len());
        let mut by_path = BTreeMap::new();
        for (path, language) in found {
            let text = read(root, &path)?;
            let role = role_of(&path, language, &manifest_dirs);
            by_path.insert(path.clone(), files.len());
            files.push(SourceFile {
                path,
                language,
                role,
                text,
            });
        }
        Ok(Repo {
            root: root.to_path_buf(),
            files,
            by_path,
            manifests,
            excludes,
        })
    }

    /// Looks up a path that is not one of the scanned source files.
    pub(crate) fn other_file(&self, path: &str) -> OtherFile {
        // Walk down the path the way the scan does, so that anything the
        // scan skipped counts as skipped here too.
        let mut walked = String::new();
        for name in path.split('/') {
            walked = join(&walked, name);
            if name.starts_with('.') || self.excludes.matches(name, &walked) {
                return OtherFile::Skipped;
            }
        }
        if self.root.join(path).is_file() {
            OtherFile::Present
        } else {
            OtherFile::Missing
        }
    }
}

/// What a path that is not a scanned source file turns out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OtherFile {
    /// A file in a language the scan does not read.
    Present,
    /// Hidden or excluded, so left out of diagrams.
    Skipped,
    Missing,
}

struct Excludes {
    names: BTreeSet<String>,
    paths: BTreeSet<String>,
}

impl Excludes {
    fn new(entries: &[String]) -> Self {
        let mut names = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for entry in entries {
            let entry = entry.trim().trim_matches('/').replace('\\', "/");
            if entry.is_empty() {
                continue;
            }
            if entry.contains('/') {
                paths.insert(entry);
            } else {
                names.insert(entry);
            }
        }
        Excludes { names, paths }
    }

    fn matches(&self, name: &str, path: &str) -> bool {
        self.names.contains(name) || self.paths.contains(path)
    }
}

fn walk(
    root: &Path,
    dir: &str,
    excludes: &Excludes,
    found: &mut Vec<(String, Language)>,
    manifests: &mut Vec<String>,
) -> Result<(), Error> {
    let abs = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let io_error = |source| Error::Io {
        path: abs.clone(),
        source,
    };
    let mut entries: Vec<(String, bool)> = Vec::new();
    for entry in fs::read_dir(&abs).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        // Names that are not valid UTF-8 cannot be written as a target, and
        // symbolic links could make the walk loop or visit a file twice.
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_symlink() {
            continue;
        }
        entries.push((name, file_type.is_dir()));
    }
    entries.sort();

    for (name, is_dir) in entries {
        if name.starts_with('.') {
            continue;
        }
        let path = join(dir, &name);
        if excludes.matches(&name, &path) {
            continue;
        }
        if is_dir {
            walk(root, &path, excludes, found, manifests)?;
        } else if name == "Cargo.toml" {
            manifests.push(path);
        } else if let Some(language) = language_of(&name) {
            found.push((path, language));
        }
    }
    Ok(())
}

fn read(root: &Path, path: &str) -> Result<String, Error> {
    let abs = root.join(path);
    let bytes = fs::read(&abs).map_err(|source| Error::Io {
        path: abs.clone(),
        source,
    })?;
    // A byte-order mark would otherwise hide a block on the first line.
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

fn language_of(name: &str) -> Option<Language> {
    let (_, ext) = split_ext(name);
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some(Language::Rust),
        "py" => Some(Language::Python),
        "c" | "h" | "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ipp" | "tpp"
        | "inl" | "tcc" => Some(Language::C),
        _ => None,
    }
}

fn role_of(path: &str, language: Language, manifest_dirs: &BTreeSet<&str>) -> Role {
    let dir = dir_of(path);
    if dir.split('/').any(|segment| TEST_DIRS.contains(&segment)) {
        return Role::Test;
    }
    let name = file_name(path);
    let stem = split_ext(name).0.to_ascii_lowercase();
    let test_name =
        stem.starts_with("test_") || stem.ends_with("_test") || stem.ends_with("_tests");
    match language {
        Language::Rust => {
            if name == "build.rs" && manifest_dirs.contains(dir) {
                return Role::Build;
            }
            if test_name || stem == "tests" || stem == "test" {
                return Role::Test;
            }
        }
        Language::Python => {
            if name == "setup.py" {
                return Role::Build;
            }
            if test_name || name == "conftest.py" {
                return Role::Test;
            }
        }
        Language::C => {
            if test_name || stem.ends_with("_unittest") {
                return Role::Test;
            }
        }
    }
    Role::Component
}

/// Whether a C or C++ file is a header, the declaration half of a pair.
pub(crate) fn is_c_header(path: &str) -> bool {
    let ext = split_ext(file_name(path)).1.to_ascii_lowercase();
    C_HEADER_EXTS.contains(&ext.as_str())
}

/// Name of the language of a component made of the given files.
pub(crate) fn technology(language: Language, paths: &[&str]) -> &'static str {
    match language {
        Language::Rust => "Rust",
        Language::Python => "Python",
        Language::C => {
            let exts: Vec<String> = paths
                .iter()
                .map(|p| split_ext(file_name(p)).1.to_ascii_lowercase())
                .collect();
            if exts.iter().any(|e| CPP_ONLY_EXTS.contains(&e.as_str())) {
                "C++"
            } else if exts.iter().any(|e| e == "c") {
                "C"
            } else {
                "C/C++"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_languages() {
        assert_eq!(language_of("a.rs"), Some(Language::Rust));
        assert_eq!(language_of("a.py"), Some(Language::Python));
        assert_eq!(language_of("a.HPP"), Some(Language::C));
        assert_eq!(language_of("a.ts"), None);
        assert_eq!(language_of("Makefile"), None);
    }

    #[test]
    fn classifies_roles() {
        let manifests: BTreeSet<&str> = ["", "tool"].into_iter().collect();
        let role = |path, language| role_of(path, language, &manifests);
        assert_eq!(role("src/a.rs", Language::Rust), Role::Component);
        assert_eq!(role("tests/a.rs", Language::Rust), Role::Test);
        assert_eq!(role("src/a_test.rs", Language::Rust), Role::Test);
        assert_eq!(role("build.rs", Language::Rust), Role::Build);
        assert_eq!(role("tool/build.rs", Language::Rust), Role::Build);
        assert_eq!(role("src/build.rs", Language::Rust), Role::Component);
        assert_eq!(role("app/test_login.py", Language::Python), Role::Test);
        assert_eq!(role("app/conftest.py", Language::Python), Role::Test);
        assert_eq!(role("setup.py", Language::Python), Role::Build);
        assert_eq!(role("src/repo_unittest.cc", Language::C), Role::Test);
        assert_eq!(role("src/contest.cc", Language::C), Role::Component);
    }

    #[test]
    fn names_technologies() {
        assert_eq!(technology(Language::C, &["a.h", "a.cpp"]), "C++");
        assert_eq!(technology(Language::C, &["a.h", "a.c"]), "C");
        assert_eq!(technology(Language::C, &["a.h"]), "C/C++");
        assert!(is_c_header("include/a.hpp"));
        assert!(!is_c_header("src/a.cpp"));
    }
}
