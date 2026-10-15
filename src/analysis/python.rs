// Relationships:
// - `src/analysis/mod.rs`: Records the dependencies detected for each file
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads Python sources and their paths

//! Dependency detection for Python: import statements, followed through
//! package initialisers and other re-exports to the file that defines each
//! imported name.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::FileDeps;
use crate::paths::{dir_of, join, parent};
use crate::repo::{FileId, Language, Repo};

/// Statements that introduce an indented block.
const COMPOUND: &[&str] = &[
    "if", "elif", "else", "try", "except", "finally", "with", "for", "while", "def", "class",
    "async",
];
/// Compound statements whose body still belongs to the enclosing scope.
const TRANSPARENT: &[&str] = &[
    "if", "elif", "else", "try", "except", "finally", "with", "for", "while",
];
/// Limit on how many re-exports are followed for one name.
const MAX_HOPS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    Op(char),
    /// A string or number literal.
    Literal,
}

/// A logical line: one statement line after joining continuation lines.
struct Logical {
    indent: usize,
    toks: Vec<Tok>,
}

#[derive(Clone, Debug, PartialEq)]
enum Import {
    /// `import a.b.c` or `import a.b.c as alias`.
    Module {
        path: Vec<String>,
        alias: Option<String>,
    },
    /// `from ..module import name as alias, ...` or `from module import *`.
    From {
        level: usize,
        module: Vec<String>,
        names: Vec<(String, Option<String>)>,
        star: bool,
    },
}

#[derive(Debug, Default)]
struct PyFile {
    imports: Vec<Import>,
    /// Names defined at module level.
    defs: BTreeSet<String>,
    /// Dotted names used in the code, such as `storage.repo.save`.
    chains: BTreeSet<Vec<String>>,
}

/// A module that an import can name.
#[derive(Clone, Debug, PartialEq)]
enum PyMod {
    File(FileId),
    /// A package directory, with its `__init__.py` if it has one.
    Package {
        dir: String,
        init: Option<FileId>,
    },
}

impl PyMod {
    fn file(&self) -> Option<FileId> {
        match self {
            PyMod::File(file) => Some(*file),
            PyMod::Package { init, .. } => *init,
        }
    }
}

/// What a name inside a module turns out to be.
enum Attr {
    Module(PyMod),
    /// Defined in `file`, or last traced to it when `exact` is false.
    Definition {
        file: FileId,
        exact: bool,
    },
    /// Imported from outside the project.
    Foreign,
    NotFound,
}

enum Binding {
    Module(Option<PyMod>),
    Item(Option<PyMod>, String),
}

/// Fills in the dependencies of every Python file.
pub(crate) fn analyze(repo: &Repo, search_paths: &[String], deps: &mut [FileDeps]) {
    let mut parsed = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    for (id, file) in repo.files.iter().enumerate() {
        if file.language != Language::Python {
            continue;
        }
        parsed.insert(id, parse(&file.text));
        let mut dir = Some(dir_of(&file.path));
        while let Some(current) = dir {
            dirs.insert(current.to_string());
            dir = parent(current);
        }
    }
    let python = Python {
        repo,
        parsed,
        dirs,
        search_paths,
        suffix_cache: RefCell::new(BTreeMap::new()),
    };
    for &id in python.parsed.keys() {
        deps[id] = python.file_deps(id);
    }
}

struct Python<'a> {
    repo: &'a Repo,
    parsed: BTreeMap<FileId, PyFile>,
    /// Directories that contain a Python file at any depth.
    dirs: BTreeSet<String>,
    search_paths: &'a [String],
    suffix_cache: RefCell<BTreeMap<String, Option<PyMod>>>,
}

impl Python<'_> {
    fn lookup(&self, path: &str) -> Option<FileId> {
        self.repo
            .by_path
            .get(path)
            .copied()
            .filter(|id| self.parsed.contains_key(id))
    }

    fn package_at(&self, dir: &str) -> PyMod {
        PyMod::Package {
            dir: dir.to_string(),
            init: self.lookup(&join(dir, "__init__.py")),
        }
    }

    /// The module `segments` under the directory `base`, if the project has it.
    fn module_at(&self, base: &str, segments: &[String]) -> Option<PyMod> {
        if segments.is_empty() {
            return Some(self.package_at(base));
        }
        let path = join(base, &segments.join("/"));
        if let Some(init) = self.lookup(&format!("{path}/__init__.py")) {
            return Some(PyMod::Package {
                dir: path,
                init: Some(init),
            });
        }
        if let Some(file) = self.lookup(&format!("{path}.py")) {
            return Some(PyMod::File(file));
        }
        self.dirs.contains(&path).then_some(PyMod::Package {
            dir: path,
            init: None,
        })
    }

    /// The directory that the top-level package of `path` sits in.
    fn package_root(&self, path: &str) -> String {
        let mut dir = dir_of(path);
        while self.lookup(&join(dir, "__init__.py")).is_some() {
            match parent(dir) {
                Some(up) => dir = up,
                None => break,
            }
        }
        dir.to_string()
    }

    /// Resolves an absolute import. Roots are tried in a fixed order: the
    /// configured search paths, the root of the importing file's package, the
    /// repository root and `src`. A dotted name that still has no match is
    /// accepted when exactly one file in the project ends with its path.
    fn absolute(&self, from: FileId, segments: &[String]) -> Option<PyMod> {
        if segments.is_empty() {
            return None;
        }
        let mut roots: Vec<String> = self.search_paths.to_vec();
        roots.push(self.package_root(&self.repo.files[from].path));
        roots.push(String::new());
        roots.push("src".to_string());
        let mut tried = BTreeSet::new();
        for root in roots {
            if tried.insert(root.clone()) {
                if let Some(module) = self.module_at(&root, segments) {
                    return Some(module);
                }
            }
        }
        if segments.len() >= 2 {
            return self.by_suffix(segments);
        }
        None
    }

    fn by_suffix(&self, segments: &[String]) -> Option<PyMod> {
        let key = segments.join("/");
        if let Some(hit) = self.suffix_cache.borrow().get(&key) {
            return hit.clone();
        }
        let module = format!("/{key}.py");
        let init = format!("/{key}/__init__.py");
        let mut hits = Vec::new();
        for &id in self.parsed.keys() {
            let path = &self.repo.files[id].path;
            if path.ends_with(&module) {
                hits.push(PyMod::File(id));
            } else if path.ends_with(&init) {
                hits.push(PyMod::Package {
                    dir: dir_of(path).to_string(),
                    init: Some(id),
                });
            }
        }
        let result = if hits.len() == 1 { hits.pop() } else { None };
        self.suffix_cache.borrow_mut().insert(key, result.clone());
        result
    }

    /// The module a `from` import takes its names from.
    fn source_module(&self, from: FileId, level: usize, module: &[String]) -> Option<PyMod> {
        if level == 0 {
            return self.absolute(from, module);
        }
        let mut base = dir_of(&self.repo.files[from].path);
        for _ in 1..level {
            base = parent(base)?;
        }
        self.module_at(base, module)
    }

    /// Finds what `name` is inside `module`, following re-exports.
    fn attr(&self, module: &PyMod, name: &str, trail: &mut Vec<(FileId, String)>) -> Attr {
        let Some(file) = module.file() else {
            return self
                .submodule(module, name)
                .map_or(Attr::NotFound, Attr::Module);
        };
        let key = (file, name.to_string());
        if trail.len() >= MAX_HOPS || trail.contains(&key) {
            return Attr::NotFound;
        }
        trail.push(key);
        let found = self.attr_in_file(module, file, name, trail);
        trail.pop();
        found
    }

    fn attr_in_file(
        &self,
        module: &PyMod,
        file: FileId,
        name: &str,
        trail: &mut Vec<(FileId, String)>,
    ) -> Attr {
        let parsed = &self.parsed[&file];
        if parsed.defs.contains(name) {
            return Attr::Definition { file, exact: true };
        }
        // The last import that binds the name wins, as it does at run time.
        for import in parsed.imports.iter().rev() {
            match import {
                Import::Module { path, alias } => {
                    let (bound, target) = match alias {
                        Some(alias) => (alias, &path[..]),
                        None => (&path[0], &path[..1]),
                    };
                    if bound == name {
                        return self
                            .absolute(file, target)
                            .map_or(Attr::Foreign, Attr::Module);
                    }
                }
                Import::From {
                    level,
                    module: source,
                    names,
                    ..
                } => {
                    for (original, alias) in names {
                        if alias.as_ref().unwrap_or(original) != name {
                            continue;
                        }
                        let Some(source) = self.source_module(file, *level, source) else {
                            return Attr::Foreign;
                        };
                        return match self.attr(&source, original, trail) {
                            Attr::NotFound => match source.file() {
                                Some(file) => Attr::Definition { file, exact: false },
                                None => Attr::NotFound,
                            },
                            found => found,
                        };
                    }
                }
            }
        }
        if let Some(sub) = self.submodule(module, name) {
            return Attr::Module(sub);
        }
        for import in &parsed.imports {
            if let Import::From {
                level,
                module: source,
                star: true,
                ..
            } = import
            {
                if let Some(source) = self.source_module(file, *level, source) {
                    let found = self.attr(&source, name, trail);
                    if matches!(
                        found,
                        Attr::Module(_) | Attr::Definition { exact: true, .. }
                    ) {
                        return found;
                    }
                }
            }
        }
        Attr::NotFound
    }

    fn submodule(&self, module: &PyMod, name: &str) -> Option<PyMod> {
        match module {
            PyMod::Package { dir, .. } => self.module_at(dir, &[name.to_string()]),
            PyMod::File(_) => None,
        }
    }

    fn file_deps(&self, file: FileId) -> FileDeps {
        let parsed = &self.parsed[&file];
        let heads: BTreeSet<&str> = parsed
            .chains
            .iter()
            .map(|chain| chain[0].as_str())
            .collect();
        let mut deps = FileDeps {
            analyzed: true,
            ..FileDeps::default()
        };
        let mut bindings: BTreeMap<&str, Vec<Binding>> = BTreeMap::new();

        for import in &parsed.imports {
            match import {
                Import::Module { path, alias } => {
                    let full = self.absolute(file, path);
                    let target = full.as_ref().and_then(PyMod::file);
                    let (bound, module) = match alias {
                        Some(alias) => (alias.as_str(), full),
                        None => (path[0].as_str(), self.absolute(file, &path[..1])),
                    };
                    if let Some(target) = target {
                        deps.imports.insert(target);
                        if !heads.contains(bound) {
                            deps.reexports.insert(target);
                        }
                    }
                    bindings
                        .entry(bound)
                        .or_default()
                        .push(Binding::Module(module));
                }
                Import::From {
                    level,
                    module,
                    names,
                    star,
                } => {
                    let source = self.source_module(file, *level, module);
                    if let Some(target) = source.as_ref().and_then(PyMod::file) {
                        deps.imports.insert(target);
                        if *star {
                            deps.open.insert(target);
                            deps.reexports.insert(target);
                        }
                    }
                    for (original, alias) in names {
                        let bound = alias.as_ref().unwrap_or(original).as_str();
                        let target = source.as_ref().and_then(|source| {
                            match self.attr(source, original, &mut Vec::new()) {
                                Attr::Module(module) => module.file(),
                                Attr::Definition { file, .. } => Some(file),
                                Attr::Foreign | Attr::NotFound => None,
                            }
                        });
                        if let Some(target) = target {
                            deps.imports.insert(target);
                            if !heads.contains(bound) {
                                deps.reexports.insert(target);
                            }
                        }
                        bindings
                            .entry(bound)
                            .or_default()
                            .push(Binding::Item(source.clone(), original.clone()));
                    }
                }
            }
        }

        // An import is a dependency only where the code goes on to use the
        // name it binds; an import that is never used is a re-export.
        for chain in &parsed.chains {
            let Some(bound) = bindings.get(chain[0].as_str()) else {
                continue;
            };
            for binding in bound {
                match binding {
                    Binding::Module(Some(module)) => self.walk(module, &chain[1..], &mut deps),
                    Binding::Item(Some(module), name) => {
                        match self.attr(module, name, &mut Vec::new()) {
                            Attr::Module(inner) => self.walk(&inner, &chain[1..], &mut deps),
                            Attr::Definition { file, exact } => deps.add_use(file, exact),
                            Attr::NotFound => {
                                if let Some(file) = module.file() {
                                    deps.add_use(file, false);
                                }
                            }
                            Attr::Foreign => {}
                        }
                    }
                    Binding::Module(None) | Binding::Item(None, _) => {}
                }
            }
        }
        deps
    }

    /// Follows the rest of a dotted name through modules until it reaches a
    /// definition, and records the file that holds it.
    fn walk(&self, module: &PyMod, rest: &[String], deps: &mut FileDeps) {
        let mut current = module.clone();
        for name in rest {
            match self.attr(&current, name, &mut Vec::new()) {
                Attr::Module(inner) => current = inner,
                Attr::Definition { file, exact } => {
                    deps.add_use(file, exact);
                    return;
                }
                Attr::NotFound => {
                    if let Some(file) = current.file() {
                        deps.add_use(file, false);
                    }
                    return;
                }
                Attr::Foreign => return,
            }
        }
        if let Some(file) = current.file() {
            deps.add_use(file, true);
        }
    }
}

fn parse(text: &str) -> PyFile {
    let mut out = PyFile::default();
    // Statements nested one level inside a top-level `if` or `try` still
    // define module-level names.
    let mut top_is_transparent = false;
    let mut nested_indent = None;
    for line in lex(text) {
        let first = match line.toks.first() {
            Some(Tok::Name(word)) => word.as_str(),
            _ => "",
        };
        let module_level = if line.indent == 0 {
            top_is_transparent = TRANSPARENT.contains(&first);
            nested_indent = None;
            true
        } else {
            if top_is_transparent && nested_indent.is_none() {
                nested_indent = Some(line.indent);
            }
            top_is_transparent && nested_indent == Some(line.indent)
        };

        let mut depth = 0usize;
        let mut start = 0;
        for (i, tok) in line.toks.iter().enumerate() {
            match tok {
                Tok::Op('(' | '[' | '{') => depth += 1,
                Tok::Op(')' | ']' | '}') => depth = depth.saturating_sub(1),
                Tok::Op(';') if depth == 0 => {
                    statement(&line.toks[start..i], module_level, &mut out);
                    start = i + 1;
                }
                _ => {}
            }
        }
        statement(&line.toks[start..], module_level, &mut out);
    }
    out
}

fn statement(toks: &[Tok], module_level: bool, out: &mut PyFile) {
    let Some(Tok::Name(first)) = toks.first() else {
        code(toks, module_level, out);
        return;
    };
    if first == "import" {
        if let Some(mut imports) = parse_import(&toks[1..]) {
            out.imports.append(&mut imports);
            return;
        }
    }
    if first == "from" {
        if let Some(import) = parse_from(&toks[1..]) {
            out.imports.push(import);
            return;
        }
    }
    if COMPOUND.contains(&first.as_str()) {
        if let Some(colon) = header_end(toks) {
            code(&toks[..colon], module_level, out);
            if colon + 1 < toks.len() {
                let inner = module_level && TRANSPARENT.contains(&first.as_str());
                statement(&toks[colon + 1..], inner, out);
            }
            return;
        }
    }
    code(toks, module_level, out);
}

/// Position of the colon that ends a compound statement's header.
fn header_end(toks: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Op('(' | '[' | '{') => depth += 1,
            Tok::Op(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Tok::Op(':') if depth == 0 && toks.get(i + 1) != Some(&Tok::Op('=')) => return Some(i),
            _ => {}
        }
    }
    None
}

/// Records the names a statement defines and the dotted names it uses.
fn code(toks: &[Tok], module_level: bool, out: &mut PyFile) {
    if module_level {
        match toks {
            [Tok::Name(keyword), Tok::Name(name), ..] if keyword == "def" || keyword == "class" => {
                out.defs.insert(name.clone());
            }
            [Tok::Name(first), Tok::Name(keyword), Tok::Name(name), ..]
                if first == "async" && keyword == "def" =>
            {
                out.defs.insert(name.clone());
            }
            [Tok::Name(keyword), Tok::Name(name), Tok::Op('=' | '['), ..] if keyword == "type" => {
                out.defs.insert(name.clone());
            }
            _ => out.defs.extend(assigned_names(toks)),
        }
    }

    let mut i = 0;
    while i < toks.len() {
        let Tok::Name(name) = &toks[i] else {
            i += 1;
            continue;
        };
        let attribute = i > 0 && toks[i - 1] == Tok::Op('.');
        let mut chain = vec![name.clone()];
        let mut next = i + 1;
        while let (Some(Tok::Op('.')), Some(Tok::Name(part))) = (toks.get(next), toks.get(next + 1))
        {
            chain.push(part.clone());
            next += 2;
        }
        if !attribute {
            out.chains.insert(chain);
        }
        i = next;
    }
}

/// Names bound by `a = ...`, `a, b = ...` or `a: T = ...`.
fn assigned_names(toks: &[Tok]) -> Vec<String> {
    let mut names = Vec::new();
    let mut i = 0;
    loop {
        match toks.get(i) {
            Some(Tok::Name(name)) => names.push(name.clone()),
            _ => return Vec::new(),
        }
        match toks.get(i + 1) {
            Some(Tok::Op(',')) => i += 2,
            Some(Tok::Op('=')) if toks.get(i + 2) != Some(&Tok::Op('=')) => return names,
            Some(Tok::Op(':')) if names.len() == 1 => return names,
            _ => return Vec::new(),
        }
    }
}

fn dotted(toks: &[Tok], mut i: usize) -> Option<(Vec<String>, usize)> {
    let mut path = Vec::new();
    loop {
        match toks.get(i) {
            Some(Tok::Name(name)) => path.push(name.clone()),
            _ => return None,
        }
        i += 1;
        if toks.get(i) == Some(&Tok::Op('.')) {
            i += 1;
        } else {
            return Some((path, i));
        }
    }
}

fn alias_at(toks: &[Tok], i: usize) -> Option<String> {
    match (toks.get(i), toks.get(i + 1)) {
        (Some(Tok::Name(keyword)), Some(Tok::Name(alias))) if keyword == "as" => {
            Some(alias.clone())
        }
        _ => None,
    }
}

/// Parses what follows the `import` keyword.
fn parse_import(toks: &[Tok]) -> Option<Vec<Import>> {
    let mut imports = Vec::new();
    let mut i = 0;
    loop {
        let (path, next) = dotted(toks, i)?;
        i = next;
        let alias = alias_at(toks, i);
        if alias.is_some() {
            i += 2;
        }
        imports.push(Import::Module { path, alias });
        if toks.get(i) == Some(&Tok::Op(',')) {
            i += 1;
        } else {
            return Some(imports);
        }
    }
}

/// Parses what follows the `from` keyword.
fn parse_from(toks: &[Tok]) -> Option<Import> {
    let mut i = 0;
    let mut level = 0;
    while toks.get(i) == Some(&Tok::Op('.')) {
        level += 1;
        i += 1;
    }
    let is_import = |tok: Option<&Tok>| matches!(tok, Some(Tok::Name(word)) if word == "import");
    let module = if is_import(toks.get(i)) {
        Vec::new()
    } else {
        let (path, next) = dotted(toks, i)?;
        i = next;
        path
    };
    if (level == 0 && module.is_empty()) || !is_import(toks.get(i)) {
        return None;
    }
    i += 1;
    if toks.get(i) == Some(&Tok::Op('*')) {
        return Some(Import::From {
            level,
            module,
            names: Vec::new(),
            star: true,
        });
    }
    if toks.get(i) == Some(&Tok::Op('(')) {
        i += 1;
    }
    let mut names = Vec::new();
    loop {
        match toks.get(i) {
            Some(Tok::Name(name)) => {
                let alias = alias_at(toks, i + 1);
                i += if alias.is_some() { 3 } else { 1 };
                names.push((name.clone(), alias));
            }
            Some(Tok::Op(',')) => i += 1,
            _ => break,
        }
    }
    (!names.is_empty()).then_some(Import::From {
        level,
        module,
        names,
        star: false,
    })
}

/// Splits source text into logical lines of tokens, dropping comments and
/// the contents of string literals.
fn lex(text: &str) -> Vec<Logical> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut lines = Vec::new();
    let mut toks: Vec<Tok> = Vec::new();
    let mut indent = 0usize;
    let mut line_start = true;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < n {
        if line_start && toks.is_empty() && depth == 0 {
            let mut width = 0;
            while i < n && (c[i] == ' ' || c[i] == '\t') {
                width += if c[i] == '\t' { 8 } else { 1 };
                i += 1;
            }
            indent = width;
            line_start = false;
            continue;
        }
        let ch = c[i];
        match ch {
            '\n' => {
                i += 1;
                if depth == 0 {
                    if !toks.is_empty() {
                        lines.push(Logical {
                            indent,
                            toks: std::mem::take(&mut toks),
                        });
                    }
                    line_start = true;
                }
            }
            '\\' if c.get(i + 1) == Some(&'\n') => i += 2,
            '\\' if c.get(i + 1) == Some(&'\r') && c.get(i + 2) == Some(&'\n') => i += 3,
            '#' => {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                i = skip_string(&c, i);
                toks.push(Tok::Literal);
            }
            '(' | '[' | '{' => {
                depth += 1;
                toks.push(Tok::Op(ch));
                i += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                toks.push(Tok::Op(ch));
                i += 1;
            }
            _ if ch.is_whitespace() => i += 1,
            _ if ch.is_alphabetic() || ch == '_' => {
                let start = i;
                while i < n && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                let word: String = c[start..i].iter().collect();
                let prefix = word.len() <= 3 && word.chars().all(|p| "rbufRBUFtT".contains(p));
                if prefix && i < n && (c[i] == '"' || c[i] == '\'') {
                    i = skip_string(&c, i);
                    toks.push(Tok::Literal);
                } else {
                    toks.push(Tok::Name(word));
                }
            }
            _ if ch.is_ascii_digit() => {
                i += 1;
                while i < n
                    && (c[i].is_alphanumeric()
                        || c[i] == '_'
                        || (c[i] == '.' && c.get(i + 1).is_some_and(char::is_ascii_digit)))
                {
                    i += 1;
                }
                toks.push(Tok::Literal);
            }
            _ => {
                toks.push(Tok::Op(ch));
                i += 1;
            }
        }
    }
    if !toks.is_empty() {
        lines.push(Logical { indent, toks });
    }
    lines
}

/// Returns the index just past the string literal that starts at `i`.
fn skip_string(c: &[char], mut i: usize) -> usize {
    let n = c.len();
    let quote = c[i];
    let triple = c.get(i + 1) == Some(&quote) && c.get(i + 2) == Some(&quote);
    if triple {
        i += 3;
        while i < n {
            if c[i] == '\\' {
                i += 2;
            } else if c[i] == quote && c.get(i + 1) == Some(&quote) && c.get(i + 2) == Some(&quote)
            {
                return i + 3;
            } else {
                i += 1;
            }
        }
        return n;
    }
    i += 1;
    while i < n {
        match c[i] {
            '\\' => i += 2,
            '\n' => return i,
            ch if ch == quote => return i + 1,
            _ => i += 1,
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_import_forms() {
        let file = parse(
            "import os, app.storage.repo as repo\nfrom . import tokens\nfrom ..storage import (\n    SessionRepo,\n    cache as c,\n)\nfrom app.models import *\n",
        );
        assert_eq!(
            file.imports,
            vec![
                Import::Module {
                    path: names(&["os"]),
                    alias: None
                },
                Import::Module {
                    path: names(&["app", "storage", "repo"]),
                    alias: Some("repo".into())
                },
                Import::From {
                    level: 1,
                    module: vec![],
                    names: vec![("tokens".into(), None)],
                    star: false
                },
                Import::From {
                    level: 2,
                    module: names(&["storage"]),
                    names: vec![
                        ("SessionRepo".into(), None),
                        ("cache".into(), Some("c".into()))
                    ],
                    star: false,
                },
                Import::From {
                    level: 0,
                    module: names(&["app", "models"]),
                    names: vec![],
                    star: true
                },
            ]
        );
    }

    #[test]
    fn ignores_imports_in_strings_and_comments() {
        let file = parse("\"\"\"Docs.\n\nimport nothing\n\"\"\"\n# import nothing\nx = 'import nothing'\ny = f\"from a import b\"\n");
        assert!(file.imports.is_empty());
    }

    #[test]
    fn finds_imports_inside_blocks_and_one_liners() {
        let file = parse("try:\n    import fast\nexcept ImportError: import slow\ndef f():\n    from .lazy import thing; thing()\n");
        assert_eq!(file.imports.len(), 3);
    }

    #[test]
    fn collects_module_level_definitions() {
        let file = parse(
            "LIMIT = 3\nA, B = 1, 2\nname: str = 'x'\ndef run(): pass\nasync def serve(): pass\nclass Repo:\n    inner = 1\n    def method(self): pass\nif FLAG:\n    def fallback(): pass\n    if X:\n        deep = 1\nx == y\n",
        );
        let expected: BTreeSet<String> = [
            "LIMIT", "A", "B", "name", "run", "serve", "Repo", "fallback",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(file.defs, expected);
    }

    #[test]
    fn collects_dotted_names() {
        let file = parse("result = storage.repo.save(session.id)\n");
        assert!(file.chains.contains(&names(&["storage", "repo", "save"])));
        assert!(file.chains.contains(&names(&["session", "id"])));
        assert!(file.chains.contains(&names(&["result"])));
        assert!(!file.chains.contains(&names(&["repo", "save"])));
    }
}
