// Relationships:
// None.

//! The error type returned when no diagram can be generated.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Reasons why [`generate`](crate::generate) cannot produce a diagram.
///
/// Problems with individual Relationships blocks are not errors. They are
/// reported as [`Diagnostic`](crate::Diagnostic)s alongside the diagram.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A file or directory could not be read.
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// No repository root was given and none could be found above the module.
    RootNotFound {
        /// The module that was searched from.
        module: PathBuf,
    },
    /// The module is not inside the repository root.
    OutsideRoot {
        /// The module path.
        module: PathBuf,
        /// The repository root.
        root: PathBuf,
    },
    /// The module contains no source file in a supported language.
    NoComponents {
        /// The module path, relative to the repository root.
        module: String,
    },
    /// The module path is a single file that is not a component, for example
    /// a test file or a file in an unsupported language.
    NotAComponent {
        /// The file path, relative to the repository root.
        path: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Error::RootNotFound { module } => write!(
                f,
                "no repository root found above {} (no .git, .hg or .svn directory); pass the root explicitly",
                module.display()
            ),
            Error::OutsideRoot { module, root } => write!(
                f,
                "{} is not inside the repository root {}",
                module.display(),
                root.display()
            ),
            Error::NoComponents { module } => {
                let shown = if module.is_empty() { "the repository root" } else { module.as_str() };
                write!(f, "{shown} contains no Rust, Python, C or C++ source files to draw; test code and excluded files do not count")
            }
            Error::NotAComponent { path } => write!(
                f,
                "{path} is not a component: it is test code, a build script, an excluded file or a file in an unsupported language"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
