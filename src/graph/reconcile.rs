// Relationships:
// - `src/analysis/dependencies.rs`: Reads detected dependencies
// - `src/diagnostics.rs`: Records problems found in blocks and dependencies
// - `src/diagram.rs`: Records the source of each relationship
// - `src/graph/block.rs`: Parses the relationship block of each file
// - `src/graph/components.rs`: Reads each component and the files it holds
// - `src/options.rs`: Reads analysis settings
// - `src/repo.rs`: Reads scanned files and languages, looks up targets

//! Reconciles each Relationships block with the dependencies detected in
//! the code. Every line of a block becomes a relationship, and so does every
//! detected dependency that has no line; problems are recorded on the way.

use std::collections::{BTreeMap, BTreeSet};

use super::block::{self, CommentSyntax};
use super::components::World;
use crate::analysis::{Analysis, FileDeps};
use crate::diagnostics::{Diagnostic, DiagnosticKind};
use crate::diagram::RelationSource;
use crate::options::Options;
use crate::repo::{FileId, Language, OtherFile};

/// One end of a relationship, before it is mapped to a box.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum End {
    Component(usize),
    /// An existing file that is not a source file in a supported language.
    File(String),
    External(String),
}

pub(super) struct Edge {
    pub(super) from: End,
    pub(super) to: End,
    pub(super) source: RelationSource,
}

/// A diagnostic, with the components it concerns so it can be filtered to
/// the module being drawn.
pub(super) struct Finding {
    pub(super) diagnostic: Diagnostic,
    pub(super) subject: usize,
    pub(super) target: Option<usize>,
}

impl Finding {
    /// Names the component at the other end of the relationship concerned.
    fn about(mut self, target: usize) -> Self {
        self.target = Some(target);
        self
    }
}

fn syntax_of(language: Language) -> &'static CommentSyntax {
    match language {
        Language::Rust => &block::SLASHES,
        Language::Python => &block::HASH,
        Language::C => &block::C_FAMILY,
        Language::TypeScript => &block::SLASHES,
    }
}

/// Produces every relationship in the repository: one per line of every
/// block, plus one without a label for each dependency that is detected
/// in the code and has no line.
pub(super) fn reconcile(
    world: &World,
    analysis: &Analysis,
    options: &Options,
) -> (Vec<Edge>, Vec<Finding>) {
    Reconciler {
        world,
        deps: &analysis.deps,
    }
    .run(options.code_analysis)
}

struct Reconciler<'w, 'a> {
    world: &'w World<'a>,
    /// What the code of each file refers to, indexed by [`FileId`].
    deps: &'w [FileDeps],
}

impl Reconciler<'_, '_> {
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
                file: self.world.path(file).to_string(),
                line,
                severity: kind.severity(),
                kind,
                message,
            },
            subject,
            target: None,
        }
    }

    fn run(&self, code_analysis: bool) -> (Vec<Edge>, Vec<Finding>) {
        let mut edges = Vec::new();
        let mut findings = Vec::new();
        let mut closures = BTreeMap::new();
        for component in 0..self.world.components.len() {
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
        let component = &self.world.components[id];
        let primary = component.files[0];
        let syntax = syntax_of(component.language);

        let mut lines = Vec::new();
        let mut heading = None;
        let mut has_block = false;
        for &file in &component.files {
            let Some(parsed) = block::parse(&self.world.repo.files[file].text, syntax) else {
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
                file: self.world.path(file).to_string(),
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
            match self.world.repo.by_path.get(target) {
                Some(&target_file) => match self.world.component_of[target_file] {
                    Some(other) if other == id => findings.push(self.finding(
                        id,
                        file,
                        at,
                        DiagnosticKind::InvalidTarget,
                        format!("`{target}` is part of this component; a file cannot be its own target"),
                    )),
                    Some(other) => {
                        let declaration = &self.world.components[other].path;
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
                None => match self.world.repo.other_file(target) {
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

        let deps = self.deps;
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
                if let Some(other) = self.world.component_of[used].filter(|&other| other != id) {
                    let entry = detected.entry(other).or_insert(exact);
                    *entry = *entry || exact;
                }
            }
        }

        for (&other, (file, line, target)) in &documented {
            let same_language = self.world.components[other].language == component.language;
            let referenced = self.world.components[other]
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
            let target = self.world.components[other].path.clone();
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
            let deps = self.deps;
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
        for &file in &self.world.components[component].files {
            let behind = self.closure(file, closures);
            let accounted = documented.keys().any(|&other| {
                other != component
                    && self.world.components[other]
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
}
