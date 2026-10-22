// Relationships:
// - `src/analysis/mod.rs`: Records the dependencies detected for each file
// - `src/analysis/tsconfig.rs`: Resolves module aliases from compiler settings
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads TypeScript sources and their paths

//! Dependency detection for TypeScript: `import` and `export … from`
//! declarations, resolved through relative paths and compiler settings, and
//! followed through re-exports, such as those of index files, to the file
//! that defines each imported name.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::tsconfig::Configs;
use super::FileDeps;
use crate::paths::{dir_of, file_name, join, normalize, split_ext};
use crate::repo::{FileId, Language, Repo};

/// Limit on how many re-exports are followed for one name.
const MAX_HOPS: usize = 32;
/// Extensions of the sources that a specifier may name directly.
const SOURCE_EXTS: &[&str] = &["ts", "tsx", "mts", "cts"];
/// Extensions tried, in order, after a specifier that has none.
const IMPLIED_EXTS: &[&str] = &["ts", "tsx"];
/// Keywords after which a `/` starts a regular expression, not a division.
const BEFORE_REGEX: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    Op(char),
    /// The spread or rest operator `...`.
    Ellipsis,
    /// A string literal, with its value.
    Str(String),
    /// A number, template literal or regular expression.
    Literal,
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    tok: Tok,
    /// A line break comes between this token and the one before it.
    newline: bool,
}

/// What an import binds a local name to.
#[derive(Clone, Debug, PartialEq)]
enum Imported {
    /// An export of the module; `default` for a default import.
    Name(String),
    /// The whole module: `* as name`, or `name = require("…")`.
    Namespace,
}

/// One `import` declaration.
#[derive(Clone, Debug, PartialEq)]
struct ImportDecl {
    specifier: String,
    /// Local names and what they are bound to; empty for `import "…"`.
    bindings: Vec<(String, Imported)>,
}

/// What an exported name of a file stands for.
#[derive(Clone, Debug, PartialEq)]
enum Export {
    /// Declared in the file.
    Local,
    /// A local name exported under this name: `export { local as name }`.
    Alias(String),
    /// `export { name as … } from "…"`.
    From { specifier: String, name: String },
    /// `export * as … from "…"`: the whole module.
    Namespace(String),
}

#[derive(Debug, Default)]
struct TsFile {
    imports: Vec<ImportDecl>,
    exports: BTreeMap<String, Export>,
    /// Specifiers of `export * from "…"`.
    stars: Vec<String>,
    /// Local names that an export clause or `export default` passes on.
    exported: BTreeSet<String>,
    /// `import("…")` and `require("…")` calls, with the member names that
    /// follow them, such as `Session` in `import("./types").Session`.
    dynamic: Vec<(String, Vec<String>)>,
    /// Dotted names used in the code, such as `storage.repo.save`.
    chains: BTreeSet<Vec<String>>,
}

impl TsFile {
    /// The import that binds a local name.
    fn binding(&self, local: &str) -> Option<(&str, &Imported)> {
        self.imports.iter().rev().find_map(|decl| {
            decl.bindings
                .iter()
                .find(|(name, _)| name == local)
                .map(|(_, imported)| (decl.specifier.as_str(), imported))
        })
    }
}

/// What a name exported by a module turns out to be.
enum Attr {
    /// A whole module, reached through a namespace export.
    Module(FileId),
    /// Defined in `file`, or last traced to it when `exact` is false.
    Definition {
        file: FileId,
        exact: bool,
    },
    /// Imported from outside the project.
    Foreign,
    NotFound,
}

/// Fills in the dependencies of every TypeScript file.
pub(crate) fn analyze(repo: &Repo, search_paths: &[String], deps: &mut [FileDeps]) {
    let parsed: BTreeMap<FileId, TsFile> = repo
        .files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.language == Language::TypeScript)
        .map(|(id, file)| (id, parse(&file.text)))
        .collect();
    let typescript = TypeScript {
        repo,
        parsed,
        search_paths,
        configs: Configs::new(repo),
        resolved: RefCell::new(BTreeMap::new()),
    };
    for &id in typescript.parsed.keys() {
        deps[id] = typescript.file_deps(id);
    }
}

struct TypeScript<'a> {
    repo: &'a Repo,
    parsed: BTreeMap<FileId, TsFile>,
    search_paths: &'a [String],
    configs: Configs<'a>,
    /// Resolved specifiers, by the directory of the importing file.
    resolved: RefCell<BTreeMap<(String, String), Option<FileId>>>,
}

impl TypeScript<'_> {
    fn lookup(&self, path: &str) -> Option<FileId> {
        self.repo
            .by_path
            .get(path)
            .copied()
            .filter(|id| self.parsed.contains_key(id))
    }

    /// Finds the project file a specifier names, or `None` for a package,
    /// a built-in module or a file in another language.
    fn resolve(&self, from: FileId, specifier: &str) -> Option<FileId> {
        let dir = dir_of(&self.repo.files[from].path);
        let key = (dir.to_string(), specifier.to_string());
        let cached = self.resolved.borrow().get(&key).copied();
        if let Some(cached) = cached {
            return cached;
        }
        let found = self.resolve_from(dir, specifier);
        self.resolved.borrow_mut().insert(key, found);
        found
    }

    /// A relative specifier is resolved against the importing directory.
    /// Any other is tried under the configured search paths, then through
    /// `paths` and `baseUrl` in the nearest `tsconfig.json`.
    fn resolve_from(&self, dir: &str, specifier: &str) -> Option<FileId> {
        if specifier.is_empty() || specifier.starts_with('/') || specifier.contains('\\') {
            return None;
        }
        let only_dir = specifier.ends_with('/') || matches!(file_name(specifier), "." | "..");
        let relative = specifier == "."
            || specifier == ".."
            || specifier.starts_with("./")
            || specifier.starts_with("../");
        if relative {
            return self.load(&normalize(dir, specifier)?, only_dir);
        }
        for base in self.search_paths {
            if let Some(found) = normalize(base, specifier).and_then(|p| self.load(&p, only_dir)) {
                return Some(found);
            }
        }
        let config = self.configs.for_dir(dir)?;
        config
            .candidates(specifier)
            .iter()
            .find_map(|path| self.load(path, only_dir))
    }

    /// The source file at a path from the root, trying extensions and index
    /// files the way the compiler does. A `.js` extension names the
    /// TypeScript file that compiles to it.
    fn load(&self, path: &str, only_dir: bool) -> Option<FileId> {
        if !only_dir && !path.is_empty() {
            let (stem, ext) = split_ext(file_name(path));
            let stem_path = join(dir_of(path), stem);
            let replacements: &[&str] = match ext {
                "js" => &["ts", "tsx"],
                "jsx" => &["tsx"],
                "mjs" => &["mts"],
                "cjs" => &["cts"],
                _ => &[],
            };
            let found = if SOURCE_EXTS.contains(&ext) {
                self.lookup(path)
            } else if !replacements.is_empty() {
                replacements
                    .iter()
                    .find_map(|ext| self.lookup(&format!("{stem_path}.{ext}")))
            } else {
                IMPLIED_EXTS
                    .iter()
                    .find_map(|ext| self.lookup(&format!("{path}.{ext}")))
            };
            if found.is_some() {
                return found;
            }
        }
        IMPLIED_EXTS
            .iter()
            .find_map(|ext| self.lookup(&join(path, &format!("index.{ext}"))))
    }

    /// Finds what the export `name` of `file` is, following re-exports.
    fn export_of(&self, file: FileId, name: &str, trail: &mut Vec<(FileId, String)>) -> Attr {
        let key = (file, name.to_string());
        if trail.len() >= MAX_HOPS || trail.contains(&key) {
            return Attr::NotFound;
        }
        trail.push(key);
        let found = self.export_in_file(file, name, trail);
        trail.pop();
        found
    }

    fn export_in_file(&self, file: FileId, name: &str, trail: &mut Vec<(FileId, String)>) -> Attr {
        let parsed = &self.parsed[&file];
        if let Some(export) = parsed.exports.get(name) {
            return match export {
                Export::Local => Attr::Definition { file, exact: true },
                Export::Alias(local) => match parsed.binding(local) {
                    Some((specifier, imported)) => self.follow(file, specifier, imported, trail),
                    None => Attr::Definition { file, exact: true },
                },
                Export::From {
                    specifier,
                    name: original,
                } => self.follow(file, specifier, &Imported::Name(original.clone()), trail),
                Export::Namespace(specifier) => self
                    .resolve(file, specifier)
                    .map_or(Attr::Foreign, Attr::Module),
            };
        }
        // `export *` passes on every name except the default export.
        if name != "default" {
            for specifier in &parsed.stars {
                if let Some(source) = self.resolve(file, specifier) {
                    let found = self.export_of(source, name, trail);
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

    /// What an import in `from` refers to.
    fn follow(
        &self,
        from: FileId,
        specifier: &str,
        imported: &Imported,
        trail: &mut Vec<(FileId, String)>,
    ) -> Attr {
        let Some(source) = self.resolve(from, specifier) else {
            return Attr::Foreign;
        };
        match imported {
            Imported::Namespace => Attr::Module(source),
            Imported::Name(original) => match self.export_of(source, original, trail) {
                Attr::NotFound => Attr::Definition {
                    file: source,
                    exact: false,
                },
                found => found,
            },
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

        for decl in &parsed.imports {
            let Some(source) = self.resolve(file, &decl.specifier) else {
                continue;
            };
            deps.imports.insert(source);
            // An import for its side effects depends on the whole module.
            if decl.bindings.is_empty() {
                deps.add_use(source, true);
            }
            for (local, imported) in &decl.bindings {
                let target = self.follow(file, &decl.specifier, imported, &mut Vec::new());
                // A name that the file only exports again is a re-export.
                if parsed.exported.contains(local) && !heads.contains(local.as_str()) {
                    deps.reexports.insert(source);
                    if let Attr::Definition { file: target, .. } | Attr::Module(target) = target {
                        deps.imports.insert(target);
                        deps.reexports.insert(target);
                    }
                    continue;
                }
                match target {
                    Attr::Definition {
                        file: target,
                        exact,
                    } => deps.add_use(target, exact),
                    Attr::Module(module) => {
                        let mut used = false;
                        for chain in parsed.chains.iter().filter(|chain| chain[0] == *local) {
                            self.walk(module, &chain[1..], &mut deps);
                            used = true;
                        }
                        if !used {
                            deps.add_use(module, false);
                        }
                    }
                    Attr::Foreign | Attr::NotFound => {}
                }
            }
        }

        for (specifier, members) in &parsed.dynamic {
            if let Some(source) = self.resolve(file, specifier) {
                deps.imports.insert(source);
                self.walk(source, members, &mut deps);
            }
        }

        for (name, export) in &parsed.exports {
            let (Export::From { specifier, .. } | Export::Namespace(specifier)) = export else {
                continue;
            };
            if let Some(source) = self.resolve(file, specifier) {
                deps.imports.insert(source);
                deps.reexports.insert(source);
            }
            if let Attr::Definition { file: target, .. } | Attr::Module(target) =
                self.export_of(file, name, &mut Vec::new())
            {
                deps.imports.insert(target);
                deps.reexports.insert(target);
            }
        }
        for specifier in &parsed.stars {
            if let Some(source) = self.resolve(file, specifier) {
                deps.imports.insert(source);
                deps.reexports.insert(source);
            }
        }
        deps
    }

    /// Follows member names through modules until they reach a definition,
    /// and records the file that holds it. A module used as a whole is
    /// recorded as a use whose definition was not found.
    fn walk(&self, module: FileId, members: &[String], deps: &mut FileDeps) {
        let mut current = module;
        for name in members {
            match self.export_of(current, name, &mut Vec::new()) {
                Attr::Module(inner) => current = inner,
                Attr::Definition { file, exact } => {
                    deps.add_use(file, exact);
                    return;
                }
                Attr::NotFound => break,
                Attr::Foreign => return,
            }
        }
        deps.add_use(current, false);
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn parse(text: &str) -> TsFile {
    let toks = lex(text);
    let mut out = TsFile::default();
    // Tokens that belong to import and export declarations, which are not
    // code that uses a name.
    let mut declaration = vec![false; toks.len()];
    let mut depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        let after_dot = i > 0 && toks[i - 1].tok == Tok::Op('.');
        match &toks[i].tok {
            Tok::Op('{') => depth += 1,
            Tok::Op('}') => depth = depth.saturating_sub(1),
            // Declarations inside braces belong to ambient modules and
            // namespaces, not to the file.
            Tok::Name(word) if depth == 0 && !after_dot => {
                let end = match word.as_str() {
                    "import" => parse_import(&toks, i + 1).map(|(decl, end)| {
                        out.imports.push(decl);
                        end
                    }),
                    "export" => Some(parse_export(&toks, i + 1, &mut out)),
                    _ => None,
                };
                if let Some(end) = end {
                    declaration[i..end].iter_mut().for_each(|flag| *flag = true);
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    collect_code(&toks, &declaration, &mut out);
    out
}

fn tok_at(toks: &[Token], i: usize) -> Option<&Tok> {
    toks.get(i).map(|token| &token.tok)
}

fn name_at(toks: &[Token], i: usize) -> Option<&str> {
    match tok_at(toks, i) {
        Some(Tok::Name(name)) => Some(name),
        _ => None,
    }
}

fn str_at(toks: &[Token], i: usize) -> Option<&str> {
    match tok_at(toks, i) {
        Some(Tok::Str(value)) => Some(value),
        _ => None,
    }
}

fn op_at(toks: &[Token], i: usize, op: char) -> bool {
    tok_at(toks, i) == Some(&Tok::Op(op))
}

/// A name in an import or export list, which may be written as a string.
fn list_name_at(toks: &[Token], i: usize) -> Option<&str> {
    name_at(toks, i).or_else(|| str_at(toks, i))
}

/// Recognises `require("…")` at `i`; returns the specifier.
fn require_at(toks: &[Token], i: usize) -> Option<&str> {
    if name_at(toks, i) == Some("require") && op_at(toks, i + 1, '(') && op_at(toks, i + 3, ')') {
        str_at(toks, i + 2)
    } else {
        None
    }
}

/// Parses what follows the `import` keyword. Returns `None` when it is not
/// an import declaration, such as `import(…)`, `import.meta` or
/// `import name = Space.Member`.
fn parse_import(toks: &[Token], start: usize) -> Option<(ImportDecl, usize)> {
    let mut i = start;
    if let Some(specifier) = str_at(toks, i) {
        let decl = ImportDecl {
            specifier: specifier.to_string(),
            bindings: Vec::new(),
        };
        return Some((decl, finish(toks, i + 1)));
    }
    // `type` is a modifier unless it is the name of a default import, as in
    // `import type from "…"`.
    let modifier = match name_at(toks, i + 1) {
        Some(next) => next != "from",
        None => op_at(toks, i + 1, '{') || op_at(toks, i + 1, '*'),
    };
    if name_at(toks, i) == Some("type") && modifier {
        i += 1;
    }

    let mut bindings = Vec::new();
    let mut more = true;
    if let Some(local) = name_at(toks, i) {
        if op_at(toks, i + 1, '=') {
            let specifier = require_at(toks, i + 2)?;
            let decl = ImportDecl {
                specifier: specifier.to_string(),
                bindings: vec![(local.to_string(), Imported::Namespace)],
            };
            return Some((decl, finish(toks, i + 6)));
        }
        bindings.push((local.to_string(), Imported::Name("default".to_string())));
        i += 1;
        more = op_at(toks, i, ',');
        if more {
            i += 1;
        }
    }
    if more {
        if op_at(toks, i, '*') {
            if name_at(toks, i + 1) != Some("as") {
                return None;
            }
            let local = name_at(toks, i + 2)?;
            bindings.push((local.to_string(), Imported::Namespace));
            i += 3;
        } else if op_at(toks, i, '{') {
            let (entries, end) = specifier_list(toks, i)?;
            for (original, local) in entries {
                bindings.push((local, Imported::Name(original)));
            }
            i = end;
        } else {
            return None;
        }
    }
    if name_at(toks, i) != Some("from") {
        return None;
    }
    let specifier = str_at(toks, i + 1)?.to_string();
    Some((
        ImportDecl {
            specifier,
            bindings,
        },
        finish(toks, i + 2),
    ))
}

/// Parses a braced list of `name`, `name as alias` and `type name` entries
/// that starts at `open`. Returns the `(name, alias)` pairs and the index
/// after the closing brace.
fn specifier_list(toks: &[Token], open: usize) -> Option<(Vec<(String, String)>, usize)> {
    let mut entries = Vec::new();
    let mut i = open + 1;
    loop {
        if op_at(toks, i, '}') {
            return Some((entries, i + 1));
        }
        if name_at(toks, i) == Some("type") {
            // `type` modifies the entry, except in `type`, `type as x` and
            // `type,`, where it is the name itself.
            let modifier = match name_at(toks, i + 1) {
                Some("as") => {
                    list_name_at(toks, i + 2).is_none() || name_at(toks, i + 2) == Some("as")
                }
                _ => list_name_at(toks, i + 1).is_some(),
            };
            if modifier {
                i += 1;
            }
        }
        let original = list_name_at(toks, i)?.to_string();
        i += 1;
        let alias = if name_at(toks, i) == Some("as") {
            let alias = list_name_at(toks, i + 1)?.to_string();
            i += 2;
            alias
        } else {
            original.clone()
        };
        entries.push((original, alias));
        if op_at(toks, i, ',') {
            i += 1;
        } else if !op_at(toks, i, '}') {
            return None;
        }
    }
}

/// Skips the import attributes and the semicolon that may end a
/// declaration whose module specifier ends just before `i`.
fn finish(toks: &[Token], mut i: usize) -> usize {
    let attributes = match name_at(toks, i) {
        Some("with") => true,
        Some("assert") => !toks[i].newline,
        _ => false,
    };
    if attributes && op_at(toks, i + 1, '{') {
        let mut depth = 0usize;
        for (j, token) in toks.iter().enumerate().skip(i + 1) {
            match token.tok {
                Tok::Op('{') => depth += 1,
                Tok::Op('}') => {
                    depth -= 1;
                    if depth == 0 {
                        i = j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    if op_at(toks, i, ';') {
        i += 1;
    }
    i
}

/// Parses what follows the `export` keyword and returns the index of the
/// first token after the declaration. Export lists and re-exports are
/// consumed whole; for an exported declaration only the keyword is, so that
/// the rest of it is read as code.
fn parse_export(toks: &[Token], start: usize, out: &mut TsFile) -> usize {
    let mut i = start;
    if name_at(toks, i) == Some("type") && (op_at(toks, i + 1, '{') || op_at(toks, i + 1, '*')) {
        i += 1;
    }

    if op_at(toks, i, '*') {
        if name_at(toks, i + 1) == Some("from") {
            if let Some(specifier) = str_at(toks, i + 2) {
                out.stars.push(specifier.to_string());
                return finish(toks, i + 3);
            }
        }
        if name_at(toks, i + 1) == Some("as") && name_at(toks, i + 3) == Some("from") {
            if let (Some(name), Some(specifier)) = (list_name_at(toks, i + 2), str_at(toks, i + 4))
            {
                out.exports
                    .insert(name.to_string(), Export::Namespace(specifier.to_string()));
                return finish(toks, i + 5);
            }
        }
        return start;
    }

    if op_at(toks, i, '{') {
        let Some((entries, end)) = specifier_list(toks, i) else {
            return start;
        };
        if name_at(toks, end) == Some("from") {
            let Some(specifier) = str_at(toks, end + 1) else {
                return start;
            };
            for (original, exported) in entries {
                let export = Export::From {
                    specifier: specifier.to_string(),
                    name: original,
                };
                out.exports.insert(exported, export);
            }
            return finish(toks, end + 2);
        }
        for (local, exported) in entries {
            out.exported.insert(local.clone());
            out.exports.insert(exported, Export::Alias(local));
        }
        return if op_at(toks, end, ';') { end + 1 } else { end };
    }

    match name_at(toks, i) {
        Some("default") => {
            // `export default name` on its own passes a local name on;
            // anything else is a declaration or expression of this file.
            let ends = |j: usize| {
                toks.get(j)
                    .is_none_or(|token| token.newline || matches!(token.tok, Tok::Op(';' | '}')))
            };
            if let Some(local) = name_at(toks, i + 1) {
                if ends(i + 2) {
                    out.exported.insert(local.to_string());
                    out.exports
                        .insert("default".to_string(), Export::Alias(local.to_string()));
                    return if op_at(toks, i + 2, ';') {
                        i + 3
                    } else {
                        i + 2
                    };
                }
            }
            out.exports.insert("default".to_string(), Export::Local);
            i + 1
        }
        Some("import") => {
            // `export import name = require("…")` imports and exports a
            // module at once; `export import name = Space.Member` declares.
            if let Some(name) = name_at(toks, i + 1) {
                if op_at(toks, i + 2, '=') {
                    if let Some(specifier) = require_at(toks, i + 3) {
                        out.imports.push(ImportDecl {
                            specifier: specifier.to_string(),
                            bindings: vec![(name.to_string(), Imported::Namespace)],
                        });
                        out.exported.insert(name.to_string());
                        out.exports
                            .insert(name.to_string(), Export::Alias(name.to_string()));
                        return finish(toks, i + 7);
                    }
                    out.exports.insert(name.to_string(), Export::Local);
                }
            }
            start
        }
        _ => {
            for name in declared_names(toks, i) {
                out.exports.insert(name, Export::Local);
            }
            start
        }
    }
}

/// Names declared by the declaration at `i`, after `export`.
fn declared_names(toks: &[Token], mut i: usize) -> Vec<String> {
    while matches!(name_at(toks, i), Some("declare" | "abstract" | "async")) {
        i += 1;
    }
    let name = match name_at(toks, i) {
        Some("function") => {
            let generator = op_at(toks, i + 1, '*');
            name_at(toks, i + if generator { 2 } else { 1 })
        }
        Some("class" | "interface" | "enum" | "type" | "namespace" | "module") => {
            name_at(toks, i + 1)
        }
        Some("const") if name_at(toks, i + 1) == Some("enum") => name_at(toks, i + 2),
        Some("const" | "let" | "var" | "using") => return binding_names(toks, i + 1),
        _ => None,
    };
    name.map(str::to_string).into_iter().collect()
}

/// Names bound by the first declarator of a variable declaration: a name,
/// or the names in an object or array pattern.
fn binding_names(toks: &[Token], i: usize) -> Vec<String> {
    match tok_at(toks, i) {
        Some(Tok::Name(name)) => vec![name.clone()],
        Some(Tok::Op('{' | '[')) => {
            let mut names = Vec::new();
            let mut depth = 0usize;
            for (j, token) in toks.iter().enumerate().skip(i) {
                match &token.tok {
                    Tok::Op('{' | '[') => depth += 1,
                    Tok::Op('}' | ']') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Name(name) => {
                        let after = matches!(
                            toks[j - 1].tok,
                            Tok::Op('{' | '[' | ',' | ':') | Tok::Ellipsis
                        );
                        let before =
                            matches!(tok_at(toks, j + 1), Some(Tok::Op(',' | '}' | ']' | '=')));
                        if after && before {
                            names.push(name.clone());
                        }
                    }
                    _ => {}
                }
            }
            names
        }
        _ => Vec::new(),
    }
}

/// Records the dotted names and the `import(…)` and `require(…)` calls in
/// the tokens that are not import or export declarations.
fn collect_code(toks: &[Token], declaration: &[bool], out: &mut TsFile) {
    let live = |j: usize| j < toks.len() && !declaration[j];
    // Member names that follow `i`, through `.`, `?.` and `!.`.
    let members = |mut j: usize| {
        let mut names = Vec::new();
        loop {
            let dot = if live(j) && toks[j].tok == Tok::Op('.') {
                j + 1
            } else if live(j + 1)
                && matches!(toks[j].tok, Tok::Op('?' | '!'))
                && toks[j + 1].tok == Tok::Op('.')
            {
                j + 2
            } else {
                return (names, j);
            };
            match tok_at(toks, dot) {
                Some(Tok::Name(name)) if live(dot) => {
                    names.push(name.clone());
                    j = dot + 1;
                }
                _ => return (names, j),
            }
        }
    };

    let mut i = 0;
    while i < toks.len() {
        let Tok::Name(name) = &toks[i].tok else {
            i += 1;
            continue;
        };
        let after_dot = i > 0 && toks[i - 1].tok == Tok::Op('.');
        if !live(i) || after_dot || name.starts_with('#') {
            i += 1;
            continue;
        }
        let call = (name == "import" || name == "require")
            && op_at(toks, i + 1, '(')
            && op_at(toks, i + 3, ')')
            && live(i + 3);
        if let (true, Some(specifier)) = (call, str_at(toks, i + 2)) {
            let (names, end) = members(i + 4);
            out.dynamic.push((specifier.to_string(), names));
            i = end;
            continue;
        }
        let (rest, end) = members(i + 1);
        let mut chain = vec![name.clone()];
        chain.extend(rest);
        out.chains.insert(chain);
        i = end;
    }
}

// ---------------------------------------------------------------------------
// Lexing
// ---------------------------------------------------------------------------

fn is_name_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == '$'
}

fn is_name_part(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// Splits source text into tokens, dropping comments. Template literals and
/// regular expressions become single tokens, but the expressions inside a
/// template literal's `${…}` are read as code.
fn lex(text: &str) -> Vec<Token> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut out: Vec<Token> = Vec::new();
    // One entry per open brace: `true` for the `${` of a template literal.
    let mut braces: Vec<bool> = Vec::new();
    let mut newline = false;
    let push = |out: &mut Vec<Token>, tok: Tok, newline: &mut bool| {
        out.push(Token {
            tok,
            newline: *newline,
        });
        *newline = false;
    };

    let mut i = 0;
    if text.starts_with("#!") {
        while i < n && c[i] != '\n' {
            i += 1;
        }
    }
    while i < n {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        match ch {
            '\n' => {
                newline = true;
                i += 1;
            }
            _ if ch.is_whitespace() => i += 1,
            '/' if next == Some('/') => {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                i += 2;
                while i < n && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                    newline |= c[i] == '\n';
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            '"' | '\'' => {
                let (value, end) = string(&c, i);
                push(&mut out, Tok::Str(value), &mut newline);
                i = end;
            }
            '`' => {
                push(&mut out, Tok::Literal, &mut newline);
                i = template(&c, i + 1, &mut braces);
            }
            '/' if regex_allowed(out.last()) => {
                push(&mut out, Tok::Literal, &mut newline);
                i = regex(&c, i);
            }
            '{' => {
                braces.push(false);
                push(&mut out, Tok::Op('{'), &mut newline);
                i += 1;
            }
            '}' => {
                if braces.pop() == Some(true) {
                    i = template(&c, i + 1, &mut braces);
                } else {
                    push(&mut out, Tok::Op('}'), &mut newline);
                    i += 1;
                }
            }
            '.' if next == Some('.') && c.get(i + 2) == Some(&'.') => {
                push(&mut out, Tok::Ellipsis, &mut newline);
                i += 3;
            }
            _ if ch.is_ascii_digit() || (ch == '.' && next.is_some_and(|d| d.is_ascii_digit())) => {
                i += 1;
                while i < n
                    && (c[i].is_alphanumeric()
                        || c[i] == '_'
                        || (c[i] == '.' && c.get(i + 1).is_some_and(char::is_ascii_digit)))
                {
                    i += 1;
                }
                push(&mut out, Tok::Literal, &mut newline);
            }
            // `#name` is a private member, which never names an import.
            _ if is_name_start(ch) || (ch == '#' && next.is_some_and(is_name_start)) => {
                let start = i;
                i += 1;
                while i < n && is_name_part(c[i]) {
                    i += 1;
                }
                push(
                    &mut out,
                    Tok::Name(c[start..i].iter().collect()),
                    &mut newline,
                );
            }
            _ => {
                push(&mut out, Tok::Op(ch), &mut newline);
                i += 1;
            }
        }
    }
    out
}

/// Reads the string literal that starts at `i`. Returns its value and the
/// index just past it. An unterminated string ends at the end of the line.
fn string(c: &[char], mut i: usize) -> (String, usize) {
    let quote = c[i];
    let mut value = String::new();
    i += 1;
    while i < c.len() {
        match c[i] {
            '\\' => {
                match c.get(i + 1) {
                    Some('\r') if c.get(i + 2) == Some(&'\n') => i += 1,
                    Some('\n' | '\r') => {}
                    Some(&escaped) => value.push(escaped),
                    None => {}
                }
                i += 2;
            }
            '\n' => return (value, i),
            ch if ch == quote => return (value, i + 1),
            ch => {
                value.push(ch);
                i += 1;
            }
        }
    }
    (value, c.len())
}

/// Skips template literal text from `i` up to the closing backtick or the
/// next `${`. Returns the index just past either; at `${`, a brace is
/// opened that ends the substitution when it closes.
fn template(c: &[char], mut i: usize, braces: &mut Vec<bool>) -> usize {
    while i < c.len() {
        match c[i] {
            '\\' => i += 2,
            '`' => return i + 1,
            '$' if c.get(i + 1) == Some(&'{') => {
                braces.push(true);
                return i + 2;
            }
            _ => i += 1,
        }
    }
    c.len()
}

/// Skips the regular expression that starts at `i`, with its flags.
fn regex(c: &[char], mut i: usize) -> usize {
    let mut class = false;
    i += 1;
    while i < c.len() {
        match c[i] {
            '\\' => i += 2,
            '\n' => return i,
            '[' => {
                class = true;
                i += 1;
            }
            ']' => {
                class = false;
                i += 1;
            }
            '/' if !class => {
                i += 1;
                while i < c.len() && is_name_part(c[i]) {
                    i += 1;
                }
                return i;
            }
            _ => i += 1,
        }
    }
    c.len()
}

/// Whether a `/` after `prev` starts a regular expression. After `<` it is
/// the closing tag of a JSX element.
fn regex_allowed(prev: Option<&Token>) -> bool {
    match prev.map(|token| &token.tok) {
        None => true,
        Some(Tok::Op(')' | ']' | '}' | '<')) => false,
        Some(Tok::Op(_) | Tok::Ellipsis) => true,
        Some(Tok::Name(word)) => BEFORE_REGEX.contains(&word.as_str()),
        Some(Tok::Str(_) | Tok::Literal) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn named(local: &str, original: &str) -> (String, Imported) {
        (local.to_string(), Imported::Name(original.to_string()))
    }

    fn import(specifier: &str, bindings: Vec<(String, Imported)>) -> ImportDecl {
        ImportDecl {
            specifier: specifier.to_string(),
            bindings,
        }
    }

    #[test]
    fn parses_import_forms() {
        let file = parse(concat!(
            "import Default from \"./a\";\n",
            "import * as ns from './b'\n",
            "import { x, y as z, default as d, type T, \"str\" as s } from \"./c\";\n",
            "import Def, { w } from \"./d\" with { type: \"json\" };\n",
            "import Def2, * as ns2 from \"./e\"\n",
            "import \"./side-effect\";\n",
            "import type { U } from \"./f\";\n",
            "import type V from \"./g\";\n",
            "import type from \"./h\";\n",
            "import fs = require(\"fs\");\n",
            "import Alias = Space.Member;\n",
            "import {} from \"./empty\";\n",
        ));
        assert_eq!(
            file.imports,
            vec![
                import("./a", vec![named("Default", "default")]),
                import("./b", vec![("ns".into(), Imported::Namespace)]),
                import(
                    "./c",
                    vec![
                        named("x", "x"),
                        named("z", "y"),
                        named("d", "default"),
                        named("T", "T"),
                        named("s", "str"),
                    ]
                ),
                import("./d", vec![named("Def", "default"), named("w", "w")]),
                import(
                    "./e",
                    vec![
                        named("Def2", "default"),
                        ("ns2".into(), Imported::Namespace)
                    ]
                ),
                import("./side-effect", vec![]),
                import("./f", vec![named("U", "U")]),
                import("./g", vec![named("V", "default")]),
                import("./h", vec![named("type", "default")]),
                import("fs", vec![("fs".into(), Imported::Namespace)]),
                import("./empty", vec![]),
            ]
        );
        // The names in declarations are not uses; `Space.Member` is.
        assert!(!file.chains.iter().any(|chain| chain[0] == "Default"));
        assert!(file.chains.contains(&names(&["Space", "Member"])));
    }

    #[test]
    fn parses_type_modifiers_in_lists() {
        let file = parse(
            "import { type, type as t2, type as, type as as a2, type A as B } from \"./m\";\n",
        );
        assert_eq!(
            file.imports[0].bindings,
            vec![
                named("type", "type"),
                named("t2", "type"),
                named("as", "as"),
                named("a2", "as"),
                named("B", "A"),
            ]
        );
    }

    #[test]
    fn parses_export_forms() {
        let file = parse(concat!(
            "export * from \"./all\";\n",
            "export * as space from \"./space\";\n",
            "export type * as types from \"./types\";\n",
            "export { a, b as c } from \"./ab\";\n",
            "export type { T } from \"./t\";\n",
            "export { default as Button, default } from \"./Button\";\n",
            "export { local, other as renamed };\n",
            "export function f() {}\n",
            "export function* gen() {}\n",
            "export async function run() {}\n",
            "export declare abstract class K {}\n",
            "export interface I {}\n",
            "export type Alias<X> = X;\n",
            "export const enum E { A }\n",
            "export enum F { B }\n",
            "export namespace N { export const inner = 1; }\n",
            "export const one = 1, two = 2;\n",
            "export let { p, q: r, ...rest } = obj;\n",
            "export const [first, , second = 2] = list;\n",
            "export default thing;\n",
        ));
        let export = |name: &str| file.exports.get(name).cloned();
        let from = |specifier: &str, name: &str| {
            Some(Export::From {
                specifier: specifier.into(),
                name: name.into(),
            })
        };
        assert_eq!(file.stars, vec!["./all".to_string()]);
        assert_eq!(export("space"), Some(Export::Namespace("./space".into())));
        assert_eq!(export("types"), Some(Export::Namespace("./types".into())));
        assert_eq!(export("a"), from("./ab", "a"));
        assert_eq!(export("c"), from("./ab", "b"));
        assert_eq!(export("T"), from("./t", "T"));
        assert_eq!(export("Button"), from("./Button", "default"));
        assert_eq!(export("local"), Some(Export::Alias("local".into())));
        assert_eq!(export("renamed"), Some(Export::Alias("other".into())));
        for name in [
            "f", "gen", "run", "K", "I", "Alias", "E", "F", "N", "one", "p", "r", "rest", "first",
            "second",
        ] {
            assert_eq!(export(name), Some(Export::Local), "{name}");
        }
        // Only the first declarator, and nothing inside a namespace.
        assert_eq!(export("two"), None);
        assert_eq!(export("inner"), None);
        assert_eq!(export("q"), None);
        // `export { default } from` is replaced by the final default export.
        assert_eq!(export("default"), Some(Export::Alias("thing".into())));
        let exported: Vec<&str> = file.exported.iter().map(String::as_str).collect();
        assert_eq!(exported, vec!["local", "other", "thing"]);
    }

    #[test]
    fn distinguishes_default_exports() {
        for (text, expected) in [
            ("export default Foo\n", Export::Alias("Foo".into())),
            ("export default Foo", Export::Alias("Foo".into())),
            ("export default class Foo {}\n", Export::Local),
            ("export default function () {}\n", Export::Local),
            ("export default connect(state)(Foo);\n", Export::Local),
            ("export default { a: 1 };\n", Export::Local),
        ] {
            assert_eq!(
                parse(text).exports.get("default"),
                Some(&expected),
                "{text}"
            );
        }
    }

    #[test]
    fn ignores_declarations_inside_ambient_modules() {
        let file = parse(
            "declare module \"pkg\" {\n  import { A } from \"./a\";\n  export function f(): void;\n}\n",
        );
        assert!(file.imports.is_empty());
        assert!(file.exports.is_empty());
    }

    #[test]
    fn collects_chains_and_dynamic_imports() {
        let file = parse(concat!(
            "const x = storage.repo.save(session?.id, cache!.get(), ...items);\n",
            "const page = await import(\"./page\");\n",
            "type T = typeof import(\"./types\").Session.Kind;\n",
            "const cfg = require('./config').settings;\n",
            "obj.import(\"./not-a-module\");\n",
            "const el = <ui.Button onClick={() => go()}>Don't stop</ui.Button>;\n",
            "@Component({}) class C { #secret = 1; }\n",
        ));
        assert!(file.chains.contains(&names(&["storage", "repo", "save"])));
        assert!(file.chains.contains(&names(&["session", "id"])));
        assert!(file.chains.contains(&names(&["cache", "get"])));
        assert!(file.chains.contains(&names(&["items"])));
        assert!(file.chains.contains(&names(&["ui", "Button"])));
        assert!(file.chains.contains(&names(&["Component"])));
        assert!(!file.chains.iter().any(|chain| chain[0] == "repo"));
        assert!(!file.chains.iter().any(|chain| chain[0] == "#secret"));
        assert_eq!(
            file.dynamic,
            vec![
                ("./page".to_string(), vec![]),
                ("./types".to_string(), names(&["Session", "Kind"])),
                ("./config".to_string(), names(&["settings"])),
            ]
        );
    }

    #[test]
    fn ignores_imports_in_strings_comments_templates_and_regexes() {
        let file = parse(concat!(
            "#!/usr/bin/env node\n",
            "// import { a } from \"./no\";\n",
            "/* import { b } from \"./no\";\n */\n",
            "const s = \"import { c } from './no'\";\n",
            "const t = `\n import { d } from \"./no\";\n ${ `${\"}\"}` } `;\n",
            "const r = /import \"\\/no\"[/]/g;\n",
            "const half = total / 2 / count;\n",
            "import { e } from \"./yes\";\n",
        ));
        assert_eq!(file.imports, vec![import("./yes", vec![named("e", "e")])]);
        assert!(file.chains.contains(&names(&["total"])));
        assert!(file.chains.contains(&names(&["count"])));
    }

    #[test]
    fn reads_template_substitutions_as_code() {
        let file = parse("const url = `${api.base}/users/${ids.map((id) => `${id}`)}`;\nimport { z } from \"./z\";\n");
        assert!(file.chains.contains(&names(&["api", "base"])));
        assert!(file.chains.contains(&names(&["ids", "map"])));
        assert_eq!(file.imports.len(), 1);
    }

    #[test]
    fn decodes_string_escapes() {
        let c: Vec<char> = "'a\\'b\\\nc' rest".chars().collect();
        assert_eq!(string(&c, 0), ("a'bc".to_string(), 9));
        let c: Vec<char> = "\"open\nnext".chars().collect();
        assert_eq!(string(&c, 0), ("open".to_string(), 5));
    }
}
