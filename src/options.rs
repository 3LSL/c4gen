// Relationships:
// None.

//! Settings that control how a diagram is generated.

use std::path::PathBuf;

/// Directory and file names that [`Options::default`] skips when the
/// repository is scanned.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "target",
    "node_modules",
    "vendor",
    "third_party",
    "build",
    "dist",
    "__pycache__",
    "venv",
    "site-packages",
];

/// How many directory levels of the module are drawn in full.
///
/// A directory below the limit is drawn as a single box that stands for
/// everything inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// Draw this many levels. `Levels(1)` draws the files that sit directly
    /// in the module and collapses each of its subdirectories into one box.
    /// Zero is treated as one.
    Levels(usize),
    /// Draw every file, with nested boundaries for subdirectories.
    Unlimited,
}

impl Depth {
    pub(crate) fn limit(self) -> usize {
        match self {
            Depth::Levels(levels) => levels.max(1),
            Depth::Unlimited => usize::MAX,
        }
    }
}

/// How project files outside the module are drawn when they have a
/// relationship with something inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outside {
    /// Draw one box per neighbouring directory. A file outside the module is
    /// represented by the top-most directory that contains it but not the
    /// module, or by itself when it sits in a directory above the module.
    Collapse,
    /// Draw one box per file.
    Files,
    /// Leave project files outside the module out of the diagram, together
    /// with the relationships that reach them. External systems are kept.
    Hide,
}

/// Settings for [`generate`](crate::generate).
///
/// Create one with [`Options::default`] or [`Options::new`] and adjust it
/// with the builder methods or by assigning the public fields.
///
/// ```
/// use c4gen::{Depth, Options, Outside};
///
/// let options = Options::new()
///     .root("/path/to/repository")
///     .depth(Depth::Unlimited)
///     .outside(Outside::Files);
/// assert_eq!(options.depth, Depth::Unlimited);
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Repository root, which every target path in a Relationships block is
    /// relative to. When `None`, the nearest directory at or above the module
    /// that contains `.git`, `.hg` or `.svn` is used.
    pub root: Option<PathBuf>,
    /// How many directory levels of the module are drawn in full.
    pub depth: Depth,
    /// How project files outside the module are drawn.
    pub outside: Outside,
    /// Files and directories to skip. An entry without a slash matches any
    /// file or directory of that name; an entry with a slash matches that
    /// path from the repository root. Names that start with a dot are always
    /// skipped. Defaults to [`DEFAULT_EXCLUDES`].
    pub exclude: Vec<String>,
    /// Directories, relative to the repository root, that are searched first
    /// when a C or C++ `#include`, an absolute Python import or a
    /// non-relative TypeScript import is resolved.
    pub search_paths: Vec<String>,
    /// Whether dependencies detected in the code are used. When `true`, a
    /// detected dependency without a line is drawn as an unlabelled arrow and
    /// reported, and a line without a detected dependency is reported. When
    /// `false`, the diagram is built from Relationships blocks alone.
    pub code_analysis: bool,
    /// Diagram title. When `None`, a title naming the module is used.
    pub title: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            root: None,
            depth: Depth::Levels(1),
            outside: Outside::Collapse,
            exclude: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            search_paths: Vec::new(),
            code_analysis: true,
            title: None,
        }
    }
}

impl Options {
    /// Same as [`Options::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the repository root.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Sets how many directory levels are drawn in full.
    pub fn depth(mut self, depth: Depth) -> Self {
        self.depth = depth;
        self
    }

    /// Sets how project files outside the module are drawn.
    pub fn outside(mut self, outside: Outside) -> Self {
        self.outside = outside;
        self
    }

    /// Adds a file or directory to skip.
    pub fn exclude(mut self, entry: impl Into<String>) -> Self {
        self.exclude.push(entry.into());
        self
    }

    /// Adds a directory to search when includes and imports are resolved.
    pub fn search_path(mut self, dir: impl Into<String>) -> Self {
        self.search_paths.push(dir.into());
        self
    }

    /// Turns the use of dependencies detected in the code on or off.
    pub fn code_analysis(mut self, enabled: bool) -> Self {
        self.code_analysis = enabled;
        self
    }

    /// Sets the diagram title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}
