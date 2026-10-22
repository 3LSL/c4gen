// Relationships:
// - `src/analysis/cpp.rs`: Detects include dependencies and pairs headers with sources
// - `src/analysis/dependencies.rs`: Collects the detected dependencies and header pairs
// - `src/analysis/python.rs`: Detects import dependencies between Python files
// - `src/analysis/rust.rs`: Detects module dependencies and finds test modules
// - `src/analysis/typescript.rs`: Detects import dependencies between TypeScript files
// - `src/repo.rs`: Reads scanned files and marks test files

//! Detection of dependencies in the code, one analyser per language family.
//!
//! The analysers favour precision over recall. A dependency is reported only
//! when a name in the code can be followed to a file of the project; anything
//! ambiguous is left out, so that an arrow is never drawn for a dependency
//! that does not exist.

mod cpp;
mod dependencies;
mod python;
mod rust;
mod tsconfig;
mod typescript;

pub(crate) use dependencies::{Analysis, FileDeps};

use crate::repo::{Repo, Role};

/// Analyses the repository. Files that turn out to be test modules are
/// marked as such in `repo`.
pub(crate) fn run(repo: &mut Repo, search_paths: &[String]) -> Analysis {
    let mut deps = vec![FileDeps::default(); repo.files.len()];

    for file in rust::analyze(repo, &mut deps) {
        repo.files[file].role = Role::Test;
    }
    python::analyze(repo, search_paths, &mut deps);
    let pairs = cpp::analyze(repo, search_paths, &mut deps);
    typescript::analyze(repo, search_paths, &mut deps);

    let deps = deps
        .into_iter()
        .enumerate()
        .map(|(file, file_deps)| file_deps.without(file))
        .collect();
    Analysis { deps, pairs }
}
