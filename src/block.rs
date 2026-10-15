// Relationships:
// - `src/diagnostics.rs`: Classifies problems found in a relationship block

//! Parser and checker for the Relationships block at the top of a file.
//!
//! The parser is lenient and the checker is strict: every line that can be
//! understood is used, and every departure from the specified form is
//! reported as a [`Problem`].

use std::collections::BTreeSet;

use crate::diagnostics::DiagnosticKind;

const HEADING: &str = "Relationships:";
const NONE: &str = "None.";
const EXTERNAL: &str = "external:";

/// Verbs that only say an arrow exists, as lower-case word sequences.
const BANNED_OPENINGS: &[&[&str]] = &[
    &["uses"],
    &["calls"],
    &["invokes"],
    &["depends", "on"],
    &["imports"],
    &["references"],
    &["accesses"],
    &["interacts", "with"],
    &["talks", "to"],
    &["works", "with"],
    &["handles"],
    &["manages"],
];

/// How comments are written in a language.
pub(crate) struct CommentSyntax {
    /// Marker that starts a line comment.
    pub line: &'static str,
    /// Opening and closing markers of a block comment, if the language has one.
    pub block: Option<(&'static str, &'static str)>,
    /// Prefixes of code lines that may come before the block, such as an
    /// include guard.
    pub preamble: &'static [&'static str],
}

pub(crate) const HASH: CommentSyntax = CommentSyntax {
    line: "#",
    block: None,
    preamble: &[],
};

pub(crate) const SLASHES: CommentSyntax = CommentSyntax {
    line: "//",
    block: Some(("/*", "*/")),
    preamble: &[],
};

pub(crate) const C_FAMILY: CommentSyntax = CommentSyntax {
    line: "//",
    block: Some(("/*", "*/")),
    preamble: &["#pragma", "#if", "#define"],
};

/// One relationship line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    /// One-based line number in the file.
    pub number: usize,
    pub target: String,
    pub inbound: bool,
    pub label: String,
}

/// A departure from the specified form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub kind: DiagnosticKind,
    pub line: usize,
    pub message: String,
}

/// A parsed Relationships block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Block {
    /// One-based line number of the heading.
    pub heading: usize,
    /// Relationship lines in source order, without duplicates.
    pub lines: Vec<Line>,
    pub problems: Vec<Problem>,
}

/// Whether a target names an external system; returns its name if so.
pub(crate) fn external_name(target: &str) -> Option<&str> {
    target.strip_prefix(EXTERNAL)
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Line,
    Block,
}

struct Heading {
    index: usize,
    style: Style,
    /// The block comment that holds the heading ends on the heading line.
    closed: bool,
}

/// Finds and parses the block in the leading comments of `text`. Returns
/// `None` when the file has no block before its first line of code.
pub(crate) fn parse(text: &str, syntax: &CommentSyntax) -> Option<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let heading = find_heading(&lines, syntax)?;
    let mut block = Block {
        heading: heading.index + 1,
        ..Block::default()
    };
    let mut none_line = None;
    let mut entries = 0usize;

    match heading.style {
        Style::Line => {
            let expected = format!("{} {HEADING}", syntax.line);
            if lines[heading.index].trim_end() != expected {
                block.problems.push(format_problem(
                    heading.index + 1,
                    format!("the heading line must be exactly `{expected}`"),
                ));
            }
            for (index, raw) in lines.iter().enumerate().skip(heading.index + 1) {
                let Some(content) = raw.trim().strip_prefix(syntax.line) else {
                    break;
                };
                let body = content.trim();
                if body.is_empty() {
                    break;
                }
                entries += 1;
                let written = raw
                    .trim_end()
                    .strip_prefix(syntax.line)
                    .and_then(|r| r.strip_prefix(' '));
                read_body(&mut block, &mut none_line, index + 1, body, written);
            }
        }
        Style::Block => {
            if !heading.closed {
                let close = syntax.block.map_or("*/", |(_, close)| close);
                for (index, raw) in lines.iter().enumerate().skip(heading.index + 1) {
                    let trimmed = raw.trim();
                    let (content, closes) = match trimmed.find(close) {
                        Some(end) => (&trimmed[..end], true),
                        None => (trimmed, false),
                    };
                    let body = strip_decoration(content);
                    if body.is_empty() {
                        break;
                    }
                    entries += 1;
                    read_body(&mut block, &mut none_line, index + 1, body, Some(body));
                    if closes {
                        break;
                    }
                }
            }
        }
    }

    if let Some(line) = none_line {
        if entries > 1 {
            block.problems.push(format_problem(
                line,
                "`None.` cannot be combined with other lines".to_string(),
            ));
        }
    } else if entries == 0 {
        block.problems.push(format_problem(
            block.heading,
            "the block is empty; write `None.` when the file has no relationships".to_string(),
        ));
    }
    check(&mut block);
    Some(block)
}

fn find_heading(lines: &[&str], syntax: &CommentSyntax) -> Option<Heading> {
    let mut in_block = false;
    for (index, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        if in_block {
            let close = syntax.block.map_or("*/", |(_, close)| close);
            let (content, rest) = match trimmed.find(close) {
                Some(end) => (&trimmed[..end], Some(trimmed[end + close.len()..].trim())),
                None => (trimmed, None),
            };
            if strip_decoration(content) == HEADING {
                return Some(Heading {
                    index,
                    style: Style::Block,
                    closed: rest.is_some(),
                });
            }
            if let Some(rest) = rest {
                in_block = false;
                if !rest.is_empty() {
                    return None;
                }
            }
            continue;
        }
        let shebang = index == 0 && trimmed.starts_with("#!") && !trimmed.starts_with("#![");
        if trimmed.is_empty() || shebang {
            continue;
        }
        if let Some((open, close)) = syntax.block {
            if let Some(after) = trimmed.strip_prefix(open) {
                match after.find(close) {
                    Some(end) => {
                        if strip_decoration(&after[..end]) == HEADING {
                            return Some(Heading {
                                index,
                                style: Style::Block,
                                closed: true,
                            });
                        }
                        if !after[end + close.len()..].trim().is_empty() {
                            return None;
                        }
                    }
                    None => {
                        if strip_decoration(after) == HEADING {
                            return Some(Heading {
                                index,
                                style: Style::Block,
                                closed: false,
                            });
                        }
                        in_block = true;
                    }
                }
                continue;
            }
        }
        if let Some(content) = trimmed.strip_prefix(syntax.line) {
            if content.trim() == HEADING {
                return Some(Heading {
                    index,
                    style: Style::Line,
                    closed: false,
                });
            }
            continue;
        }
        if syntax
            .preamble
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        {
            continue;
        }
        return None;
    }
    None
}

/// Removes the `*` that conventionally starts each line of a block comment.
fn strip_decoration(content: &str) -> &str {
    content.trim().trim_start_matches('*').trim()
}

/// Reads one line of the block body. `written` is the line as it appears
/// after the comment marker and one space, used to check the exact form.
fn read_body(
    block: &mut Block,
    none_line: &mut Option<usize>,
    number: usize,
    body: &str,
    written: Option<&str>,
) {
    if body == NONE {
        none_line.get_or_insert(number);
        if written != Some(NONE) {
            block.problems.push(format_problem(
                number,
                "write `None.` after the comment marker and one space".to_string(),
            ));
        }
        return;
    }
    match parse_relationship(body) {
        Ok((target, inbound, label)) => {
            let marker = if inbound { " (inbound)" } else { "" };
            let canonical = format!("- `{target}`{marker}: {label}");
            if written != Some(canonical.as_str()) {
                block.problems.push(format_problem(
                    number,
                    format!("spacing or punctuation differs from `{canonical}`"),
                ));
            }
            block.lines.push(Line {
                number,
                target,
                inbound,
                label,
            });
        }
        Err(message) => block.problems.push(Problem {
            kind: DiagnosticKind::MalformedLine,
            line: number,
            message: format!("{message}; the line is ignored"),
        }),
    }
}

fn parse_relationship(body: &str) -> Result<(String, bool, String), &'static str> {
    let rest = body
        .strip_prefix('-')
        .ok_or("expected `- `target`: Label` or `None.`")?
        .trim_start();
    let rest = rest
        .strip_prefix('`')
        .ok_or("the target must be written in backticks")?;
    let end = rest.find('`').ok_or("the target has no closing backtick")?;
    let target = rest[..end].trim();
    let mut rest = rest[end + 1..].trim_start();
    let inbound = match rest.strip_prefix("(inbound)") {
        Some(after) => {
            rest = after.trim_start();
            true
        }
        None => false,
    };
    let label = rest
        .strip_prefix(':')
        .ok_or("expected `:` after the target")?
        .trim();
    if target.is_empty() {
        return Err("the target is empty");
    }
    if label.is_empty() {
        return Err("the label is missing");
    }
    Ok((target.to_string(), inbound, label.to_string()))
}

fn format_problem(line: usize, message: String) -> Problem {
    Problem {
        kind: DiagnosticKind::Format,
        line,
        message,
    }
}

/// Checks targets, labels, duplicates and order, and drops duplicate lines.
fn check(block: &mut Block) {
    let mut problems = Vec::new();
    for line in &block.lines {
        if let Some(message) = check_target(&line.target, line.inbound) {
            problems.push(Problem {
                kind: DiagnosticKind::InvalidTarget,
                line: line.number,
                message,
            });
        }
        for message in check_label(&line.label) {
            problems.push(Problem {
                kind: DiagnosticKind::Label,
                line: line.number,
                message,
            });
        }
    }

    let sort_key = |line: &Line| (line.target.to_lowercase(), line.target.clone());
    if let Some(pair) = block
        .lines
        .windows(2)
        .find(|pair| sort_key(&pair[0]) > sort_key(&pair[1]))
    {
        problems.push(Problem {
            kind: DiagnosticKind::Unsorted,
            line: pair[1].number,
            message: format!(
                "lines are not sorted by target: `{}` comes after `{}`",
                pair[1].target, pair[0].target
            ),
        });
    }

    let mut seen = BTreeSet::new();
    block.lines.retain(|line| {
        let first = seen.insert((line.target.clone(), line.inbound));
        if !first {
            problems.push(Problem {
                kind: DiagnosticKind::DuplicateTarget,
                line: line.number,
                message: format!(
                    "`{}` already has a line in this block; this line is ignored",
                    line.target
                ),
            });
        }
        first
    });

    block.problems.append(&mut problems);
    block
        .problems
        .sort_by(|a, b| (a.line, a.kind, &a.message).cmp(&(b.line, b.kind, &b.message)));
}

fn check_target(target: &str, inbound: bool) -> Option<String> {
    if let Some(name) = external_name(target) {
        let well_formed = !name.is_empty()
            && name.split('-').all(|word| {
                !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            });
        if !well_formed {
            return Some(format!(
                "`{target}` is not a valid external name; use lower-case words joined by hyphens"
            ));
        }
        return None;
    }
    if inbound {
        return Some("`(inbound)` is only for `external:` targets".to_string());
    }
    let malformed = target.starts_with('/')
        || target.ends_with('/')
        || target.contains('\\')
        || target
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if malformed {
        return Some(format!(
            "`{target}` is not a path from the repository root with forward slashes"
        ));
    }
    None
}

fn check_label(label: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let words: Vec<String> = label.split_whitespace().map(str::to_lowercase).collect();
    if words.len() < 2 || words.len() > 8 {
        problems.push(format!(
            "the label has {} word{}; it must have two to eight",
            words.len(),
            if words.len() == 1 { "" } else { "s" }
        ));
    }
    if let Some(bad) = label
        .chars()
        .find(|c| !(c.is_alphanumeric() || matches!(c, ' ' | '-' | ',')))
    {
        problems.push(format!(
            "the label contains `{bad}`; only letters, digits, spaces, hyphens and commas are allowed"
        ));
    }
    if !label.chars().next().is_some_and(char::is_uppercase) {
        problems.push("the label must begin with a capitalised verb".to_string());
    }
    for opening in BANNED_OPENINGS {
        let matches = words.len() >= opening.len()
            && opening
                .iter()
                .zip(&words)
                .all(|(banned, word)| banned == word);
        if matches {
            problems.push(format!(
                "the label begins with `{}`, which only says that an arrow exists",
                opening.join(" ")
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(block: &Block) -> Vec<DiagnosticKind> {
        block.problems.iter().map(|p| p.kind).collect()
    }

    #[test]
    fn parses_a_python_block_after_a_shebang() {
        let text = "#!/usr/bin/env python3\n# Licence text.\n\n# Relationships:\n# - `app/a.py`: Stores session records\n# - `external:web-client` (inbound): Submits login requests\n\nimport os\n";
        let block = parse(text, &HASH).unwrap();
        assert_eq!(block.heading, 4);
        assert!(block.problems.is_empty(), "{:?}", block.problems);
        assert_eq!(
            block.lines,
            vec![
                Line {
                    number: 5,
                    target: "app/a.py".into(),
                    inbound: false,
                    label: "Stores session records".into()
                },
                Line {
                    number: 6,
                    target: "external:web-client".into(),
                    inbound: true,
                    label: "Submits login requests".into()
                },
            ]
        );
    }

    #[test]
    fn parses_none() {
        let block = parse("// Relationships:\n// None.\n\nfn main() {}\n", &SLASHES).unwrap();
        assert!(block.lines.is_empty());
        assert!(block.problems.is_empty());
    }

    #[test]
    fn finds_no_block_after_code_or_in_doc_comments() {
        assert!(parse("import os\n# Relationships:\n# None.\n", &HASH).is_none());
        assert!(parse("\"\"\"Docstring.\"\"\"\n# Relationships:\n# None.\n", &HASH).is_none());
        assert!(parse("/// Relationships:\n/// None.\n", &SLASHES).is_none());
        assert!(parse(
            "#![allow(dead_code)]\n// Relationships:\n// None.\n",
            &SLASHES
        )
        .is_none());
        assert!(parse("", &SLASHES).is_none());
    }

    #[test]
    fn accepts_guards_and_block_comments_in_c() {
        let text = "/* Licence. */\n#pragma once\n\n/*\n * Relationships:\n * - `src/a.h`: Reads order rows\n */\n\n#include \"a.h\"\n";
        let block = parse(text, &C_FAMILY).unwrap();
        assert_eq!(block.heading, 5);
        assert_eq!(block.lines.len(), 1);
        assert_eq!(block.lines[0].target, "src/a.h");
        assert!(block.problems.is_empty(), "{:?}", block.problems);

        let text = "#ifndef A_H\n#define A_H\n// Relationships:\n// None.\n\n#endif\n";
        assert!(parse(text, &C_FAMILY).is_some());
        assert!(parse(text, &SLASHES).is_none());
    }

    #[test]
    fn stops_at_the_first_blank_or_empty_comment_line() {
        let text = "// Relationships:\n// - `a.rs`: Stores session records\n//\n// - `b.rs`: Stores other records\n";
        assert_eq!(parse(text, &SLASHES).unwrap().lines.len(), 1);
    }

    #[test]
    fn reports_malformed_and_non_canonical_lines() {
        let text = "//Relationships:\n// -  `a.rs` :  Stores session records\n// a.rs: Stores session records\n// - `b.rs` Stores session records\n";
        let block = parse(text, &SLASHES).unwrap();
        assert_eq!(block.lines.len(), 1);
        assert_eq!(block.lines[0].label, "Stores session records");
        assert_eq!(
            kinds(&block),
            vec![
                DiagnosticKind::Format,
                DiagnosticKind::Format,
                DiagnosticKind::MalformedLine,
                DiagnosticKind::MalformedLine,
            ]
        );
    }

    #[test]
    fn reports_duplicates_order_and_empty_blocks() {
        let text = "# Relationships:\n# - `b.py`: Stores session records\n# - `a.py`: Stores session records\n# - `b.py`: Loads session records\n";
        let block = parse(text, &HASH).unwrap();
        assert_eq!(block.lines.len(), 2);
        assert_eq!(
            kinds(&block),
            vec![DiagnosticKind::Unsorted, DiagnosticKind::DuplicateTarget]
        );

        let block = parse("# Relationships:\n\nimport os\n", &HASH).unwrap();
        assert_eq!(kinds(&block), vec![DiagnosticKind::Format]);

        let block = parse(
            "# Relationships:\n# None.\n# - `a.py`: Stores session records\n",
            &HASH,
        )
        .unwrap();
        assert_eq!(kinds(&block), vec![DiagnosticKind::Format]);
    }

    #[test]
    fn checks_targets() {
        assert!(check_target("src/a.rs", false).is_none());
        assert!(check_target("external:orders-db", false).is_none());
        assert!(check_target("external:orders-db", true).is_none());
        assert!(check_target("external:Orders_DB", false).is_some());
        assert!(check_target("external:", false).is_some());
        assert!(check_target("src/a.rs", true).is_some());
        assert!(check_target("./a.rs", false).is_some());
        assert!(check_target("../a.rs", false).is_some());
        assert!(check_target("/src/a.rs", false).is_some());
        assert!(check_target("src\\a.rs", false).is_some());
    }

    #[test]
    fn checks_labels() {
        assert!(check_label("Stores and retrieves session records").is_empty());
        assert!(check_label("Reads retry, timeout and back-off settings").is_empty());
        assert!(check_label("Works out tax totals").is_empty());
        assert_eq!(check_label("Stores").len(), 1);
        assert_eq!(
            check_label("Stores one two three four five six seven eight").len(),
            1
        );
        assert_eq!(check_label("stores session records").len(), 1);
        assert_eq!(check_label("Stores session records.").len(), 1);
        assert_eq!(check_label("Calls Server::run").len(), 2);
        assert_eq!(check_label("Uses the repository").len(), 1);
        assert_eq!(check_label("Depends on config").len(), 1);
        assert_eq!(check_label("Works with the session store").len(), 1);
    }
}
