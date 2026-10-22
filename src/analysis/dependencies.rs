// Relationships:
// - `src/repo.rs`: Identifies scanned files by their index

//! What the analysers find: the dependencies detected in each file's code,
//! and the C and C++ implementation files paired with their headers.

use std::collections::{BTreeMap, BTreeSet};

use crate::repo::FileId;

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
    pub(super) fn without(mut self, own: FileId) -> Self {
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
