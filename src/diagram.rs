// Relationships:
// - `src/diagnostics.rs`: Carries diagnostics alongside the generated diagram

//! The generated diagram and its rendering as Mermaid text.

use std::collections::BTreeMap;
use std::fmt;

use crate::diagnostics::{Diagnostic, Severity};

/// A C4 component diagram of one module.
///
/// The diagram is plain data. [`Diagram::to_mermaid`] renders it, and the
/// fields can be read directly by callers that want to draw it another way.
/// Every list is in a fixed order, so the same source files always give an
/// identical value.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagram {
    /// Title printed at the top of the diagram.
    pub title: String,
    /// Path of the module, relative to the repository root. Empty when the
    /// module is the repository root itself.
    pub module: String,
    /// Boxes and boundaries, parents before children.
    pub elements: Vec<Element>,
    /// Arrows between elements.
    pub relations: Vec<Relation>,
    /// Problems found in the Relationships blocks that the diagram draws on,
    /// sorted by file and line.
    pub diagnostics: Vec<Diagnostic>,
}

/// What an [`Element`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ElementKind {
    /// A directory of the module drawn as a frame around its contents.
    Boundary,
    /// A source file inside the module. A C or C++ header and its
    /// implementation files are one component.
    Component,
    /// A directory inside the module, collapsed into one box.
    Directory,
    /// A project file outside the module.
    OutsideComponent,
    /// A directory outside the module, collapsed into one box.
    OutsideDirectory,
    /// A system outside the process, named by an `external:` target.
    ExternalSystem,
}

/// A box or boundary in a [`Diagram`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Element {
    /// Identifier used in the Mermaid text and in [`Relation`]s.
    pub id: String,
    /// What the element stands for.
    pub kind: ElementKind,
    /// Path relative to the repository root, or the name of an external
    /// system.
    pub path: String,
    /// Text shown on the element.
    pub label: String,
    /// Programming language of the element. Empty when unknown or not
    /// applicable.
    pub technology: String,
    /// Identifier of the boundary that contains the element, if any.
    pub parent: Option<String>,
}

/// An arrow between two [`Element`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Relation {
    /// Identifier of the element the arrow starts at.
    pub from: String,
    /// Identifier of the element the arrow ends at.
    pub to: String,
    /// Text shown on the arrow. When the arrow stands for several
    /// relationships their distinct labels are joined with `; `. Empty when
    /// none of them has a label.
    pub label: String,
    /// The relationships the arrow stands for.
    pub sources: Vec<RelationSource>,
}

/// One relationship behind a [`Relation`]: a line in a Relationships block,
/// or a dependency detected in the code that has no line.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationSource {
    /// File whose block holds the line, or whose code has the dependency.
    pub file: String,
    /// One-based line number of the line. `None` for a detected dependency.
    pub line: Option<usize>,
    /// The target as written in the line, or the path of the file the code
    /// depends on.
    pub target: String,
    /// The label of the line. `None` for a detected dependency.
    pub label: Option<String>,
}

impl Diagram {
    /// Renders the diagram as a Mermaid `C4Component` diagram.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("C4Component\n");
        out.push_str(&format!("    title {}\n", single_line(&self.title)));

        let mut children: BTreeMap<&str, Vec<&Element>> = BTreeMap::new();
        let mut top: Vec<&Element> = Vec::new();
        for element in &self.elements {
            match &element.parent {
                Some(parent) => children.entry(parent.as_str()).or_default().push(element),
                None => top.push(element),
            }
        }

        let mut group = None;
        for element in top {
            let next = group_of(element.kind);
            if group != Some(next) {
                out.push('\n');
                group = Some(next);
            }
            write_element(&mut out, element, &children, 1);
        }

        if !self.relations.is_empty() {
            out.push('\n');
            for relation in &self.relations {
                out.push_str(&format!(
                    "    Rel({}, {}, \"{}\")\n",
                    relation.from,
                    relation.to,
                    quoted(&relation.label)
                ));
            }
        }
        out
    }

    /// Whether any diagnostic has [`Severity::Error`].
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

impl fmt::Display for Diagram {
    /// Writes the Mermaid text, the same as [`Diagram::to_mermaid`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_mermaid())
    }
}

/// Top-level elements are separated by a blank line where the group changes.
fn group_of(kind: ElementKind) -> u8 {
    match kind {
        ElementKind::Boundary | ElementKind::Component | ElementKind::Directory => 0,
        ElementKind::OutsideComponent | ElementKind::OutsideDirectory => 1,
        ElementKind::ExternalSystem => 2,
    }
}

fn write_element(
    out: &mut String,
    element: &Element,
    children: &BTreeMap<&str, Vec<&Element>>,
    level: usize,
) {
    let indent = "    ".repeat(level);
    let label = quoted(&element.label);
    match element.kind {
        ElementKind::Boundary => {
            out.push_str(&format!(
                "{indent}Boundary({}, \"{label}\", \"module\") {{\n",
                element.id
            ));
            if let Some(inner) = children.get(element.id.as_str()) {
                for child in inner {
                    write_element(out, child, children, level + 1);
                }
            }
            out.push_str(&format!("{indent}}}\n"));
        }
        ElementKind::ExternalSystem => {
            out.push_str(&format!(
                "{indent}System_Ext({}, \"{label}\")\n",
                element.id
            ));
        }
        kind => {
            let name = match kind {
                ElementKind::OutsideComponent | ElementKind::OutsideDirectory => "Component_Ext",
                _ => "Component",
            };
            if element.technology.is_empty() {
                out.push_str(&format!("{indent}{name}({}, \"{label}\")\n", element.id));
            } else {
                out.push_str(&format!(
                    "{indent}{name}({}, \"{label}\", \"{}\")\n",
                    element.id,
                    quoted(&element.technology)
                ));
            }
        }
    }
}

/// Makes text safe inside a double-quoted Mermaid string.
fn quoted(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: &str, kind: ElementKind, label: &str, parent: Option<&str>) -> Element {
        Element {
            id: id.to_string(),
            kind,
            path: label.to_string(),
            label: label.to_string(),
            technology: if kind == ElementKind::Component {
                "Rust".to_string()
            } else {
                String::new()
            },
            parent: parent.map(str::to_string),
        }
    }

    #[test]
    fn renders_nested_elements_and_relations() {
        let diagram = Diagram {
            title: "Component diagram for src".to_string(),
            module: "src".to_string(),
            elements: vec![
                element("b_src", ElementKind::Boundary, "src", None),
                element("f_a", ElementKind::Component, "a.rs", Some("b_src")),
                element("x_db", ElementKind::ExternalSystem, "orders-db", None),
            ],
            relations: vec![Relation {
                from: "f_a".to_string(),
                to: "x_db".to_string(),
                label: "Reads \"order\" rows".to_string(),
                sources: Vec::new(),
            }],
            diagnostics: Vec::new(),
        };
        let expected = "C4Component\n    title Component diagram for src\n\n    Boundary(b_src, \"src\", \"module\") {\n        Component(f_a, \"a.rs\", \"Rust\")\n    }\n\n    System_Ext(x_db, \"orders-db\")\n\n    Rel(f_a, x_db, \"Reads 'order' rows\")\n";
        assert_eq!(diagram.to_mermaid(), expected);
        assert_eq!(diagram.to_string(), expected);
        assert!(!diagram.has_errors());
    }
}
