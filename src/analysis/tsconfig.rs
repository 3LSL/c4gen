// Relationships:
// - `src/paths.rs`: Splits and joins repository paths
// - `src/repo.rs`: Reads TypeScript compiler settings files

//! Module aliases of TypeScript projects: `baseUrl` and `paths` in the
//! nearest `tsconfig.json`, including settings inherited through `extends`.
//!
//! Settings files are JSON with comments and trailing commas, as the
//! TypeScript compiler accepts them. A file that cannot be read gives no
//! aliases, so imports that rely on them are left unresolved.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::paths::{dir_of, join, normalize, parent};
use crate::repo::{OtherFile, Repo};

const CONFIG_NAME: &str = "tsconfig.json";
/// Limit on the length of an `extends` chain, as a guard against cycles.
const MAX_EXTENDS: usize = 16;
/// Limit on the nesting of JSON values.
const MAX_NESTING: usize = 64;

/// The alias settings that apply to the files of one directory.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Config {
    /// Directory that non-relative imports are also looked up in.
    base_url: Option<String>,
    paths: PathPatterns,
    /// Directory that the substitutions in `paths` are relative to.
    paths_base: String,
}

impl Config {
    /// Paths from the repository root that a non-relative import may name,
    /// in the order the compiler tries them: the substitutions of the best
    /// matching `paths` pattern, then the import under `baseUrl`.
    pub(crate) fn candidates(&self, specifier: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some((substitutions, star)) = self.matching(specifier) {
            for substitution in substitutions {
                if is_absolute(substitution) {
                    continue;
                }
                let path = match star {
                    Some(star) => substitution.replacen('*', star, 1),
                    None => substitution.clone(),
                };
                out.extend(normalize(&self.paths_base, &path));
            }
        }
        if let Some(base) = &self.base_url {
            out.extend(normalize(base, specifier));
        }
        out
    }

    /// The `paths` entry for a specifier and the text its `*` stands for.
    /// A pattern without a wildcard must match exactly and wins; otherwise
    /// the wildcard pattern with the longest prefix wins, the first written
    /// on a tie.
    fn matching<'s>(&self, specifier: &'s str) -> Option<(&[String], Option<&'s str>)> {
        let mut best: Option<(usize, &[String], &'s str)> = None;
        for (pattern, substitutions) in &self.paths {
            match pattern.split_once('*') {
                None => {
                    if pattern == specifier {
                        return Some((substitutions, None));
                    }
                }
                Some((prefix, suffix)) => {
                    let fits = !suffix.contains('*')
                        && specifier.len() >= prefix.len() + suffix.len()
                        && specifier.starts_with(prefix)
                        && specifier.ends_with(suffix);
                    if fits && best.is_none_or(|(length, _, _)| prefix.len() > length) {
                        let star = &specifier[prefix.len()..specifier.len() - suffix.len()];
                        best = Some((prefix.len(), substitutions, star));
                    }
                }
            }
        }
        best.map(|(_, substitutions, star)| (substitutions, Some(star)))
    }
}

/// `paths` patterns and their substitutions, in the order written.
type PathPatterns = Vec<(String, Vec<String>)>;

/// What one settings file contributes, with inherited settings applied.
#[derive(Clone, Debug, Default)]
struct Settings {
    base_url: Option<String>,
    /// The `paths` entries and the directory of the file that wrote them.
    paths: Option<(String, PathPatterns)>,
}

impl Settings {
    fn inherit(&mut self, other: Settings) {
        if other.base_url.is_some() {
            self.base_url = other.base_url;
        }
        if other.paths.is_some() {
            self.paths = other.paths;
        }
    }

    fn effective(self) -> Config {
        let (defined_in, paths) = self.paths.unwrap_or_default();
        // Substitutions are relative to `baseUrl` when there is one, and
        // otherwise to the settings file that wrote them.
        let paths_base = self.base_url.clone().unwrap_or(defined_in);
        Config {
            base_url: self.base_url,
            paths,
            paths_base,
        }
    }
}

/// The settings of every directory, read on first use.
pub(crate) struct Configs<'a> {
    repo: &'a Repo,
    by_dir: RefCell<BTreeMap<String, Option<Config>>>,
}

impl<'a> Configs<'a> {
    pub(crate) fn new(repo: &'a Repo) -> Self {
        Configs {
            repo,
            by_dir: RefCell::new(BTreeMap::new()),
        }
    }

    /// The settings from the `tsconfig.json` nearest to `dir`, in it or in
    /// a directory above it.
    pub(crate) fn for_dir(&self, dir: &str) -> Option<Config> {
        let cached = self.by_dir.borrow().get(dir).cloned();
        if let Some(cached) = cached {
            return cached;
        }
        let path = join(dir, CONFIG_NAME);
        let found = if self.repo.other_file(&path) == OtherFile::Present {
            self.settings(&path, &mut Vec::new())
                .map(Settings::effective)
        } else {
            parent(dir).and_then(|up| self.for_dir(up))
        };
        self.by_dir
            .borrow_mut()
            .insert(dir.to_string(), found.clone());
        found
    }

    /// Reads one settings file and the files it extends. `chain` holds the
    /// files being read, so that a cycle ends the chain.
    fn settings(&self, path: &str, chain: &mut Vec<String>) -> Option<Settings> {
        if chain.len() >= MAX_EXTENDS || chain.iter().any(|seen| seen == path) {
            return None;
        }
        let json = parse(&self.repo.read_other(path)?)?;
        let dir = dir_of(path);
        let mut settings = Settings::default();

        // Later entries of an `extends` list override earlier ones, and the
        // file's own options override them all.
        let parents: Vec<&str> = match json.get("extends") {
            Some(Json::Str(single)) => vec![single.as_str()],
            Some(Json::Array(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Json::Str(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        chain.push(path.to_string());
        for extended in parents {
            if let Some(extended) = self.extended_path(dir, extended) {
                if let Some(inherited) = self.settings(&extended, chain) {
                    settings.inherit(inherited);
                }
            }
        }
        chain.pop();

        if let Some(options) = json.get("compilerOptions") {
            if let Some(Json::Str(base)) = options.get("baseUrl") {
                if !is_absolute(base) {
                    settings.base_url = normalize(dir, base);
                }
            }
            if let Some(Json::Object(entries)) = options.get("paths") {
                let paths = entries
                    .iter()
                    .filter_map(|(pattern, value)| match value {
                        Json::Array(items) => Some((
                            pattern.clone(),
                            items
                                .iter()
                                .filter_map(|item| match item {
                                    Json::Str(text) => Some(text.clone()),
                                    _ => None,
                                })
                                .collect(),
                        )),
                        _ => None,
                    })
                    .collect();
                settings.paths = Some((dir.to_string(), paths));
            }
        }
        Some(settings)
    }

    /// The file an `extends` entry names, when it is a relative path. A
    /// path without the `.json` extension also names the file with it, as
    /// the compiler accepts. Shared settings from installed packages are
    /// outside the repository and are not followed.
    fn extended_path(&self, dir: &str, extended: &str) -> Option<String> {
        let relative = extended.starts_with("./") || extended.starts_with("../");
        if !relative {
            return None;
        }
        let path = normalize(dir, extended)?;
        if path.ends_with(".json") || self.repo.other_file(&path) == OtherFile::Present {
            Some(path)
        } else {
            Some(format!("{path}.json"))
        }
    }
}

/// Whether a path in a settings file is absolute, and so cannot be
/// expressed from the repository root.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.get(1..2) == Some(":")
}

/// A JSON value. Numbers, booleans and null are not needed and keep no value.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Scalar,
    Str(String),
    Array(Vec<Json>),
    /// Entries in the order written.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// The value of a key of an object. A repeated key takes its last value,
    /// as the compiler does.
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries
                .iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
}

/// Parses JSON that may contain comments and trailing commas.
fn parse(text: &str) -> Option<Json> {
    let mut parser = Parser {
        c: text.chars().collect(),
        i: 0,
    };
    let value = parser.value(0)?;
    parser.skip();
    (parser.i == parser.c.len()).then_some(value)
}

struct Parser {
    c: Vec<char>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    /// Skips whitespace and comments.
    fn skip(&mut self) {
        while let Some(ch) = self.peek() {
            let next = self.c.get(self.i + 1).copied();
            if ch.is_whitespace() {
                self.i += 1;
            } else if ch == '/' && next == Some('/') {
                while self.peek().is_some_and(|ch| ch != '\n') {
                    self.i += 1;
                }
            } else if ch == '/' && next == Some('*') {
                self.i += 2;
                while self.i < self.c.len()
                    && !(self.c[self.i] == '*' && self.c.get(self.i + 1) == Some(&'/'))
                {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(self.c.len());
            } else {
                return;
            }
        }
    }

    fn value(&mut self, nesting: usize) -> Option<Json> {
        if nesting > MAX_NESTING {
            return None;
        }
        self.skip();
        match self.peek()? {
            '{' => {
                self.i += 1;
                let mut entries = Vec::new();
                loop {
                    self.skip();
                    if self.peek()? == '}' {
                        self.i += 1;
                        return Some(Json::Object(entries));
                    }
                    let key = self.string()?;
                    self.skip();
                    if self.peek()? != ':' {
                        return None;
                    }
                    self.i += 1;
                    entries.push((key, self.value(nesting + 1)?));
                    if !self.separator('}')? {
                        return Some(Json::Object(entries));
                    }
                }
            }
            '[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip();
                    if self.peek()? == ']' {
                        self.i += 1;
                        return Some(Json::Array(items));
                    }
                    items.push(self.value(nesting + 1)?);
                    if !self.separator(']')? {
                        return Some(Json::Array(items));
                    }
                }
            }
            '"' => self.string().map(Json::Str),
            _ => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '+' | '.'))
                {
                    self.i += 1;
                }
                let word: String = self.c[start..self.i].iter().collect();
                let number = word.starts_with(|ch: char| ch == '-' || ch.is_ascii_digit());
                (number || matches!(word.as_str(), "true" | "false" | "null"))
                    .then_some(Json::Scalar)
            }
        }
    }

    /// Reads the comma after an entry. Returns `Some(true)` when another
    /// entry may follow, and `Some(false)` when `close` ended the container.
    fn separator(&mut self, close: char) -> Option<bool> {
        self.skip();
        let ch = self.peek()?;
        self.i += 1;
        match ch {
            ',' => Some(true),
            _ if ch == close => Some(false),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.peek()? != '"' {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let ch = self.peek()?;
            self.i += 1;
            match ch {
                '"' => return Some(out),
                '\\' => {
                    let escaped = self.peek()?;
                    self.i += 1;
                    out.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'u' => {
                            let hex: String = self.c.get(self.i..self.i + 4)?.iter().collect();
                            self.i += 4;
                            u32::from_str_radix(&hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .unwrap_or(char::REPLACEMENT_CHARACTER)
                        }
                        other => other,
                    });
                }
                other => out.push(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base_url: Option<&str>, paths: &[(&str, &[&str])], paths_base: &str) -> Config {
        Config {
            base_url: base_url.map(str::to_string),
            paths: paths
                .iter()
                .map(|(pattern, subs)| {
                    (
                        pattern.to_string(),
                        subs.iter().map(|s| s.to_string()).collect(),
                    )
                })
                .collect(),
            paths_base: paths_base.to_string(),
        }
    }

    #[test]
    fn parses_json_with_comments_and_trailing_commas() {
        let json = parse(
            "// Settings.\n{\n  /* inherited */ \"extends\": \"./base.json\",\n  \"compilerOptions\": {\n    \"strict\": true, \"target\": \"es2022\", \"lib\": [\"dom\",],\n    \"paths\": { \"@/*\": [\"./src/*\"], },\n    \"maxNodeModuleJsDepth\": -1.5e2,\n    \"x\": null, \"esc\": \"a\\\"b\\u0041\",\n  },\n}\n",
        )
        .unwrap();
        assert_eq!(json.get("extends"), Some(&Json::Str("./base.json".into())));
        let options = json.get("compilerOptions").unwrap();
        assert_eq!(options.get("esc"), Some(&Json::Str("a\"bA".into())));
        assert_eq!(
            options.get("paths").and_then(|p| p.get("@/*")),
            Some(&Json::Array(vec![Json::Str("./src/*".into())]))
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse("{\"a\": }").is_none());
        assert!(parse("{\"a\": 1").is_none());
        assert!(parse("{\"a\" 1}").is_none());
        assert!(parse("{} x").is_none());
        assert!(parse("[1 2]").is_none());
        assert!(parse("").is_none());
        assert!(parse(&"[".repeat(1000)).is_none());
    }

    #[test]
    fn repeated_keys_take_the_last_value() {
        let json = parse("{\"a\": \"1\", \"a\": \"2\"}").unwrap();
        assert_eq!(json.get("a"), Some(&Json::Str("2".into())));
    }

    #[test]
    fn matches_path_patterns_as_the_compiler_does() {
        let config = config(
            None,
            &[
                ("*", &["types/*"]),
                ("@app/*", &["./src/app/*", "./gen/*"]),
                ("@app/core/*", &["./core/*"]),
                ("lib", &["./lib/main.ts"]),
                ("@app/styles/*.css", &["./styles/*.css"]),
                ("@app/*.svg", &["./never/*"]),
            ],
            "web",
        );
        assert_eq!(
            config.candidates("@app/session/service"),
            vec!["web/src/app/session/service", "web/gen/session/service"]
        );
        assert_eq!(config.candidates("@app/core/log"), vec!["web/core/log"]);
        assert_eq!(config.candidates("lib"), vec!["web/lib/main.ts"]);
        assert_eq!(config.candidates("react"), vec!["web/types/react"]);
        assert_eq!(
            config.candidates("@app/styles/x.css"),
            vec!["web/styles/x.css"]
        );
        // On equal prefixes the pattern written first wins.
        assert_eq!(
            config.candidates("@app/icon.svg"),
            vec!["web/src/app/icon.svg", "web/gen/icon.svg"]
        );
    }

    #[test]
    fn falls_back_to_the_base_url() {
        let based = config(Some("src"), &[("@/*", &["./*"])], "src");
        assert_eq!(based.candidates("@/a"), vec!["src/a", "src/@/a"]);
        assert_eq!(based.candidates("lib/a"), vec!["src/lib/a"]);
        let outside = config(None, &[("@/*", &["/abs/*", "../../../out/*"])], "a");
        assert!(outside.candidates("@/x").is_empty());
    }

    #[test]
    fn substitutions_are_relative_to_the_base_url_or_their_file() {
        let parent = Settings {
            base_url: None,
            paths: Some(("pkg".into(), vec![("@/*".into(), vec!["src/*".into()])])),
        };
        assert_eq!(parent.clone().effective().paths_base, "pkg");
        // A child that inherits the patterns and sets its own `baseUrl`.
        let mut child = Settings::default();
        child.inherit(parent);
        child.base_url = Some("pkg/lib".into());
        assert_eq!(child.effective().paths_base, "pkg/lib");
    }
}
