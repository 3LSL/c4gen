// Relationships:
// - `src/analysis/cpp.rs`: Detects include dependencies and pairs headers with sources
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
mod python;
mod rust;
mod tsconfig;
mod typescript;

use std::collections::{BTreeMap, BTreeSet};

use crate::repo::{FileId, Repo, Role};

/// What the code of one file refers to.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileDeps {
    /// Files whose definitions the code uses. The flag is `true` when the
    /// used name was found defined in that file, and `false` when the trail
    /// ended there without finding it.
    pub uses: BTreeMap<FileId, bool>,
    /// Every project file named by an import, used or not.
    pub imports: BTreeSet<FileId>,
    /// Files whose whole contents are visible to this file: wildcard imports,
    /// included headers, and files where a name could not be found.
    pub open: BTreeSet<FileId>,
    /// Files this file passes on to its own importers.
    pub reexports: BTreeSet<FileId>,
    /// Whether the file was analysed at all.
    pub analyzed: bool,
}

impl FileDeps {
    pub(crate) fn add_use(&mut self, file: FileId, exact: bool) {
        let entry = self.uses.entry(file).or_insert(exact);
        *entry = *entry || exact;
        self.imports.insert(file);
        if !exact {
            self.open.insert(file);
        }
    }

    /// Removes a file's references to itself.
    fn without(mut self, own: FileId) -> Self {
        self.uses.remove(&own);
        self.imports.remove(&own);
        self.open.remove(&own);
        self.reexports.remove(&own);
        self
    }
}

/// Result of analysing every file of the repository.
pub(crate) struct Analysis {
    /// Indexed by [`FileId`].
    pub deps: Vec<FileDeps>,
    /// C and C++ implementation files, mapped to the header they belong to.
    pub pairs: BTreeMap<FileId, FileId>,
}

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
