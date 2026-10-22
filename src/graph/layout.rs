// Relationships:
// - `src/diagnostics.rs`: Sorts the diagnostics reported with the diagram
// - `src/diagram.rs`: Builds the elements and relations of the diagram
// - `src/error.rs`: Reports modules that contain no components
// - `src/graph/components.rs`: Reads components and where each file is shown
// - `src/graph/reconcile.rs`: Groups relationships into arrows and filters problems
// - `src/options.rs`: Reads depth, outside and title settings
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads scanned files

//! Lays the diagram out around the chosen module: its components inside a
//! boundary, deeper directories and outside files collapsed into boxes, and
//! the relationships and problems that touch it.

use std::collections::{BTreeMap, BTreeSet};

use super::components::World;
use super::reconcile::{Edge, End, Finding};
use crate::diagnostics::Diagnostic;
use crate::diagram::{Diagram, Element, ElementKind, Relation, RelationSource};
use crate::error::Error;
use crate::options::{Options, Outside};
use crate::paths::{dir_of, file_name, join, segments};
use crate::repo::FileId;

/// The module being drawn.
pub(super) enum Scope {
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

pub(super) fn scope(world: &World, module: &str, module_is_file: bool) -> Result<Scope, Error> {
    if !module_is_file {
        return Ok(Scope::Dir(segments(module)));
    }
    let component = world
        .repo
        .by_path
        .get(module)
        .and_then(|&file| world.component_of[file])
        .ok_or_else(|| Error::NotAComponent {
            path: module.to_string(),
        })?;
    Ok(Scope::File {
        component,
        dir: segments(dir_of(module)),
    })
}

/// Draws the components of the module, the boxes they have relationships
/// with, and the relationships and problems that touch the module.
pub(super) fn assemble(
    world: &World,
    scope: &Scope,
    module: &str,
    edges: Vec<Edge>,
    findings: Vec<Finding>,
    options: &Options,
) -> Result<Diagram, Error> {
    Layout { world }.draw(scope, module, edges, findings, options)
}

struct Layout<'w, 'a> {
    world: &'w World<'a>,
}

impl Layout<'_, '_> {
    fn place(&self, scope: &Scope, id: usize) -> Place {
        let component = &self.world.components[id];
        match scope {
            Scope::Dir(base) => {
                for &file in &component.files {
                    let dirs = &self.world.file_dirs[file];
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
            dirs: self.world.file_dirs[component.files[0]].clone(),
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
                let component = &self.world.components[*id];
                match self.place(scope, *id) {
                    Place::Inside { anchor, below } => Some(inside_node(
                        scope,
                        self.world.path(anchor),
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
            .world
            .components
            .iter()
            .filter(|c| {
                c.files
                    .iter()
                    .any(|&f| self.world.file_dirs[f].starts_with(&prefix))
            })
            .map(|c| c.technology)
            .collect();
        technologies.into_iter().collect::<Vec<_>>().join(", ")
    }

    fn draw(
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
        for id in 0..self.world.components.len() {
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
