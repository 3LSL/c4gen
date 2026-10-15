// Relationships:
// None.

//! Problems found in Relationships blocks, reported alongside the diagram.

use std::fmt;

/// How serious a [`Diagnostic`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Information was lost: something written in a block could not be used.
    Error,
    /// The diagram is complete, but a block breaks the commenting rules or
    /// disagrees with the code.
    Warning,
}

/// The kind of problem a [`Diagnostic`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DiagnosticKind {
    /// A source file has no Relationships block.
    MissingBlock,
    /// A block is in an implementation file instead of its declaration file.
    MisplacedBlock,
    /// A line in a block is neither a relationship nor `None.`. It is ignored.
    MalformedLine,
    /// A block or line can be read but is not written exactly as specified.
    Format,
    /// A block has two lines for the same target. The second is ignored.
    DuplicateTarget,
    /// The lines of a block are not sorted by target.
    Unsorted,
    /// A target is not written as a root-relative path or an external name.
    InvalidTarget,
    /// A target names a file that does not exist. The line is ignored.
    TargetNotFound,
    /// A target names test code, a build script, or a hidden or excluded
    /// file. The line is ignored.
    TargetNotComponent,
    /// A label breaks one of the mechanical label rules.
    Label,
    /// The code depends on a file that the block has no line for. The
    /// relationship is drawn without a label.
    Undocumented,
    /// A line names a file that the code does not appear to refer to.
    Unconfirmed,
}

impl DiagnosticKind {
    /// The severity of every diagnostic of this kind. Kinds that cause a
    /// line to be ignored are errors; the rest are warnings.
    pub fn severity(self) -> Severity {
        match self {
            DiagnosticKind::MalformedLine
            | DiagnosticKind::DuplicateTarget
            | DiagnosticKind::TargetNotFound
            | DiagnosticKind::TargetNotComponent => Severity::Error,
            _ => Severity::Warning,
        }
    }

    /// A short, stable identifier for the kind, such as `missing-block`.
    pub fn code(self) -> &'static str {
        match self {
            DiagnosticKind::MissingBlock => "missing-block",
            DiagnosticKind::MisplacedBlock => "misplaced-block",
            DiagnosticKind::MalformedLine => "malformed-line",
            DiagnosticKind::Format => "format",
            DiagnosticKind::DuplicateTarget => "duplicate-target",
            DiagnosticKind::Unsorted => "unsorted",
            DiagnosticKind::InvalidTarget => "invalid-target",
            DiagnosticKind::TargetNotFound => "target-not-found",
            DiagnosticKind::TargetNotComponent => "target-not-component",
            DiagnosticKind::Label => "label",
            DiagnosticKind::Undocumented => "undocumented",
            DiagnosticKind::Unconfirmed => "unconfirmed",
        }
    }
}

/// A problem found in the Relationships block of one file.
///
/// Diagnostics are ordered by file, then line, so a sorted list reads in
/// source order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Diagnostic {
    /// Path of the file, relative to the repository root.
    pub file: String,
    /// One-based line number, when the problem belongs to a particular line.
    pub line: Option<usize>,
    /// How serious the problem is.
    pub severity: Severity,
    /// What kind of problem it is.
    pub kind: DiagnosticKind,
    /// A sentence describing the problem.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, ": {severity}[{}]: {}", self.kind.code(), self.message)
    }
}
