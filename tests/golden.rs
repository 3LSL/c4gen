//! End-to-end tests against the fixture projects in `tests/fixtures`.
//!
//! Each golden case generates a diagram and compares the Mermaid text and the
//! diagnostics with the files in `tests/golden`. After an intended change,
//! run the tests with `C4GEN_BLESS=1` to rewrite those files, then review the
//! differences before committing them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use c4gen::{generate, Depth, DiagnosticKind, ElementKind, Error, Options, Outside};

fn fixture(project: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(project)
}

fn golden(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(file)
}

fn options(project: &str) -> Options {
    Options::new().root(fixture(project))
}

fn compare(file: &str, actual: &str) {
    let path = golden(file);
    if std::env::var_os("C4GEN_BLESS").is_some() {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no golden file {}; run with C4GEN_BLESS=1", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "{file} differs from its golden file");
}

fn check(case: &str, project: &str, module: &str, options: Options) {
    let diagram = generate(fixture(project).join(module), &options).unwrap();
    compare(&format!("{case}.mmd"), &diagram.to_mermaid());
    let diagnostics: String = diagram
        .diagnostics
        .iter()
        .map(|d| format!("{d}\n"))
        .collect();
    compare(&format!("{case}.diagnostics.txt"), &diagnostics);
}

/// Writes a small project under Cargo's temporary directory and returns its
/// root.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    root
}

#[test]
fn python_module() {
    check(
        "python_session",
        "python_app",
        "app/session",
        options("python_app"),
    );
}

#[test]
fn python_module_with_a_collapsed_subdirectory() {
    check(
        "python_storage",
        "python_app",
        "app/storage",
        options("python_app"),
    );
}

#[test]
fn python_package_in_full_with_outside_files() {
    let options = options("python_app")
        .depth(Depth::Unlimited)
        .outside(Outside::Files);
    check("python_app_full", "python_app", "app", options);
}

#[test]
fn python_repository_from_blocks_alone() {
    let options = options("python_app")
        .depth(Depth::Levels(2))
        .code_analysis(false);
    check("python_root_blocks_only", "python_app", "", options);
}

#[test]
fn rust_crate() {
    check("rust_src", "rust_app", "src", options("rust_app"));
}

#[test]
fn rust_module_with_its_root_file_beside_the_directory() {
    check(
        "rust_session",
        "rust_app",
        "src/session",
        options("rust_app"),
    );
}

#[test]
fn rust_single_file() {
    check(
        "rust_service_file",
        "rust_app",
        "src/session/service.rs",
        options("rust_app"),
    );
}

#[test]
fn cpp_module_with_header_and_source_pairs() {
    check("cpp_src", "cpp_app", "src", options("cpp_app"));
}

#[test]
fn cpp_repository_in_full() {
    check(
        "cpp_root_full",
        "cpp_app",
        "",
        options("cpp_app").depth(Depth::Unlimited),
    );
}

#[test]
fn python_module_without_outside_files() {
    check(
        "python_session_hide",
        "python_app",
        "app/session",
        options("python_app").outside(Outside::Hide),
    );
}

#[test]
fn output_is_identical_between_runs() {
    for (project, module) in [("python_app", "app"), ("rust_app", "src"), ("cpp_app", "")] {
        let options = options(project).depth(Depth::Unlimited);
        let first = generate(fixture(project).join(module), &options).unwrap();
        let second = generate(fixture(project).join(module), &options).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.to_mermaid(), second.to_mermaid());
    }
}

#[test]
fn output_does_not_depend_on_how_the_paths_are_spelled() {
    let root = fixture("python_app");
    let direct = generate(root.join("app/session"), &Options::new().root(&root)).unwrap();
    let roundabout = generate(
        root.join("app/storage/../session/."),
        &Options::new().root(root.join("docs/..")),
    )
    .unwrap();
    assert_eq!(direct, roundabout);
}

#[test]
fn test_code_is_left_out() {
    let diagram = generate(
        fixture("rust_app").join("src"),
        &options("rust_app").depth(Depth::Unlimited),
    )
    .unwrap();
    let paths: Vec<&str> = diagram.elements.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"src/storage/repo.rs"));
    assert!(
        !paths.contains(&"src/storage/checks.rs"),
        "a `#[cfg(test)] mod` file is test code"
    );

    let diagram = generate(
        fixture("python_app").join("app/session"),
        &options("python_app"),
    )
    .unwrap();
    assert!(diagram
        .elements
        .iter()
        .all(|e| !e.path.contains("test_service")));
}

#[test]
fn finds_the_repository_root_from_a_marker_directory() {
    let root = project(
        "root_detection",
        &[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("pkg/a.py", "# Relationships:\n# - `pkg/b.py`: Formats report rows\n\nfrom pkg import b\n\nb.render()\n"),
            ("pkg/b.py", "# Relationships:\n# None.\n\ndef render():\n    pass\n"),
        ],
    );
    let diagram = generate(root.join("pkg"), &Options::default()).unwrap();
    assert_eq!(diagram.module, "pkg");
    assert_eq!(diagram.relations.len(), 1);
    assert_eq!(diagram.relations[0].label, "Formats report rows");
    assert!(diagram.diagnostics.is_empty(), "{:?}", diagram.diagnostics);
}

#[test]
fn reports_problems_without_failing() {
    let root = project(
        "problems",
        &[
            (
                "a.py",
                "# Relationships:\n# - `missing.py`: Stores session records\n# - `b.py`: Uses the helper\n# - `test_c.py`: Checks the helper output\n# - `notes.txt`: Reads the release notes\n\nimport b\n\nb.run()\n",
            ),
            ("b.py", "def run():\n    pass\n"),
            ("test_c.py", "import b\n"),
            ("notes.txt", "Notes.\n"),
            ("pair.h", "// Relationships:\n// None.\n\n#pragma once\n"),
            (
                "pair.cpp",
                "// Relationships:\n// - `other.h`: Reads shared constants\n\n#include \"pair.h\"\n#include \"other.h\"\n",
            ),
            ("other.h", "// Relationships:\n// None.\n\n#pragma once\nconst int LIMIT = 3;\n"),
        ],
    );
    let diagram = generate(&root, &Options::new().root(&root)).unwrap();
    let reported: BTreeSet<(&str, DiagnosticKind)> = diagram
        .diagnostics
        .iter()
        .map(|d| (d.file.as_str(), d.kind))
        .collect();
    let expected: BTreeSet<(&str, DiagnosticKind)> = [
        ("a.py", DiagnosticKind::TargetNotFound),
        ("a.py", DiagnosticKind::Label),
        ("a.py", DiagnosticKind::TargetNotComponent),
        ("a.py", DiagnosticKind::Unsorted),
        ("b.py", DiagnosticKind::MissingBlock),
        ("pair.cpp", DiagnosticKind::MisplacedBlock),
    ]
    .into_iter()
    .collect();
    assert_eq!(reported, expected);
    assert!(diagram.has_errors());

    // Lines that can be used are still drawn: to `b.py`, to the non-source
    // file `notes.txt`, and from the pair to `other.h`.
    let labels: BTreeSet<&str> = diagram.relations.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Reads shared constants",
            "Reads the release notes",
            "Uses the helper"
        ]
        .into_iter()
        .collect()
    );
    let notes = diagram
        .elements
        .iter()
        .find(|e| e.path == "notes.txt")
        .unwrap();
    assert_eq!(
        (notes.kind, notes.technology.as_str()),
        (ElementKind::Component, "")
    );
}

#[test]
fn excluded_files_are_not_components() {
    let module = fixture("python_app").join("app");
    let all = Options::new()
        .root(fixture("python_app"))
        .depth(Depth::Unlimited);
    let paths = |options: &Options| -> Vec<String> {
        let diagram = generate(&module, options).unwrap();
        diagram
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Component)
            .map(|e| e.path.clone())
            .collect()
    };
    assert!(paths(&all).contains(&"app/storage/drivers/sqlite.py".to_string()));

    let by_name = paths(&all.clone().exclude("drivers"));
    assert!(
        !by_name.iter().any(|p| p.contains("drivers")),
        "{by_name:?}"
    );
    assert!(by_name.contains(&"app/storage/cache.py".to_string()));
    // The line in `repo.py` that names the excluded file is reported.
    let diagram = generate(&module, &all.clone().exclude("drivers")).unwrap();
    assert!(diagram
        .diagnostics
        .iter()
        .any(|d| d.file == "app/storage/repo.py" && d.kind == DiagnosticKind::TargetNotComponent));

    let by_path = paths(&all.clone().exclude("app/storage/cache.py"));
    assert!(!by_path.contains(&"app/storage/cache.py".to_string()));
    assert!(by_path.contains(&"app/storage/drivers/sqlite.py".to_string()));
}

#[test]
fn search_paths_resolve_includes_and_imports() {
    let root = project(
        "search_paths",
        &[
            ("inc/api.h", "// Relationships:\n// None.\n\n#pragma once\nint answer();\n"),
            (
                "src/use.cpp",
                "// Relationships:\n// - `inc/api.h`: Reads the published answer\n\n#include <api.h>\n\nint twice() { return 2 * answer(); }\n",
            ),
            ("lib/helper.py", "# Relationships:\n# None.\n\ndef run():\n    pass\n"),
            (
                "tool.py",
                "# Relationships:\n# - `lib/helper.py`: Runs the shared clean-up step\n\nimport helper\n\nhelper.run()\n",
            ),
        ],
    );
    let kinds = |options: Options| -> Vec<DiagnosticKind> {
        generate(&root, &options.root(&root))
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.kind)
            .collect()
    };
    // A bare `<api.h>` and a bare `import helper` cannot be told apart from
    // system headers and installed packages without a search path.
    assert_eq!(
        kinds(Options::new()),
        vec![DiagnosticKind::Unconfirmed, DiagnosticKind::Unconfirmed]
    );
    assert_eq!(
        kinds(Options::new().search_path("inc").search_path("lib/")),
        vec![]
    );
}

#[test]
fn rust_names_are_followed_across_crates_globs_and_macros() {
    let root = project(
        "rust_workspace",
        &[
            ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"cli\"]\n"),
            ("core/Cargo.toml", "[package]\nname = \"app-core\"\nversion = \"0.1.0\"\n"),
            (
                "core/src/lib.rs",
                "// Relationships:\n// None.\n\npub mod model;\npub mod prelude;\nmod report;\n\n#[macro_export]\nmacro_rules! shout {\n    () => {};\n}\n",
            ),
            ("core/src/model.rs", "// Relationships:\n// None.\n\npub struct Order;\npub struct Invoice;\n"),
            ("core/src/prelude.rs", "// Relationships:\n// None.\n\npub use crate::model::*;\n"),
            (
                "core/src/report.rs",
                "// Relationships:\n// - `core/src/model.rs`: Summarises orders and invoices\n\nuse super::prelude::*;\n\npub fn total(_order: &Order, _invoice: &Invoice) {}\n",
            ),
            ("cli/Cargo.toml", "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n"),
            (
                "cli/src/main.rs",
                "// Relationships:\n// - `core/src/lib.rs`: Announces the start of a run\n// - `core/src/model.rs`: Creates the order to process\n\nuse app_core::prelude::Order;\n\nfn main() {\n    let _order = Order;\n    app_core::shout!();\n}\n",
            ),
        ],
    );
    let options = Options::new().root(&root).depth(Depth::Unlimited);
    let diagram = generate(&root, &options).unwrap();
    assert!(diagram.diagnostics.is_empty(), "{:#?}", diagram.diagnostics);
    assert!(diagram.relations.iter().all(|r| !r.label.is_empty()));
    assert_eq!(diagram.relations.len(), 3);

    // The same code without its lines: both uses in `main.rs` are detected
    // and followed through the re-export to the defining file. What
    // `report.rs` takes through the glob import is deliberately not guessed.
    let stripped = fs::read_to_string(root.join("cli/src/main.rs")).unwrap();
    let body = stripped.split_once("\n\n").unwrap().1;
    fs::write(
        root.join("cli/src/main.rs"),
        format!("// Relationships:\n// None.\n\n{body}"),
    )
    .unwrap();
    let diagram = generate(&root, &options).unwrap();
    let undocumented: Vec<&str> = diagram
        .relations
        .iter()
        .filter(|r| r.label.is_empty())
        .flat_map(|r| r.sources.iter().map(|s| s.target.as_str()))
        .collect();
    assert_eq!(undocumented, vec!["core/src/lib.rs", "core/src/model.rs"]);
    assert_eq!(diagram.diagnostics.len(), 2);
    assert!(diagram
        .diagnostics
        .iter()
        .all(|d| d.kind == DiagnosticKind::Undocumented));
}

#[test]
fn fails_when_there_is_nothing_to_draw() {
    let python = fixture("python_app");
    let with_root = |module: PathBuf| generate(module, &Options::new().root(&python));

    assert!(matches!(
        with_root(python.join("docs")),
        Err(Error::NoComponents { .. })
    ));
    assert!(matches!(
        with_root(python.join("app/session/test_service.py")),
        Err(Error::NotAComponent { .. })
    ));
    assert!(matches!(
        with_root(python.join("no/such/dir")),
        Err(Error::Io { .. })
    ));
    assert!(matches!(
        with_root(fixture("cpp_app")),
        Err(Error::OutsideRoot { .. })
    ));
}

#[test]
fn command_line_prints_the_diagram_and_reports_diagnostics() {
    let root = fixture("python_app");
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_c4gen"))
            .arg("--root")
            .arg(&root)
            .args(extra)
            .arg(root.join("app/session"))
            .output()
            .unwrap()
    };

    let plain = run(&[]);
    assert_eq!(plain.status.code(), Some(0));
    compare(
        "python_session.mmd",
        &String::from_utf8(plain.stdout).unwrap(),
    );
    let stderr = String::from_utf8(plain.stderr).unwrap();
    assert!(stderr.contains("warning[undocumented]"), "{stderr}");

    assert_eq!(run(&["--strict"]).status.code(), Some(1));

    let quiet = run(&[
        "--quiet",
        "--no-code-analysis",
        "--strict",
        "--depth=all",
        "--outside",
        "files",
    ]);
    assert_eq!(quiet.status.code(), Some(0));
    assert!(quiet.stderr.is_empty());

    let bad = run(&["--depth", "none"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty());

    let help = Command::new(env!("CARGO_BIN_EXE_c4gen"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .starts_with("Generate a C4 component diagram"));
}

/// Splits the README around its `index`-th Mermaid block: the text up to
/// and including the opening fence, the diagram, and the rest.
fn readme_block(readme: &str, index: usize) -> (&str, &str, &str) {
    const OPEN: &str = "```mermaid\n";
    let mut start = 0;
    for _ in 0..=index {
        start += readme[start..]
            .find(OPEN)
            .expect("the README has the Mermaid block")
            + OPEN.len();
    }
    let end = start
        + readme[start..]
            .find("```")
            .expect("the Mermaid block is closed");
    (&readme[..start], &readme[start..end], &readme[end..])
}

#[test]
fn readme_example_is_the_real_output() {
    let readme =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
    let readme = readme.replace("\r\n", "\n");
    compare("python_session.mmd", readme_block(&readme, 0).1);
    let diagnostics = fs::read_to_string(golden("python_session.diagnostics.txt")).unwrap();
    for line in diagnostics.lines() {
        assert!(readme.contains(line), "the README does not show: {line}");
    }
}

#[test]
fn this_package_follows_its_own_convention() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let diagram = generate(root.join("src"), &Options::new().root(root)).unwrap();
    assert!(diagram.diagnostics.is_empty(), "{:#?}", diagram.diagnostics);

    // The same holds with every file drawn, which also checks the blocks
    // inside the collapsed `analysis` directory one by one.
    let full = generate(
        root.join("src"),
        &Options::new().root(root).depth(Depth::Unlimited),
    )
    .unwrap();
    assert!(full
        .relations
        .iter()
        .all(|relation| !relation.label.is_empty()));

    let path = root.join("README.md");
    let readme = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    let (before, shown, after) = readme_block(&readme, 1);
    if std::env::var_os("C4GEN_BLESS").is_some() {
        fs::write(&path, format!("{before}{}{after}", diagram.to_mermaid())).unwrap();
    } else {
        assert_eq!(
            shown,
            diagram.to_mermaid(),
            "the diagram of this package in the README is out of date"
        );
    }
}
