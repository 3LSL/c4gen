// Relationships:
// - `src/analysis/mod.rs`: Detects dependencies between the scanned source files
// - `src/diagram.rs`: Returns the generated diagram to the caller
// - `src/error.rs`: Reports why a diagram could not be generated
// - `src/graph/mod.rs`: Assembles the diagram from blocks and dependencies
// - `src/options.rs`: Reads the generation settings
// - `src/repo.rs`: Locates the repository root and loads source files

//! Deterministic C4 component diagrams, generated from source code.
//!
//! `c4gen` takes the path of a software module and produces a C4 component
//! diagram of it in [Mermaid](https://mermaid.js.org/syntax/c4.html) syntax.
//! Each source file of the module becomes a component, and each arrow comes
//! from the *Relationships block* at the top of a file:
//!
//! ```text
//! // Relationships:
//! // - `external:web-client` (inbound): Submits login and logout requests
//! // - `src/crypto/token.rs`: Signs and verifies session tokens
//! // - `src/storage/repo.rs`: Stores and retrieves session records
//! ```
//!
//! The file that holds the block is where each arrow starts, the target in
//! backticks is where it ends, and the label is printed on the arrow word for
//! word. Rust, Python, C, C++ and TypeScript files are supported.
//!
//! # Example
//!
//! ```no_run
//! use c4gen::{generate, Options};
//!
//! # fn main() -> Result<(), c4gen::Error> {
//! let diagram = generate("src/session", &Options::default())?;
//! println!("{}", diagram.to_mermaid());
//! for diagnostic in &diagram.diagnostics {
//!     eprintln!("{diagnostic}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Where arrows come from
//!
//! * Every line of a block gives an arrow with that line's label.
//! * The code is also analysed. A dependency that is found in the code but
//!   has no line gives an arrow with no label and an
//!   [`Undocumented`](DiagnosticKind::Undocumented) diagnostic. A line whose
//!   dependency is not found in the code is still drawn, with an
//!   [`Unconfirmed`](DiagnosticKind::Unconfirmed) diagnostic.
//!
//! Set [`Options::code_analysis`] to `false` to build the diagram from the
//! blocks alone.
//!
//! # Determinism
//!
//! The output is a pure function of the files under the repository root and
//! the [`Options`]. Directories are read in sorted order, every collection is
//! ordered, and nothing depends on the clock, the environment, the absolute
//! location of the repository or the platform's path separator. Generating a
//! diagram twice from the same files gives the same bytes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod analysis;
mod diagnostics;
mod diagram;
mod error;
mod graph;
mod options;
mod paths;
mod repo;

use std::path::Path;

pub use diagnostics::{Diagnostic, DiagnosticKind, Severity};
pub use diagram::{Diagram, Element, ElementKind, Relation, RelationSource};
pub use error::Error;
pub use options::{Depth, Options, Outside, DEFAULT_EXCLUDES};

/// Generates the C4 component diagram of the module at `module`.
///
/// `module` is a directory, or a single source file, inside a repository. A
/// relative path is resolved against the current directory. The repository
/// root is taken from [`Options::root`], or found by looking upwards from the
/// module for a `.git`, `.hg` or `.svn` directory.
///
/// For a directory, the diagram shows its source files as components inside
/// a boundary, the neighbouring files and directories they have
/// relationships with, and the external systems they name. For a single
/// file, it shows that file and its neighbours.
///
/// Problems in individual Relationships blocks do not stop generation. They
/// are returned in [`Diagram::diagnostics`].
///
/// # Errors
///
/// Returns an [`Error`] when the module or the repository cannot be read,
/// when no repository root can be found, when the module is not inside the
/// root, or when the module contains nothing to draw.
pub fn generate<P: AsRef<Path>>(module: P, options: &Options) -> Result<Diagram, Error> {
    let location = repo::locate(module.as_ref(), options.root.as_deref())?;
    let mut repository = repo::Repo::scan(&location.root, &options.exclude)?;
    let search_paths: Vec<String> = options
        .search_paths
        .iter()
        .map(|path| path.trim().replace('\\', "/").trim_matches('/').to_string())
        .collect();
    let analysis = analysis::run(&mut repository, &search_paths);
    graph::build(
        &repository,
        &analysis,
        &location.module,
        location.is_file,
        options,
    )
}
