// Relationships:
// - `src/analysis/dependencies.rs`: Records the dependencies detected for each file
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads Rust sources and crate manifests

//! Dependency detection for Rust: `use` declarations and paths in code,
//! resolved through the module tree of each crate and followed through
//! re-exports to the file that defines each item.

use std::collections::{BTreeMap, BTreeSet};

use super::FileDeps;
use crate::paths::{dir_of, file_name, join, normalize, split_ext};
use crate::repo::{FileId, Language, Repo};

/// Limit on nested path resolutions, as a guard against import cycles.
const MAX_DEPTH: usize = 48;
/// Keywords that are followed by the name of the item they define.
const ITEM_KEYWORDS: &[&str] = &[
    "fn", "struct", "enum", "union", "trait", "type", "const", "static",
];
/// Words that can follow an item keyword without being the item's name.
const NOT_A_NAME: &[&str] = &["fn", "unsafe", "async", "extern", "mut", "_", "impl", "dyn"];

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
    /// The path separator `::`.
    Sep,
    Str(String),
    /// A number, character or lifetime.
    Other,
}

/// One name brought in by a `use` declaration.
#[derive(Clone, Debug, PartialEq)]
struct UseLeaf {
    is_pub: bool,
    /// The path starts with `::`.
    absolute: bool,
    segments: Vec<String>,
    glob: bool,
    /// The name the import binds; empty for a glob.
    name: String,
}

#[derive(Clone, Debug, PartialEq)]
struct PathRef {
    absolute: bool,
    segments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ExternalMod {
    name: String,
    path_attr: Option<String>,
    cfg_test: bool,
}

/// The contents of one module: a file, or an inline `mod name { ... }`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Scope {
    name: String,
    /// The module starts with `#![cfg(test)]`.
    cfg_test: bool,
    inline: Vec<Scope>,
    external: Vec<ExternalMod>,
    uses: Vec<UseLeaf>,
    /// Names of the items defined directly in the module.
    defs: BTreeSet<String>,
    /// Macros defined with `macro_rules!`; `true` when exported.
    macros: BTreeMap<String, bool>,
    /// Paths of two or more segments used in the code.
    paths: Vec<PathRef>,
    /// Every identifier that starts a path or stands alone in the code.
    idents: BTreeSet<String>,
    /// Names of macros invoked without a path.
    macro_calls: BTreeSet<String>,
}

#[derive(Default)]
struct Attribute {
    /// `#[cfg(test)]` or a `cfg` that mentions `test` without negating it.
    test_gate: bool,
    /// `#[test]`, `#[bench]` and the like.
    test_item: bool,
    macro_export: bool,
    path: Option<String>,
}

#[derive(Default)]
struct Pending {
    skip: bool,
    is_pub: bool,
    macro_export: bool,
    path: Option<String>,
}

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self, offset: usize) -> Option<&Tok> {
        self.toks.get(self.i + offset)
    }

    fn is_punct(&self, offset: usize, c: char) -> bool {
        self.peek(offset) == Some(&Tok::Punct(c))
    }

    fn ident(&self, offset: usize) -> Option<&str> {
        match self.peek(offset) {
            Some(Tok::Ident(word)) => Some(word.as_str()),
            _ => None,
        }
    }

    /// Parses a module body up to its closing brace or the end of the file.
    fn scope(&mut self, name: String) -> Scope {
        let mut scope = Scope {
            name,
            ..Scope::default()
        };
        let mut depth = 0usize;
        let mut pending = Pending::default();
        while self.i < self.toks.len() {
            // Attributes and visibility apply to the item that follows them.
            if self.is_punct(0, '#') {
                let inner = self.is_punct(1, '!');
                let open = self.i + 1 + usize::from(inner);
                if self.toks.get(open) == Some(&Tok::Punct('[')) {
                    let close = self.matching(open, '[', ']');
                    let attribute = classify(&self.toks[open + 1..close.min(self.toks.len())]);
                    if inner {
                        scope.cfg_test |= attribute.test_gate;
                    } else {
                        pending.skip |= attribute.test_gate || attribute.test_item;
                        pending.macro_export |= attribute.macro_export;
                        if attribute.path.is_some() {
                            pending.path = attribute.path;
                        }
                    }
                    self.i = close + 1;
                    continue;
                }
            }
            if self.ident(0) == Some("pub") {
                pending.is_pub = true;
                self.i += 1;
                let restricted = self.is_punct(0, '(')
                    && matches!(self.ident(1), Some("crate" | "super" | "self" | "in"));
                if restricted {
                    self.i = self.matching(self.i, '(', ')') + 1;
                }
                continue;
            }

            let pending_now = std::mem::take(&mut pending);
            if self.ident(0) == Some("mod") {
                if let Some(name) = self.ident(1).map(str::to_string) {
                    if self.is_punct(2, ';') {
                        self.i += 3;
                        scope.defs.insert(name.clone());
                        scope.external.push(ExternalMod {
                            name,
                            path_attr: pending_now.path,
                            cfg_test: pending_now.skip,
                        });
                        continue;
                    }
                    if self.is_punct(2, '{') {
                        self.i += 3;
                        let child = self.scope(name.clone());
                        if !pending_now.skip && !child.cfg_test {
                            scope.defs.insert(name);
                            scope.inline.push(child);
                        }
                        continue;
                    }
                }
            }
            if pending_now.skip {
                self.skip_item();
                continue;
            }
            if self.ident(0) == Some("use") && !self.is_punct(1, '<') {
                self.i += 1;
                self.use_declaration(&mut scope, pending_now.is_pub);
                continue;
            }

            match self.toks[self.i].clone() {
                Tok::Punct('{') => {
                    depth += 1;
                    self.i += 1;
                }
                Tok::Punct('}') => {
                    self.i += 1;
                    if depth == 0 {
                        return scope;
                    }
                    depth -= 1;
                }
                Tok::Ident(word) => {
                    if depth == 0 {
                        self.definition(&mut scope, &word, pending_now.macro_export);
                    }
                    self.path(&mut scope, word);
                }
                _ => self.i += 1,
            }
        }
        scope
    }

    /// Records the name of the item that the keyword at the cursor defines.
    fn definition(&self, scope: &mut Scope, keyword: &str, exported: bool) {
        if keyword == "macro_rules" && self.is_punct(1, '!') {
            if let Some(name) = self.ident(2) {
                scope.macros.insert(name.to_string(), exported);
            }
            return;
        }
        if !ITEM_KEYWORDS.contains(&keyword) {
            return;
        }
        let mut offset = 1;
        if self.ident(offset) == Some("mut") {
            offset += 1;
        }
        if let Some(name) = self.ident(offset) {
            if !NOT_A_NAME.contains(&name) {
                scope.defs.insert(name.to_string());
            }
        }
    }

    /// Consumes the path that starts at the cursor and records it.
    fn path(&mut self, scope: &mut Scope, first: String) {
        let previous = self.i.checked_sub(1).map(|p| &self.toks[p]);
        let before_previous = self.i.checked_sub(2).map(|p| &self.toks[p]);
        // After `.` the identifier is a field or method, and after `>::` it
        // is an associated item; neither starts a path.
        let member = previous == Some(&Tok::Punct('.'))
            || (previous == Some(&Tok::Sep) && before_previous == Some(&Tok::Punct('>')));
        let absolute = previous == Some(&Tok::Sep) && !member;

        let mut segments = vec![first];
        self.i += 1;
        while self.peek(0) == Some(&Tok::Sep) {
            let Some(next) = self.ident(1) else { break };
            segments.push(next.to_string());
            self.i += 2;
        }
        if member {
            return;
        }
        if segments.len() == 1 && self.is_punct(0, '!') && !self.is_punct(1, '=') {
            scope.macro_calls.insert(segments[0].clone());
            return;
        }
        scope.idents.insert(segments[0].clone());
        if segments.len() >= 2 {
            scope.paths.push(PathRef { absolute, segments });
        }
    }

    fn use_declaration(&mut self, scope: &mut Scope, is_pub: bool) {
        let absolute = self.peek(0) == Some(&Tok::Sep);
        if absolute {
            self.i += 1;
        }
        let mut leaves = Vec::new();
        self.use_tree(Vec::new(), &mut leaves);
        // Resynchronise on the semicolon, whatever the tree looked like.
        while self.i < self.toks.len() && !self.is_punct(0, ';') {
            self.i += 1;
        }
        self.i += 1;
        for (segments, glob, name) in leaves {
            if !segments.is_empty() {
                scope.uses.push(UseLeaf {
                    is_pub,
                    absolute,
                    segments,
                    glob,
                    name,
                });
            }
        }
    }

    fn use_tree(&mut self, mut prefix: Vec<String>, out: &mut Vec<(Vec<String>, bool, String)>) {
        loop {
            match self.peek(0).cloned() {
                Some(Tok::Ident(word)) => {
                    prefix.push(word);
                    self.i += 1;
                    if self.peek(0) == Some(&Tok::Sep) {
                        self.i += 1;
                        continue;
                    }
                    // `a::b::{self}` imports `a::b` itself.
                    if prefix.len() > 1 && prefix.last().map(String::as_str) == Some("self") {
                        prefix.pop();
                    }
                    let mut name = prefix.last().cloned().unwrap_or_default();
                    if self.ident(0) == Some("as") {
                        if let Some(alias) = self.ident(1) {
                            name = alias.to_string();
                        }
                        self.i += 2;
                    }
                    out.push((prefix, false, name));
                    return;
                }
                Some(Tok::Punct('*')) => {
                    self.i += 1;
                    out.push((prefix, true, String::new()));
                    return;
                }
                Some(Tok::Punct('{')) => {
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            Some(Tok::Punct('}')) => {
                                self.i += 1;
                                return;
                            }
                            Some(Tok::Punct(',')) => self.i += 1,
                            Some(Tok::Punct(';')) | None => return,
                            Some(_) => {
                                let before = self.i;
                                self.use_tree(prefix.clone(), out);
                                if self.i == before {
                                    self.i += 1;
                                }
                            }
                        }
                    }
                }
                _ => return,
            }
        }
    }

    /// Skips one item or statement: up to its semicolon or the end of its
    /// first brace group, without leaving the enclosing braces.
    fn skip_item(&mut self) {
        let (mut round, mut square, mut curly) = (0i32, 0i32, 0i32);
        while self.i < self.toks.len() {
            match self.toks[self.i] {
                Tok::Punct('(') => round += 1,
                Tok::Punct(')') => round -= 1,
                Tok::Punct('[') => square += 1,
                Tok::Punct(']') => square -= 1,
                Tok::Punct('{') => curly += 1,
                Tok::Punct('}') => {
                    if curly == 0 {
                        return;
                    }
                    curly -= 1;
                    if curly == 0 && round <= 0 && square <= 0 {
                        self.i += 1;
                        return;
                    }
                }
                Tok::Punct(';') if curly == 0 && round <= 0 && square <= 0 => {
                    self.i += 1;
                    return;
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// Index of the bracket that closes the one at `open`.
    fn matching(&self, open: usize, opening: char, closing: char) -> usize {
        let mut depth = 0usize;
        for (index, tok) in self.toks.iter().enumerate().skip(open) {
            if *tok == Tok::Punct(opening) {
                depth += 1;
            } else if *tok == Tok::Punct(closing) {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
        }
        self.toks.len()
    }
}

fn classify(body: &[Tok]) -> Attribute {
    let mut leading = Vec::new();
    for tok in body {
        match tok {
            Tok::Ident(word) => leading.push(word.as_str()),
            Tok::Sep => {}
            _ => break,
        }
    }
    let has = |word: &str| {
        body.iter()
            .any(|tok| matches!(tok, Tok::Ident(w) if w == word))
    };
    let first = leading.first().copied().unwrap_or("");
    let last = leading.last().copied().unwrap_or("");
    let path = match body {
        [Tok::Ident(word), Tok::Punct('='), Tok::Str(value), ..] if word == "path" => {
            Some(value.clone())
        }
        _ => None,
    };
    Attribute {
        test_gate: first == "cfg" && has("test") && !has("not"),
        test_item: matches!(last, "test" | "bench" | "rstest"),
        macro_export: first == "macro_export",
        path,
    }
}

fn parse(text: &str) -> Scope {
    let toks = lex(text);
    Parser { toks: &toks, i: 0 }.scope(String::new())
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lex(text: &str) -> Vec<Tok> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && next == Some('/') {
            while i < n && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && next == Some('*') {
            // Block comments nest.
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if ch == '"' {
            let (value, end) = lex_string(&c, i);
            out.push(Tok::Str(value));
            i = end;
        } else if ch == '\'' {
            if next == Some('\\') {
                // An escaped character literal such as '\n' or '\''.
                i += 3;
                while i < n && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if c.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                // A lifetime or loop label.
                i += 1;
                while i < n && is_ident_continue(c[i]) {
                    i += 1;
                }
            }
            out.push(Tok::Other);
        } else if is_ident_start(ch) {
            if let Some(end) = lex_prefixed_literal(&c, i, &mut out) {
                i = end;
                continue;
            }
            let start = i;
            while i < n && is_ident_continue(c[i]) {
                i += 1;
            }
            let raw_ident = i - start == 1
                && ch == 'r'
                && c.get(i) == Some(&'#')
                && c.get(i + 1).copied().is_some_and(is_ident_start);
            if raw_ident {
                let name_start = i + 1;
                i = name_start;
                while i < n && is_ident_continue(c[i]) {
                    i += 1;
                }
                out.push(Tok::Ident(c[name_start..i].iter().collect()));
            } else {
                out.push(Tok::Ident(c[start..i].iter().collect()));
            }
        } else if ch.is_ascii_digit() {
            i += 1;
            while i < n
                && (c[i].is_ascii_alphanumeric()
                    || c[i] == '_'
                    || (c[i] == '.' && c.get(i + 1).is_some_and(char::is_ascii_digit)))
            {
                i += 1;
            }
            out.push(Tok::Other);
        } else if ch == ':' && next == Some(':') {
            out.push(Tok::Sep);
            i += 2;
        } else {
            out.push(Tok::Punct(ch));
            i += 1;
        }
    }
    out
}

/// Lexes the string literal whose opening quote is at `i`.
fn lex_string(c: &[char], i: usize) -> (String, usize) {
    let mut value = String::new();
    let mut j = i + 1;
    while j < c.len() {
        match c[j] {
            '\\' => {
                if let Some(&escaped) = c.get(j + 1) {
                    value.push(escaped);
                }
                j += 2;
            }
            '"' => return (value, j + 1),
            other => {
                value.push(other);
                j += 1;
            }
        }
    }
    (value, c.len())
}

/// Lexes a literal with a letter prefix: `b"..."`, `c"..."`, `r#"..."#`,
/// `br"..."`, `cr"..."` or `b'x'`. Returns `None` for an ordinary identifier.
fn lex_prefixed_literal(c: &[char], i: usize, out: &mut Vec<Tok>) -> Option<usize> {
    let n = c.len();
    let mut j = i;
    if c[j] == 'b' || c[j] == 'c' {
        j += 1;
    }
    let raw = c.get(j) == Some(&'r');
    if raw {
        j += 1;
    }
    if j == i {
        return None;
    }
    if raw {
        let mut hashes = 0;
        while c.get(j + hashes) == Some(&'#') {
            hashes += 1;
        }
        let open = j + hashes;
        if c.get(open) != Some(&'"') {
            return None;
        }
        let start = open + 1;
        let mut k = start;
        while k < n {
            if c[k] == '"' && (1..=hashes).all(|h| c.get(k + h) == Some(&'#')) {
                break;
            }
            k += 1;
        }
        out.push(Tok::Str(c[start..k.min(n)].iter().collect()));
        return Some((k + 1 + hashes).min(n));
    }
    match c.get(j) {
        Some('"') => {
            let (value, end) = lex_string(c, j);
            out.push(Tok::Str(value));
            Some(end)
        }
        Some('\'') if c[i] == 'b' => {
            let mut k = j + if c.get(j + 1) == Some(&'\\') { 3 } else { 2 };
            while k < n && c[k] != '\'' {
                k += 1;
            }
            out.push(Tok::Other);
            Some((k + 1).min(n))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Module tree and name resolution
// ---------------------------------------------------------------------------

struct Module {
    file: FileId,
    parent: Option<usize>,
    /// Root module of the crate this module belongs to.
    root: usize,
    children: BTreeMap<String, usize>,
    scope: Scope,
}

/// Where a path leads.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Res {
    Module(usize),
    /// An item of `module`. `exact` is false when the name was not found
    /// there and the module is merely where the trail ended.
    Item {
        module: usize,
        exact: bool,
    },
    /// Something outside the project, or nothing that can be resolved.
    Unknown,
}

enum Member {
    Module(usize),
    Item { module: usize, exact: bool },
    Foreign,
    NotFound,
}

#[derive(Default)]
struct Guard {
    depth: usize,
    /// `use` leaves that are being resolved, as (module, leaf index).
    leaves: Vec<(usize, usize)>,
    /// Member lookups in progress.
    members: Vec<(usize, String)>,
}

struct Manifest {
    dir: String,
    lib_name: Option<String>,
    lib_path: Option<String>,
    bin_paths: Vec<String>,
}

struct Tree<'a> {
    repo: &'a Repo,
    parsed: BTreeMap<FileId, Scope>,
    modules: Vec<Module>,
    /// Library crates by name, as other crates refer to them.
    libs: BTreeMap<String, usize>,
    /// Macros of each crate by name, keyed by (crate root, macro name).
    macros: BTreeMap<(usize, String), usize>,
    test_files: BTreeSet<FileId>,
}

/// Fills in the dependencies of every Rust file and returns the files that
/// are declared as test modules with `#[cfg(test)] mod name;`.
pub(crate) fn analyze(repo: &Repo, deps: &mut [FileDeps]) -> BTreeSet<FileId> {
    let mut tree = Tree {
        repo,
        parsed: BTreeMap::new(),
        modules: Vec::new(),
        libs: BTreeMap::new(),
        macros: BTreeMap::new(),
        test_files: BTreeSet::new(),
    };
    for (id, file) in repo.files.iter().enumerate() {
        if file.language == Language::Rust {
            tree.parsed.insert(id, parse(&file.text));
        }
    }
    tree.build();
    tree.collect(deps);
    tree.test_files
}

impl Tree<'_> {
    fn lookup(&self, path: &str) -> Option<FileId> {
        self.repo
            .by_path
            .get(path)
            .copied()
            .filter(|id| self.parsed.contains_key(id))
    }

    /// Builds the module tree of every crate, from its root file down.
    fn build(&mut self) {
        let mut roots: Vec<(FileId, Option<String>)> = Vec::new();
        let mut rooted = BTreeSet::new();
        let mut add_root = |roots: &mut Vec<(FileId, Option<String>)>,
                            file: Option<FileId>,
                            name: Option<String>| {
            if let Some(file) = file {
                if rooted.insert(file) {
                    roots.push((file, name));
                }
            }
        };

        for manifest in self
            .repo
            .manifests
            .iter()
            .filter_map(|(path, text)| parse_manifest(path, text))
        {
            let in_package = |path: &str| join(&manifest.dir, path);
            let lib = self.lookup(&in_package(
                manifest.lib_path.as_deref().unwrap_or("src/lib.rs"),
            ));
            add_root(&mut roots, lib, manifest.lib_name.clone());
            add_root(&mut roots, self.lookup(&in_package("src/main.rs")), None);
            for path in &manifest.bin_paths {
                add_root(&mut roots, self.lookup(&in_package(path)), None);
            }
            for dir in ["src/bin", "examples"] {
                let base = in_package(dir);
                let prefix = format!("{base}/");
                for &file in self.parsed.keys() {
                    let Some(rest) = self.repo.files[file].path.strip_prefix(&prefix) else {
                        continue;
                    };
                    let direct = !rest.contains('/');
                    let nested_main = rest.ends_with("/main.rs") && rest.matches('/').count() == 1;
                    if direct || nested_main {
                        add_root(&mut roots, Some(file), None);
                    }
                }
            }
        }
        // Crates without a manifest in the repository are found by the
        // conventional names of their root files.
        for &file in self.parsed.keys() {
            if matches!(file_name(&self.repo.files[file].path), "lib.rs" | "main.rs") {
                add_root(&mut roots, Some(file), None);
            }
        }

        for (file, name) in roots {
            let root = self.modules.len();
            let dir = dir_of(&self.repo.files[file].path).to_string();
            let scope = self.parsed[&file].clone();
            self.add_module(scope, file, None, root, dir, &mut vec![file]);
            if let Some(name) = name {
                self.libs.entry(name).or_insert(root);
            }
        }

        for (id, module) in self.modules.iter().enumerate() {
            for name in module.scope.macros.keys() {
                self.macros.entry((module.root, name.clone())).or_insert(id);
            }
        }
    }

    /// Adds a module and, recursively, the modules it declares. `dir` is the
    /// directory in which the files of its child modules are looked up.
    fn add_module(
        &mut self,
        scope: Scope,
        file: FileId,
        parent: Option<usize>,
        root: usize,
        dir: String,
        stack: &mut Vec<FileId>,
    ) -> usize {
        let id = self.modules.len();
        let inline = scope.inline.clone();
        let external = scope.external.clone();
        self.modules.push(Module {
            file,
            parent,
            root,
            children: BTreeMap::new(),
            scope,
        });

        for child in inline {
            let name = child.name.clone();
            let child_dir = join(&dir, &name);
            let child_id = self.add_module(child, file, Some(id), root, child_dir, stack);
            self.modules[id].children.insert(name, child_id);
        }
        for declared in external {
            let Some((child_file, child_dir)) = self.module_file(file, &dir, &declared) else {
                continue;
            };
            if declared.cfg_test {
                self.mark_test(child_file, &child_dir);
                continue;
            }
            if stack.contains(&child_file) {
                continue;
            }
            stack.push(child_file);
            let scope = self.parsed[&child_file].clone();
            let child_id = self.add_module(scope, child_file, Some(id), root, child_dir, stack);
            stack.pop();
            self.modules[id].children.insert(declared.name, child_id);
        }
        id
    }

    /// Finds the file of `mod name;` and the directory of its own children.
    fn module_file(
        &self,
        from: FileId,
        dir: &str,
        declared: &ExternalMod,
    ) -> Option<(FileId, String)> {
        if let Some(path) = &declared.path_attr {
            let path = normalize(
                dir_of(&self.repo.files[from].path),
                &path.replace('\\', "/"),
            )?;
            let file = self.lookup(&path)?;
            let child_dir = if file_name(&path) == "mod.rs" {
                dir_of(&path).to_string()
            } else {
                join(dir_of(&path), split_ext(file_name(&path)).0)
            };
            return Some((file, child_dir));
        }
        let child_dir = join(dir, &declared.name);
        let file = self
            .lookup(&format!("{child_dir}.rs"))
            .or_else(|| self.lookup(&format!("{child_dir}/mod.rs")))?;
        Some((file, child_dir))
    }

    /// Marks a file and the module files it declares as test code.
    fn mark_test(&mut self, file: FileId, dir: &str) {
        if !self.test_files.insert(file) {
            return;
        }
        for declared in self.parsed[&file].external.clone() {
            if let Some((child, child_dir)) = self.module_file(file, dir, &declared) {
                self.mark_test(child, &child_dir);
            }
        }
    }

    /// Resolves a path written in the module `scope`.
    fn resolve(&self, scope: usize, absolute: bool, segments: &[String], guard: &mut Guard) -> Res {
        if segments.is_empty() || guard.depth >= MAX_DEPTH {
            return Res::Unknown;
        }
        guard.depth += 1;
        let result = self.resolve_inner(scope, absolute, segments, guard);
        guard.depth -= 1;
        result
    }

    fn resolve_inner(
        &self,
        scope: usize,
        absolute: bool,
        segments: &[String],
        guard: &mut Guard,
    ) -> Res {
        let module = &self.modules[scope];
        let first = segments[0].as_str();
        let mut index = 1;
        let mut current = if absolute {
            match self.libs.get(first) {
                Some(&root) => root,
                None => return Res::Unknown,
            }
        } else if first == "crate" {
            module.root
        } else if first == "self" {
            scope
        } else if first == "super" {
            let mut current = scope;
            index = 0;
            while segments.get(index).map(String::as_str) == Some("super") {
                match self.modules[current].parent {
                    Some(parent) => current = parent,
                    None => return Res::Unknown,
                }
                index += 1;
            }
            current
        } else if let Some(&child) = module.children.get(first) {
            child
        } else {
            match self.resolve_first(scope, first, guard) {
                Res::Module(found) => found,
                other => return other,
            }
        };

        while index < segments.len() {
            match self.member(current, &segments[index], guard) {
                Member::Module(next) => {
                    current = next;
                    index += 1;
                }
                Member::Item { module, exact } => return Res::Item { module, exact },
                Member::Foreign => return Res::Unknown,
                Member::NotFound => {
                    return Res::Item {
                        module: current,
                        exact: false,
                    }
                }
            }
        }
        Res::Module(current)
    }

    /// Resolves the first segment of a path that is not a keyword or a child
    /// module: an imported name, a local item, another crate of the project,
    /// or a name brought in by a glob import.
    fn resolve_first(&self, scope: usize, first: &str, guard: &mut Guard) -> Res {
        let module = &self.modules[scope];
        for (index, leaf) in module.scope.uses.iter().enumerate().rev() {
            if leaf.glob || leaf.name != first || guard.leaves.contains(&(scope, index)) {
                continue;
            }
            guard.leaves.push((scope, index));
            let found = self.resolve(scope, leaf.absolute, &leaf.segments, guard);
            guard.leaves.pop();
            return found;
        }
        if module.scope.defs.contains(first) {
            return Res::Unknown;
        }
        if let Some(&root) = self.libs.get(first) {
            return Res::Module(root);
        }
        for (index, leaf) in module.scope.uses.iter().enumerate() {
            if !leaf.glob || guard.leaves.contains(&(scope, index)) {
                continue;
            }
            guard.leaves.push((scope, index));
            let target = self.resolve(scope, leaf.absolute, &leaf.segments, guard);
            guard.leaves.pop();
            if let Res::Module(target) = target {
                match self.member(target, first, guard) {
                    Member::Module(found) => return Res::Module(found),
                    Member::Item {
                        module,
                        exact: true,
                    } => {
                        return Res::Item {
                            module,
                            exact: true,
                        }
                    }
                    _ => {}
                }
            }
        }
        Res::Unknown
    }

    /// Finds what `name` is inside the module `module`.
    fn member(&self, module: usize, name: &str, guard: &mut Guard) -> Member {
        let key = (module, name.to_string());
        if guard.depth >= MAX_DEPTH || guard.members.contains(&key) {
            return Member::NotFound;
        }
        guard.members.push(key);
        guard.depth += 1;
        let found = self.member_inner(module, name, guard);
        guard.depth -= 1;
        guard.members.pop();
        found
    }

    fn member_inner(&self, id: usize, name: &str, guard: &mut Guard) -> Member {
        let module = &self.modules[id];
        if let Some(&child) = module.children.get(name) {
            return Member::Module(child);
        }
        if module.scope.defs.contains(name) {
            return Member::Item {
                module: id,
                exact: true,
            };
        }
        if module.parent.is_none() {
            // Exported macros live at the crate root, wherever they are defined.
            if let Some(&home) = self.macros.get(&(id, name.to_string())) {
                if self.modules[home].scope.macros.get(name) == Some(&true) {
                    return Member::Item {
                        module: home,
                        exact: true,
                    };
                }
            }
        }
        for (index, leaf) in module.scope.uses.iter().enumerate().rev() {
            if leaf.glob || leaf.name != name || guard.leaves.contains(&(id, index)) {
                continue;
            }
            guard.leaves.push((id, index));
            let found = self.resolve(id, leaf.absolute, &leaf.segments, guard);
            guard.leaves.pop();
            return match found {
                Res::Module(found) => Member::Module(found),
                Res::Item { module, exact } => Member::Item { module, exact },
                Res::Unknown => Member::Foreign,
            };
        }
        for (index, leaf) in module.scope.uses.iter().enumerate() {
            if !leaf.glob || guard.leaves.contains(&(id, index)) {
                continue;
            }
            guard.leaves.push((id, index));
            let target = self.resolve(id, leaf.absolute, &leaf.segments, guard);
            guard.leaves.pop();
            if let Res::Module(target) = target {
                let found = self.member(target, name, guard);
                if matches!(found, Member::Module(_) | Member::Item { exact: true, .. }) {
                    return found;
                }
            }
        }
        Member::NotFound
    }

    /// Turns the resolved imports and paths of every module into the
    /// dependencies of the file that holds it.
    fn collect(&self, deps: &mut [FileDeps]) {
        for (id, module) in self.modules.iter().enumerate() {
            let file_deps = &mut deps[module.file];
            file_deps.analyzed = true;
            let file_of = |module: usize| self.modules[module].file;

            for (index, leaf) in module.scope.uses.iter().enumerate() {
                let mut guard = Guard {
                    leaves: vec![(id, index)],
                    ..Guard::default()
                };
                let target = match self.resolve(id, leaf.absolute, &leaf.segments, &mut guard) {
                    Res::Module(target) => {
                        // A module is only a namespace; what the code takes
                        // from it shows up as paths. A glob makes all of it
                        // visible.
                        if leaf.glob {
                            file_deps.open.insert(file_of(target));
                        }
                        file_of(target)
                    }
                    Res::Item {
                        module: home,
                        exact,
                    } => {
                        // A private import is a use. A public one is a
                        // re-export unless the code also uses the name.
                        let used = !leaf.is_pub
                            || (!leaf.glob && module.scope.idents.contains(&leaf.name));
                        if used {
                            file_deps.add_use(file_of(home), exact);
                        }
                        file_of(home)
                    }
                    Res::Unknown => continue,
                };
                file_deps.imports.insert(target);
                if leaf.is_pub {
                    file_deps.reexports.insert(target);
                }
            }

            for path in &module.scope.paths {
                match self.resolve(id, path.absolute, &path.segments, &mut Guard::default()) {
                    Res::Item {
                        module: home,
                        exact,
                    } => file_deps.add_use(file_of(home), exact),
                    Res::Module(target) => {
                        file_deps.imports.insert(file_of(target));
                    }
                    Res::Unknown => {}
                }
            }

            for name in &module.scope.macro_calls {
                if module.scope.macros.contains_key(name) {
                    continue;
                }
                if let Some(&home) = self.macros.get(&(module.root, name.clone())) {
                    file_deps.add_use(file_of(home), true);
                }
            }
        }
    }
}

/// Reads the package name and target paths from a `Cargo.toml`. Returns
/// `None` for a manifest without a `[package]` table.
fn parse_manifest(path: &str, text: &str) -> Option<Manifest> {
    let mut section = String::new();
    let mut package = None;
    let mut lib_name = None;
    let mut lib_path = None;
    let mut bin_paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line
                .trim_matches(|c| c == '[' || c == ']')
                .trim()
                .to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let Some(value) = value.strip_prefix('"').and_then(|v| v.split('"').next()) else {
            continue;
        };
        match (section.as_str(), key.trim()) {
            ("package", "name") => package = Some(value.to_string()),
            ("lib", "name") => lib_name = Some(value.to_string()),
            ("lib", "path") => lib_path = Some(value.to_string()),
            ("bin", "path") => bin_paths.push(value.to_string()),
            _ => {}
        }
    }
    let package = package?;
    Some(Manifest {
        dir: dir_of(path).to_string(),
        lib_name: Some(lib_name.unwrap_or(package).replace('-', "_")),
        lib_path,
        bin_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lexes_literals_comments_and_lifetimes() {
        let toks = lex("a::b /* x /* y */ z */ 'a' '\\'' 'life r#\"raw \" text\"# b\"bytes\" r#type 1.5 x.0 // tail\n");
        assert_eq!(
            toks,
            vec![
                Tok::Ident("a".into()),
                Tok::Sep,
                Tok::Ident("b".into()),
                Tok::Other,
                Tok::Other,
                Tok::Other,
                Tok::Str("raw \" text".into()),
                Tok::Str("bytes".into()),
                Tok::Ident("type".into()),
                Tok::Other,
                Tok::Ident("x".into()),
                Tok::Punct('.'),
                Tok::Other,
            ]
        );
    }

    #[test]
    fn flattens_use_trees() {
        let scope = parse(
            "pub use crate::a::{self, b::C as D, e::*};\nuse ::other::Thing;\nuse super::x as _;\n",
        );
        let leaf = |is_pub, absolute, segments: &[&str], glob, name: &str| UseLeaf {
            is_pub,
            absolute,
            segments: strings(segments),
            glob,
            name: name.to_string(),
        };
        assert_eq!(
            scope.uses,
            vec![
                leaf(true, false, &["crate", "a"], false, "a"),
                leaf(true, false, &["crate", "a", "b", "C"], false, "D"),
                leaf(true, false, &["crate", "a", "e"], true, ""),
                leaf(false, true, &["other", "Thing"], false, "Thing"),
                leaf(false, false, &["super", "x"], false, "_"),
            ]
        );
    }

    #[test]
    fn records_modules_definitions_and_paths() {
        let scope = parse(
            "mod a;\n#[path = \"other.rs\"]\npub(crate) mod b;\nmod c { pub fn inner() {} }\npub struct S;\nconst fn make() {}\nstatic mut COUNT: u32 = 0;\nimpl S { fn method(&self) { crate::a::run(); self.field.call(); Vec::<u8>::new(); } }\n#[macro_export]\nmacro_rules! shout { () => {} }\nfn f() { shout!(); log!(); if a != b {} }\n",
        );
        assert_eq!(scope.external.len(), 2);
        assert_eq!(scope.external[1].path_attr.as_deref(), Some("other.rs"));
        assert_eq!(scope.inline.len(), 1);
        assert!(scope.inline[0].defs.contains("inner"));
        for name in ["a", "b", "c", "S", "make", "COUNT", "f"] {
            assert!(scope.defs.contains(name), "{name} should be defined");
        }
        assert!(!scope.defs.contains("method"));
        assert_eq!(scope.macros.get("shout"), Some(&true));
        assert_eq!(
            scope.paths,
            vec![PathRef {
                absolute: false,
                segments: strings(&["crate", "a", "run"])
            }]
        );
        assert!(scope.macro_calls.contains("shout") && scope.macro_calls.contains("log"));
        assert!(
            !scope.idents.contains("field")
                && !scope.idents.contains("call")
                && !scope.idents.contains("new")
        );
    }

    #[test]
    fn skips_test_code() {
        let scope = parse(
            "use crate::real::Thing;\n#[cfg(test)]\nmod tests {\n    use super::*;\n    use crate::fake::Double;\n    #[test]\n    fn works() { crate::fake::run(); }\n}\n#[cfg(test)]\nuse crate::fake::{A, B};\n#[cfg(test)]\nmod helpers;\n#[test]\nfn loose() { crate::fake::run(); }\n#[cfg(not(test))]\nfn kept() { crate::real::run(); }\n",
        );
        assert_eq!(scope.uses.len(), 1);
        assert!(scope.inline.is_empty());
        assert_eq!(
            scope.external,
            vec![ExternalMod {
                name: "helpers".into(),
                path_attr: None,
                cfg_test: true
            }]
        );
        assert_eq!(
            scope.paths,
            vec![PathRef {
                absolute: false,
                segments: strings(&["crate", "real", "run"])
            }]
        );
        assert!(scope.defs.contains("kept") && !scope.defs.contains("loose"));
    }

    #[test]
    fn reads_manifests() {
        let manifest = parse_manifest(
            "tools/gen/Cargo.toml",
            "[package]\nname = \"my-gen\" # comment\nversion = \"0.1.0\"\n\n[lib]\npath = \"lib/root.rs\"\n\n[[bin]]\nname = \"gen\"\npath = \"cli/main.rs\"\n",
        )
        .unwrap();
        assert_eq!(manifest.dir, "tools/gen");
        assert_eq!(manifest.lib_name.as_deref(), Some("my_gen"));
        assert_eq!(manifest.lib_path.as_deref(), Some("lib/root.rs"));
        assert_eq!(manifest.bin_paths, vec!["cli/main.rs".to_string()]);
        assert!(parse_manifest("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n").is_none());
    }
}
