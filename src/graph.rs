// Relationships:
// - `src/analysis/mod.rs`: Reads detected dependencies and header pairs
// - `src/block.rs`: Parses the relationship block of each file
// - `src/diagnostics.rs`: Records problems found in blocks and dependencies
// - `src/diagram.rs`: Builds the elements and relations of the diagram
// - `src/error.rs`: Reports modules that contain no components
// - `src/options.rs`: Reads depth, outside and analysis settings
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads scanned files, roles and languages

//! Turns the scanned repository into a diagram: groups files into
//! components, reconciles each Relationships block with the dependencies
//! detected in the code, and lays the result out around the chosen module.

use std::collections::{BTreeMap, BTreeSet};

use crate::analysis::Analysis;
use crate::block::{self, CommentSyntax};
use crate::diagnostics::{Diagnostic, DiagnosticKind};
use crate::diagram::{Diagram, Element, ElementKind, Relation, RelationSource};
use crate::error::Error;
use crate::options::{Options, Outside};
use crate::paths::{dir_of, file_name, join, parent, segments, split_ext};
use crate::repo::{technology, FileId, Language, OtherFile, Repo, Role};

/// A box-sized unit of code: one source file, or a C or C++ header together
/// with its implementation files.
struct Component {
    /// Path of the declaration file, which identifies the component.
    path: String,
    /// The declaration file first, then implementation files by path.
    files: Vec<FileId>,
    language: Language,
    technology: &'static str,
}

/// One end of a relationship, before it is mapped to a box.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum End {
    Component(usize),
    /// An existing file that is not a source file in a supported language.
    File(String),
    External(String),
}

struct Edge {
    from: End,
    to: End,
    source: RelationSource,
}

/// A diagnostic, with the components it concerns so it can be filtered to
/// the module being drawn.
struct Finding {
    diagnostic: Diagnostic,
    subject: usize,
    target: Option<usize>,
}

impl Finding {
    /// Names the component at the other end of the relationship concerned.
    fn about(mut self, target: usize) -> Self {
        self.target = Some(target);
        self
    }
}

/// The module being drawn.
enum Scope {
    Dir(Vec<String>),
    File { component: usize, dir: Vec<String> },
}

impl Scope {
    /// Directory that the module is, or that the module file sits in.
    fn base(&self) -> &[String] {
        match self {
            Scope::Dir(dir) | Scope::File { dir, .. } => dir,
        }
    }
}

enum Place {
    /// Inside the module, `below` these directories.
    Inside {
        anchor: FileId,
        below: Vec<String>,
    },
    Outside {
        dirs: Vec<String>,
    },
}

/// The variants are in the order in which the groups are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NodeKind {
    File,
    Directory,
    OutsideFile,
    OutsideDirectory,
    External,
}

impl NodeKind {
    fn is_inside(self) -> bool {
        matches!(self, NodeKind::File | NodeKind::Directory)
    }

    fn is_directory(self) -> bool {
        matches!(self, NodeKind::Directory | NodeKind::OutsideDirectory)
    }
}

/// A box in the diagram, identified by its kind and its path or name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Node {
    kind: NodeKind,
    key: String,
}

struct NodeInfo {
    label: String,
    technology: String,
    /// Directories between the module and the box, for boxes inside it.
    below: Vec<String>,
}

/// Builds the diagram of `module`, a path relative to the repository root.
pub(crate) fn build(
    repo: &Repo,
    analysis: &Analysis,
    module: &str,
    module_is_file: bool,
    options: &Options,
) -> Result<Diagram, Error> {
    let world = World::new(repo, analysis);
    let scope = world.scope(module, module_is_file)?;
    let (edges, findings) = world.reconcile(options.code_analysis);
    world.assemble(&scope, module, edges, findings, options)
}

struct World<'a> {
    repo: &'a Repo,
    analysis: &'a Analysis,
    components: Vec<Component>,
    component_of: Vec<Option<usize>>,
    /// Directory each file is shown in, indexed by [`FileId`].
    file_dirs: Vec<Vec<String>>,
}

fn syntax_of(language: Language) -> &'static CommentSyntax {
    match language {
        Language::Rust => &block::SLASHES,
        Language::Python => &block::HASH,
        Language::C => &block::C_FAMILY,
        Language::TypeScript => &block::SLASHES,
    }
}

impl<'a> World<'a> {
    fn new(repo: &'a Repo, analysis: &'a Analysis) -> Self {
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
            analysis,
            components,
            component_of,
            file_dirs,
        }
    }

    fn scope(&self, module: &str, module_is_file: bool) -> Result<Scope, Error> {
        if !module_is_file {
            return Ok(Scope::Dir(segments(module)));
        }
        let component = self
            .repo
            .by_path
            .get(module)
            .and_then(|&file| self.component_of[file])
            .ok_or_else(|| Error::NotAComponent {
                path: module.to_string(),
            })?;
        Ok(Scope::File {
            component,
            dir: segments(dir_of(module)),
        })
    }

    fn path(&self, file: FileId) -> &str {
        &self.repo.files[file].path
    }

    fn finding(
        &self,
        subject: usize,
        file: FileId,
        line: Option<usize>,
        kind: DiagnosticKind,
        message: String,
    ) -> Finding {
        Finding {
            diagnostic: Diagnostic {
                file: self.path(file).to_string(),
                line,
                severity: kind.severity(),
                kind,
                message,
            },
            subject,
            target: None,
        }
    }

    // -----------------------------------------------------------------------
    // Reconciling blocks with code
    // -----------------------------------------------------------------------

    /// Produces every relationship in the repository: one per line of every
    /// block, plus one without a label for each dependency that is detected
    /// in the code and has no line.
    fn reconcile(&self, code_analysis: bool) -> (Vec<Edge>, Vec<Finding>) {
        let mut edges = Vec::new();
        let mut findings = Vec::new();
        let mut closures = BTreeMap::new();
        for component in 0..self.components.len() {
            self.reconcile_component(
                component,
                code_analysis,
                &mut closures,
                &mut edges,
                &mut findings,
            );
        }
        (edges, findings)
    }

    fn reconcile_component(
        &self,
        id: usize,
        code_analysis: bool,
        closures: &mut BTreeMap<FileId, BTreeSet<FileId>>,
        edges: &mut Vec<Edge>,
        findings: &mut Vec<Finding>,
    ) {
        let component = &self.components[id];
        let primary = component.files[0];
        let syntax = syntax_of(component.language);

        let mut lines = Vec::new();
        let mut heading = None;
        let mut has_block = false;
        for &file in &component.files {
            let Some(parsed) = block::parse(&self.repo.files[file].text, syntax) else {
                continue;
            };
            has_block = true;
            if file == primary {
                heading = Some(parsed.heading);
            } else {
                findings.push(self.finding(
                    id,
                    file,
                    Some(parsed.heading),
                    DiagnosticKind::MisplacedBlock,
                    format!(
                        "the block of a declaration and implementation pair belongs in `{}`",
                        component.path
                    ),
                ));
            }
            for problem in parsed.problems {
                findings.push(self.finding(
                    id,
                    file,
                    Some(problem.line),
                    problem.kind,
                    problem.message,
                ));
            }
            lines.extend(parsed.lines.into_iter().map(|line| (file, line)));
        }
        if !has_block {
            findings.push(self.finding(
                id,
                primary,
                None,
                DiagnosticKind::MissingBlock,
                "the file has no Relationships block".to_string(),
            ));
        }

        // Components that have a line, with the line that names them first.
        let mut documented: BTreeMap<usize, (FileId, usize, String)> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for (file, line) in &lines {
            let file = *file;
            let at = Some(line.number);
            let target = &line.target;
            if !seen.insert((target.clone(), line.inbound)) {
                findings.push(self.finding(
                    id,
                    file,
                    at,
                    DiagnosticKind::DuplicateTarget,
                    format!(
                        "`{target}` already has a line in `{}`; this line is ignored",
                        component.path
                    ),
                ));
                continue;
            }
            let source = RelationSource {
                file: self.path(file).to_string(),
                line: at,
                target: target.clone(),
                label: Some(line.label.clone()),
            };
            let own = End::Component(id);
            if let Some(name) = block::external_name(target) {
                let system = End::External(name.to_string());
                let (from, to) = if line.inbound {
                    (system, own)
                } else {
                    (own, system)
                };
                edges.push(Edge { from, to, source });
                continue;
            }
            match self.repo.by_path.get(target) {
                Some(&target_file) => match self.component_of[target_file] {
                    Some(other) if other == id => findings.push(self.finding(
                        id,
                        file,
                        at,
                        DiagnosticKind::InvalidTarget,
                        format!("`{target}` is part of this component; a file cannot be its own target"),
                    )),
                    Some(other) => {
                        let declaration = &self.components[other].path;
                        if target != declaration {
                            let message = format!(
                                "`{target}` is an implementation file; name its declaration file `{declaration}`"
                            );
                            findings.push(
                                self.finding(id, file, at, DiagnosticKind::InvalidTarget, message).about(other),
                            );
                        }
                        documented.entry(other).or_insert((file, line.number, target.clone()));
                        edges.push(Edge { from: own, to: End::Component(other), source });
                    }
                    None => findings.push(self.finding(
                        id,
                        file,
                        at,
                        DiagnosticKind::TargetNotComponent,
                        format!("`{target}` is test code or a build script, which diagrams leave out; the line is ignored"),
                    )),
                },
                None => match self.repo.other_file(target) {
                    OtherFile::Present => {
                        edges.push(Edge { from: own, to: End::File(target.clone()), source });
                    }
                    OtherFile::Skipped => findings.push(self.finding(
                        id,
                        file,
                        at,
                        DiagnosticKind::TargetNotComponent,
                        format!("`{target}` is hidden or excluded, so diagrams leave it out; the line is ignored"),
                    )),
                    OtherFile::Missing => findings.push(self.finding(
                        id,
                        file,
                        at,
                        DiagnosticKind::TargetNotFound,
                        format!("`{target}` does not exist; the line is ignored"),
                    )),
                },
            }
        }

        let deps = &self.analysis.deps;
        if !code_analysis || !component.files.iter().any(|&file| deps[file].analyzed) {
            return;
        }

        // What the code uses, and everything it could plausibly refer to.
        let mut detected: BTreeMap<usize, bool> = BTreeMap::new();
        let mut reachable: BTreeSet<FileId> = BTreeSet::new();
        for &file in &component.files {
            let file_deps = &deps[file];
            reachable.extend(&file_deps.imports);
            reachable.extend(file_deps.uses.keys());
            for &open in &file_deps.open {
                reachable.extend(self.closure(open, closures).iter());
            }
            for (&used, &exact) in &file_deps.uses {
                if let Some(other) = self.component_of[used].filter(|&other| other != id) {
                    let entry = detected.entry(other).or_insert(exact);
                    *entry = *entry || exact;
                }
            }
        }

        for (&other, (file, line, target)) in &documented {
            let same_language = self.components[other].language == component.language;
            let referenced = self.components[other]
                .files
                .iter()
                .any(|f| reachable.contains(f));
            if same_language && !referenced {
                let message = format!("no reference to `{target}` was found in the code");
                findings.push(
                    self.finding(id, *file, Some(*line), DiagnosticKind::Unconfirmed, message)
                        .about(other),
                );
            }
        }

        for (&other, &exact) in &detected {
            if documented.contains_key(&other) {
                continue;
            }
            // Where the trail of a name ended at a file without finding its
            // definition, a line for a file behind that one accounts for it.
            if !exact && self.covered(other, &documented, closures) {
                continue;
            }
            let target = self.components[other].path.clone();
            if has_block {
                let message =
                    format!("the code refers to `{target}`, but the block has no line for it");
                findings.push(
                    self.finding(id, primary, heading, DiagnosticKind::Undocumented, message)
                        .about(other),
                );
            }
            edges.push(Edge {
                from: End::Component(id),
                to: End::Component(other),
                source: RelationSource {
                    file: component.path.clone(),
                    line: None,
                    target,
                    label: None,
                },
            });
        }
    }

    /// The file itself, everything it imports, and everything those files
    /// pass on through re-exports.
    fn closure<'m>(
        &self,
        file: FileId,
        memo: &'m mut BTreeMap<FileId, BTreeSet<FileId>>,
    ) -> &'m BTreeSet<FileId> {
        memo.entry(file).or_insert_with(|| {
            let deps = &self.analysis.deps;
            let mut seen = BTreeSet::from([file]);
            let mut queue: Vec<FileId> = deps[file].imports.iter().copied().collect();
            while let Some(next) = queue.pop() {
                if seen.insert(next) {
                    queue.extend(&deps[next].reexports);
                }
            }
            seen
        })
    }

    fn covered(
        &self,
        component: usize,
        documented: &BTreeMap<usize, (FileId, usize, String)>,
        closures: &mut BTreeMap<FileId, BTreeSet<FileId>>,
    ) -> bool {
        for &file in &self.components[component].files {
            let behind = self.closure(file, closures);
            let accounted = documented.keys().any(|&other| {
                other != component
                    && self.components[other]
                        .files
                        .iter()
                        .any(|f| behind.contains(f))
            });
            if accounted {
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Laying out the module
    // -----------------------------------------------------------------------

    fn place(&self, scope: &Scope, id: usize) -> Place {
        let component = &self.components[id];
        match scope {
            Scope::Dir(base) => {
                for &file in &component.files {
                    let dirs = &self.file_dirs[file];
                    if dirs.starts_with(base) {
                        return Place::Inside {
                            anchor: file,
                            below: dirs[base.len()..].to_vec(),
                        };
                    }
                }
            }
            Scope::File { component: own, .. } => {
                if *own == id {
                    return Place::Inside {
                        anchor: component.files[0],
                        below: Vec::new(),
                    };
                }
            }
        }
        Place::Outside {
            dirs: self.file_dirs[component.files[0]].clone(),
        }
    }

    /// The box that stands for one end of a relationship, or `None` when
    /// that end is hidden.
    fn node(&self, scope: &Scope, end: &End, options: &Options) -> Option<(Node, NodeInfo)> {
        match end {
            End::External(name) => Some((
                Node {
                    kind: NodeKind::External,
                    key: name.clone(),
                },
                NodeInfo {
                    label: name.clone(),
                    technology: String::new(),
                    below: Vec::new(),
                },
            )),
            End::Component(id) => {
                let component = &self.components[*id];
                match self.place(scope, *id) {
                    Place::Inside { anchor, below } => Some(inside_node(
                        scope,
                        self.path(anchor),
                        below,
                        component.technology,
                        options,
                    )),
                    Place::Outside { dirs } => {
                        outside_node(scope, &component.path, &dirs, component.technology, options)
                    }
                }
            }
            End::File(path) => {
                let dirs = segments(dir_of(path));
                match scope {
                    Scope::Dir(base) if dirs.starts_with(base) => Some(inside_node(
                        scope,
                        path,
                        dirs[base.len()..].to_vec(),
                        "",
                        options,
                    )),
                    _ => outside_node(scope, path, &dirs, "", options),
                }
            }
        }
    }

    /// Languages of the components under a directory, joined with commas.
    fn directory_technology(&self, dir: &str) -> String {
        let prefix = segments(dir);
        let technologies: BTreeSet<&str> = self
            .components
            .iter()
            .filter(|c| {
                c.files
                    .iter()
                    .any(|&f| self.file_dirs[f].starts_with(&prefix))
            })
            .map(|c| c.technology)
            .collect();
        technologies.into_iter().collect::<Vec<_>>().join(", ")
    }

    fn assemble(
        &self,
        scope: &Scope,
        module: &str,
        edges: Vec<Edge>,
        findings: Vec<Finding>,
        options: &Options,
    ) -> Result<Diagram, Error> {
        // Every component of the module is drawn, connected or not.
        let mut nodes: BTreeMap<Node, NodeInfo> = BTreeMap::new();
        let mut inside = BTreeSet::new();
        for id in 0..self.components.len() {
            if matches!(self.place(scope, id), Place::Inside { .. }) {
                inside.insert(id);
                if let Some((node, info)) = self.node(scope, &End::Component(id), options) {
                    nodes.insert(node, info);
                }
            }
        }
        if inside.is_empty() {
            return Err(Error::NoComponents {
                module: module.to_string(),
            });
        }

        // Relationships that touch the module, merged per pair of boxes.
        let touches = |end: &End| match end {
            End::Component(id) => inside.contains(id),
            End::File(path) => {
                matches!(scope, Scope::Dir(base) if segments(dir_of(path)).starts_with(base))
            }
            End::External(_) => false,
        };
        let mut grouped: BTreeMap<(Node, Node), Vec<RelationSource>> = BTreeMap::new();
        for edge in edges {
            if !touches(&edge.from) && !touches(&edge.to) {
                continue;
            }
            let (Some((from, from_info)), Some((to, to_info))) = (
                self.node(scope, &edge.from, options),
                self.node(scope, &edge.to, options),
            ) else {
                continue;
            };
            if from == to {
                continue;
            }
            nodes.entry(from.clone()).or_insert(from_info);
            nodes.entry(to.clone()).or_insert(to_info);
            grouped.entry((from, to)).or_default().push(edge.source);
        }
        for (node, info) in &mut nodes {
            if node.kind.is_directory() {
                info.technology = self.directory_technology(&node.key);
            }
        }

        // Identifiers, assigned in a fixed order so collisions resolve the
        // same way every time.
        let mut used = BTreeSet::new();
        let mut ids: BTreeMap<&Node, String> = BTreeMap::new();
        for node in nodes.keys() {
            let prefix = match node.kind {
                NodeKind::External => "x",
                kind if kind.is_directory() => "d",
                _ => "f",
            };
            ids.insert(node, unique_id(prefix, &node.key, &mut used));
        }

        let mut elements = Vec::new();
        let by_label = |a: &(&Node, &NodeInfo), b: &(&Node, &NodeInfo)| {
            (&a.1.label, a.0).cmp(&(&b.1.label, b.0))
        };
        let element = |node: &Node, info: &NodeInfo, parent: Option<String>| Element {
            id: ids[node].clone(),
            kind: match node.kind {
                NodeKind::File => ElementKind::Component,
                NodeKind::Directory => ElementKind::Directory,
                NodeKind::OutsideFile => ElementKind::OutsideComponent,
                NodeKind::OutsideDirectory => ElementKind::OutsideDirectory,
                NodeKind::External => ElementKind::ExternalSystem,
            },
            path: node.key.clone(),
            label: info.label.clone(),
            technology: info.technology.clone(),
            parent,
        };

        let mut drawn_inside: Vec<(&Node, &NodeInfo)> = nodes
            .iter()
            .filter(|(node, _)| node.kind.is_inside())
            .collect();
        drawn_inside.sort_by(by_label);
        match scope {
            Scope::Dir(base) => {
                // One boundary for the module and one for each directory
                // that is drawn in full.
                let mut boundaries: BTreeMap<Vec<String>, String> = BTreeMap::new();
                let mut chains = BTreeSet::from([Vec::new()]);
                for (_, info) in &drawn_inside {
                    for length in 1..=info.below.len() {
                        chains.insert(info.below[..length].to_vec());
                    }
                }
                for chain in chains {
                    let path = join(&base.join("/"), &chain.join("/"));
                    let id = unique_id("b", &path, &mut used);
                    let label = match chain.last() {
                        Some(name) => name.clone(),
                        None if module.is_empty() => "(repository root)".to_string(),
                        None => module.to_string(),
                    };
                    let parent = chain.split_last().map(|(_, rest)| boundaries[rest].clone());
                    elements.push(Element {
                        id: id.clone(),
                        kind: ElementKind::Boundary,
                        path,
                        label,
                        technology: String::new(),
                        parent,
                    });
                    for (node, info) in &drawn_inside {
                        if info.below == chain {
                            elements.push(element(node, info, Some(id.clone())));
                        }
                    }
                    boundaries.insert(chain, id);
                }
            }
            Scope::File { .. } => {
                for (node, info) in &drawn_inside {
                    elements.push(element(node, info, None));
                }
            }
        }
        let mut outside: Vec<(&Node, &NodeInfo)> = nodes
            .iter()
            .filter(|(node, _)| {
                matches!(
                    node.kind,
                    NodeKind::OutsideFile | NodeKind::OutsideDirectory
                )
            })
            .collect();
        outside.sort_by(by_label);
        let systems = nodes
            .iter()
            .filter(|(node, _)| node.kind == NodeKind::External);
        for (node, info) in outside.into_iter().chain(systems) {
            elements.push(element(node, info, None));
        }

        let relations = grouped
            .into_iter()
            .map(|((from, to), mut sources)| {
                sources.sort_by(|a, b| {
                    (&a.file, a.line, &a.target).cmp(&(&b.file, b.line, &b.target))
                });
                let mut labels: Vec<&str> = Vec::new();
                for label in sources.iter().filter_map(|source| source.label.as_deref()) {
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                Relation {
                    from: ids[&from].clone(),
                    to: ids[&to].clone(),
                    label: labels.join("; "),
                    sources,
                }
            })
            .collect();

        let mut diagnostics: Vec<Diagnostic> = findings
            .into_iter()
            .filter(|f| {
                inside.contains(&f.subject) || f.target.is_some_and(|t| inside.contains(&t))
            })
            .map(|f| f.diagnostic)
            .collect();
        diagnostics.sort();
        diagnostics.dedup();

        let title = options.title.clone().unwrap_or_else(|| {
            let shown = if module.is_empty() {
                "the repository root"
            } else {
                module
            };
            format!("Component diagram for {shown}")
        });
        Ok(Diagram {
            title,
            module: module.to_string(),
            elements,
            relations,
            diagnostics,
        })
    }
}

fn inside_node(
    scope: &Scope,
    path: &str,
    below: Vec<String>,
    technology: &str,
    options: &Options,
) -> (Node, NodeInfo) {
    let depth = options.depth.limit();
    if below.len() < depth {
        return (
            Node {
                kind: NodeKind::File,
                key: path.to_string(),
            },
            NodeInfo {
                label: file_name(path).to_string(),
                technology: technology.to_string(),
                below,
            },
        );
    }
    // The file is deeper than the diagram goes, so the directory at the
    // limit stands for it.
    let kept = &below[..depth];
    (
        Node {
            kind: NodeKind::Directory,
            key: join(&scope.base().join("/"), &kept.join("/")),
        },
        NodeInfo {
            label: format!("{}/", kept[depth - 1]),
            technology: String::new(),
            below: kept[..depth - 1].to_vec(),
        },
    )
}

fn outside_node(
    scope: &Scope,
    path: &str,
    dirs: &[String],
    technology: &str,
    options: &Options,
) -> Option<(Node, NodeInfo)> {
    let file = || {
        (
            Node {
                kind: NodeKind::OutsideFile,
                key: path.to_string(),
            },
            NodeInfo {
                label: path.to_string(),
                technology: technology.to_string(),
                below: Vec::new(),
            },
        )
    };
    match options.outside {
        Outside::Hide => None,
        Outside::Files => Some(file()),
        Outside::Collapse => {
            // The neighbour is the directory just below the point where the
            // file's path and the module's path part ways.
            let shared = scope
                .base()
                .iter()
                .zip(dirs)
                .take_while(|(a, b)| a == b)
                .count();
            if dirs.len() <= shared {
                return Some(file());
            }
            let dir = dirs[..=shared].join("/");
            Some((
                Node {
                    kind: NodeKind::OutsideDirectory,
                    key: dir.clone(),
                },
                NodeInfo {
                    label: format!("{dir}/"),
                    technology: String::new(),
                    below: Vec::new(),
                },
            ))
        }
    }
}

/// Makes a Mermaid identifier from a path, numbering it if it is taken.
fn unique_id(prefix: &str, key: &str, used: &mut BTreeSet<String>) -> String {
    let cleaned: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let base = if cleaned.is_empty() {
        format!("{prefix}_root")
    } else {
        format!("{prefix}_{cleaned}")
    };
    let mut candidate = base.clone();
    let mut number = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{base}_{number}");
        number += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_colliding_identifiers() {
        let mut used = BTreeSet::new();
        assert_eq!(unique_id("f", "src/a-b.py", &mut used), "f_src_a_b_py");
        assert_eq!(unique_id("f", "src/a_b.py", &mut used), "f_src_a_b_py_2");
        assert_eq!(unique_id("f", "src/a.b.py", &mut used), "f_src_a_b_py_3");
        assert_eq!(unique_id("b", "", &mut used), "b_root");
    }
}
