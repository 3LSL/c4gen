// Relationships:
// - `src/analysis/dependencies.rs`: Reads header pairs
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads scanned files, roles and languages

//! Groups the scanned files into components, the boxes of a diagram, and
//! works out the directory each file is shown in.

use std::collections::BTreeSet;

use crate::analysis::Analysis;
use crate::paths::{dir_of, file_name, join, parent, segments, split_ext};
use crate::repo::{technology, FileId, Language, Repo, Role};

/// A box-sized unit of code: one source file, or a C or C++ header together
/// with its implementation files.
pub(super) struct Component {
    /// Path of the declaration file, which identifies the component.
    pub(super) path: String,
    /// The declaration file first, then implementation files by path.
    pub(super) files: Vec<FileId>,
    pub(super) language: Language,
    pub(super) technology: &'static str,
}

/// The scanned repository, grouped into components.
pub(super) struct World<'a> {
    pub(super) repo: &'a Repo,
    pub(super) components: Vec<Component>,
    /// Component of each file, indexed by [`FileId`]; `None` for a file
    /// that is not drawn.
    pub(super) component_of: Vec<Option<usize>>,
    /// Directory each file is shown in, indexed by [`FileId`].
    pub(super) file_dirs: Vec<Vec<String>>,
}

impl<'a> World<'a> {
    pub(super) fn new(repo: &'a Repo, analysis: &Analysis) -> Self {
        let is_component = |file: FileId| repo.files[file].role == Role::Component;
        let header_of = |file: FileId| {
            analysis
                .pairs
                .get(&file)
                .copied()
                .filter(|&h| is_component(h))
        };

        let mut components: Vec<Component> = Vec::new();
        let mut component_of = vec![None; repo.files.len()];
        for (file, source) in repo.files.iter().enumerate() {
            if is_component(file) && header_of(file).is_none() {
                component_of[file] = Some(components.len());
                components.push(Component {
                    path: source.path.clone(),
                    files: vec![file],
                    language: source.language,
                    technology: "",
                });
            }
        }
        for file in 0..repo.files.len() {
            if !is_component(file) {
                continue;
            }
            if let Some(component) = header_of(file).and_then(|header| component_of[header]) {
                component_of[file] = Some(component);
                components[component].files.push(file);
            }
        }
        for component in &mut components {
            let paths: Vec<&str> = component
                .files
                .iter()
                .map(|&f| repo.files[f].path.as_str())
                .collect();
            component.technology = technology(component.language, &paths);
        }

        // A Rust file `a/b.rs` that has a directory `a/b` of submodules next
        // to it is the root of that module, so it is shown inside `a/b`.
        let mut rust_dirs = BTreeSet::new();
        for source in &repo.files {
            if source.language == Language::Rust && source.role == Role::Component {
                let mut dir = Some(dir_of(&source.path));
                while let Some(current) = dir {
                    rust_dirs.insert(current);
                    dir = parent(current);
                }
            }
        }
        let file_dirs = repo
            .files
            .iter()
            .map(|source| {
                let dir = dir_of(&source.path);
                let mut dirs = segments(dir);
                let name = file_name(&source.path);
                if source.language == Language::Rust
                    && !matches!(name, "mod.rs" | "lib.rs" | "main.rs")
                {
                    let stem = split_ext(name).0;
                    if rust_dirs.contains(join(dir, stem).as_str()) {
                        dirs.push(stem.to_string());
                    }
                }
                dirs
            })
            .collect();

        World {
            repo,
            components,
            component_of,
            file_dirs,
        }
    }

    pub(super) fn path(&self, file: FileId) -> &str {
        &self.repo.files[file].path
    }
}
