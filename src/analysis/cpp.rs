// Relationships:
// - `src/analysis/dependencies.rs`: Records the dependencies detected for each file
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads C-family sources and their paths

//! Dependency detection for C and C++: `#include` directives, and the pairing
//! of each implementation file with its header.

use std::collections::{BTreeMap, BTreeSet};

use super::FileDeps;
use crate::paths::{dir_of, file_name, join, normalize, parent, split_ext};
use crate::repo::{is_c_header, FileId, Language, Repo, Role};

/// Directories tried under each ancestor of the including file.
const CONVENTIONAL_DIRS: &[&str] = &["", "include", "src"];

struct Parsed {
    /// Include paths, with `true` for the quoted form.
    includes: Vec<(String, bool)>,
    /// The file consists of preprocessor directives only, so it defines
    /// nothing itself and merely gathers other headers.
    umbrella: bool,
}

/// Fills in the dependencies of every C and C++ file and returns the
/// implementation files paired with their headers.
pub(crate) fn analyze(
    repo: &Repo,
    search_paths: &[String],
    deps: &mut [FileDeps],
) -> BTreeMap<FileId, FileId> {
    let files: Vec<FileId> = (0..repo.files.len())
        .filter(|&f| repo.files[f].language == Language::C)
        .collect();
    let mut by_name: BTreeMap<&str, Vec<FileId>> = BTreeMap::new();
    for &f in &files {
        by_name
            .entry(file_name(&repo.files[f].path))
            .or_default()
            .push(f);
    }

    let mut includes: BTreeMap<FileId, BTreeSet<FileId>> = BTreeMap::new();
    let mut umbrellas = BTreeSet::new();
    for &f in &files {
        let parsed = parse(&repo.files[f].text);
        if parsed.umbrella {
            umbrellas.insert(f);
        }
        let from = &repo.files[f].path;
        let resolved = parsed
            .includes
            .iter()
            .filter_map(|(path, quoted)| resolve(repo, &by_name, search_paths, from, path, *quoted))
            .filter(|&target| target != f)
            .collect();
        includes.insert(f, resolved);
    }

    for &f in &files {
        let file_deps = &mut deps[f];
        file_deps.analyzed = true;
        for &target in &includes[&f] {
            // Everything a header includes is visible to whoever includes it.
            file_deps.imports.insert(target);
            file_deps.open.insert(target);
            file_deps.reexports.insert(target);
            // An umbrella header only passes its includes on, so including
            // something is not a use for it, and including an umbrella is a
            // use of whatever lies behind it.
            if !umbrellas.contains(&f) {
                file_deps.add_use(target, !umbrellas.contains(&target));
            }
        }
    }

    pair(repo, &files, &includes)
}

fn parse(text: &str) -> Parsed {
    let stripped = strip_comments(text);
    let mut includes = Vec::new();
    let mut umbrella = true;
    for line in stripped.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(directive) = line.strip_prefix('#') else {
            umbrella = false;
            continue;
        };
        let Some(rest) = directive.trim_start().strip_prefix("include") else {
            continue;
        };
        let rest = rest.trim_start();
        let (close, quoted) = match rest.chars().next() {
            Some('"') => ('"', true),
            Some('<') => ('>', false),
            _ => continue,
        };
        if let Some(end) = rest[1..].find(close) {
            includes.push((rest[1..1 + end].to_string(), quoted));
        }
    }
    Parsed {
        umbrella: umbrella && !includes.is_empty(),
        includes,
    }
}

/// Replaces comments with spaces, keeping line breaks and string literals.
fn strip_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(chars.len());
            out.push(' ');
        } else if c == '"' || (c == '\'' && !(i > 0 && chars[i - 1].is_ascii_alphanumeric())) {
            // A quote after a digit or letter is a digit separator, not a literal.
            out.push(c);
            i += 1;
            while i < chars.len() {
                let d = chars[i];
                out.push(d);
                i += 1;
                if d == '\\' {
                    if let Some(&escaped) = chars.get(i) {
                        out.push(escaped);
                        i += 1;
                    }
                } else if d == c || d == '\n' {
                    break;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn lookup(repo: &Repo, path: &str) -> Option<FileId> {
    repo.by_path
        .get(path)
        .copied()
        .filter(|&f| repo.files[f].language == Language::C)
}

/// Finds the project file an include names, or `None` for a system or
/// third-party header. Candidates are tried in a fixed order: next to the
/// including file, the configured search paths, the conventional directories
/// of each ancestor, and last a unique match on the full include path.
fn resolve(
    repo: &Repo,
    by_name: &BTreeMap<&str, Vec<FileId>>,
    search_paths: &[String],
    from: &str,
    include: &str,
    quoted: bool,
) -> Option<FileId> {
    let include = include.trim().replace('\\', "/");
    if include.is_empty() || include.starts_with('/') {
        return None;
    }
    let qualified = include.contains('/');
    let from_dir = dir_of(from);
    let candidate = |base: &str| normalize(base, &include).and_then(|path| lookup(repo, &path));

    if quoted {
        if let Some(found) = candidate(from_dir) {
            return Some(found);
        }
    }
    for base in search_paths {
        if let Some(found) = candidate(base) {
            return Some(found);
        }
    }
    // A bare name in angle brackets is a system header unless a search path
    // says otherwise.
    if !quoted && !qualified {
        return None;
    }
    let mut dir = Some(from_dir);
    while let Some(current) = dir {
        for sub in CONVENTIONAL_DIRS {
            if let Some(found) = candidate(&join(current, sub)) {
                return Some(found);
            }
        }
        dir = parent(current);
    }
    if qualified {
        let suffix = format!("/{include}");
        let matches: Vec<FileId> = by_name
            .get(file_name(&include))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&f| repo.files[f].path.ends_with(&suffix))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
    }
    None
}

/// Pairs each implementation file with the header of the same name that is
/// in the same directory, or that it includes, or that includes it. A header
/// in the same directory wins; remaining ties go to the first path.
fn pair(
    repo: &Repo,
    files: &[FileId],
    includes: &BTreeMap<FileId, BTreeSet<FileId>>,
) -> BTreeMap<FileId, FileId> {
    let stem = |f: FileId| split_ext(file_name(&repo.files[f].path)).0;
    let is_component = |f: FileId| repo.files[f].role == Role::Component;

    let mut headers: BTreeMap<&str, Vec<FileId>> = BTreeMap::new();
    for &f in files {
        if is_component(f) && is_c_header(&repo.files[f].path) {
            headers.entry(stem(f)).or_default().push(f);
        }
    }

    let mut pairs = BTreeMap::new();
    for &source in files {
        let path = &repo.files[source].path;
        if !is_component(source) || is_c_header(path) {
            continue;
        }
        let best = headers
            .get(stem(source))
            .into_iter()
            .flatten()
            .copied()
            .filter_map(|header| {
                let same_dir = dir_of(&repo.files[header].path) == dir_of(path);
                let linked =
                    includes[&source].contains(&header) || includes[&header].contains(&source);
                (same_dir || linked).then_some((
                    !same_dir,
                    repo.files[header].path.as_str(),
                    header,
                ))
            })
            .min();
        if let Some((_, _, header)) = best {
            pairs.insert(source, header);
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_includes_outside_comments() {
        let parsed = parse(
            "// #include \"no.h\"\n/* #include \"no.h\"\n */\n#include \"a.h\"\n  #  include <lib/b.hpp> // trailing\n#include_next <c.h>\n#include MACRO\nint x = 1'000;\nconst char* s = \"// not a comment\";\n",
        );
        assert_eq!(
            parsed.includes,
            vec![("a.h".to_string(), true), ("lib/b.hpp".to_string(), false)]
        );
        assert!(!parsed.umbrella);
    }

    #[test]
    fn recognises_umbrella_headers() {
        assert!(
            parse("#pragma once\n// All of it.\n#include \"a.h\"\n#include \"b.h\"\n").umbrella
        );
        assert!(!parse("#pragma once\n#include \"a.h\"\nstruct S {};\n").umbrella);
        assert!(!parse("#pragma once\n").umbrella);
    }
}
