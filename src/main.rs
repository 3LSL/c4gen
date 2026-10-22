// Relationships:
// - `external:command-line-user` (inbound): Requests a diagram for a module path
// - `src/diagram.rs`: Renders the diagram and reads its diagnostics
// - `src/lib.rs`: Generates the diagram for the requested module
// - `src/options.rs`: Builds generation settings from command-line arguments

//! Command-line front end for the `c4gen` library.

use std::env;
use std::fs;
use std::process::ExitCode;

use c4gen::{generate, Depth, Diagram, Options, Outside};

const USAGE: &str = "\
Generate a C4 component diagram of a software module, in Mermaid syntax.

Usage: c4gen [OPTIONS] <MODULE>

Arguments:
  <MODULE>  Directory or source file to draw

Options:
      --root <DIR>          Repository root that target paths are relative to
                            [default: nearest directory above MODULE that
                            contains .git, .hg or .svn]
      --depth <N|all>       Directory levels of the module drawn in full; deeper
                            directories become one box each [default: 1]
      --outside <MODE>      How project files outside the module are drawn:
                            collapse, files or hide [default: collapse]
      --exclude <ENTRY>     File or directory to skip, by name or by path from
                            the root; may be repeated
      --no-default-excludes Do not skip the directories that are skipped by
                            default (target, node_modules, vendor, build, ...)
      --search-path <DIR>   Directory, relative to the root, searched first for
                            C/C++ includes and Python and TypeScript imports;
                            may be repeated
      --no-code-analysis    Build the diagram from Relationships blocks alone
      --title <TEXT>        Diagram title
  -o, --output <FILE>       Write the diagram to FILE instead of standard output
      --strict              Exit with status 1 if any diagnostic is reported
  -q, --quiet               Do not print diagnostics
  -h, --help                Print this help
  -V, --version             Print the version

The diagram goes to standard output and diagnostics go to standard error.
Exit status: 0 on success, 1 with --strict when diagnostics were reported,
2 when no diagram could be generated.
";

struct Cli {
    module: String,
    options: Options,
    output: Option<String>,
    strict: bool,
    quiet: bool,
}

enum Command {
    Run(Box<Cli>),
    Help,
    Version,
}

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("c4gen: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let cli = match parse(args)? {
        Command::Run(cli) => cli,
        Command::Help => {
            print!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Command::Version => {
            println!("c4gen {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
    };

    let diagram: Diagram =
        generate(&cli.module, &cli.options).map_err(|error| error.to_string())?;
    let text = diagram.to_mermaid();
    match &cli.output {
        Some(path) => {
            fs::write(path, &text).map_err(|error| format!("cannot write {path}: {error}"))?
        }
        None => print!("{text}"),
    }
    if !cli.quiet {
        for diagnostic in &diagram.diagnostics {
            eprintln!("{diagnostic}");
        }
    }
    if cli.strict && !diagram.diagnostics.is_empty() {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

fn parse(args: Vec<String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut excludes = Vec::new();
    let mut default_excludes = true;
    let mut module = None;
    let mut output = None;
    let mut strict = false;
    let mut quiet = false;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        // `--name=value` and `--name value` are both accepted.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let value = |args: &mut std::vec::IntoIter<String>| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--root" => options.root = Some(value(&mut args)?.into()),
            "--depth" => {
                let text = value(&mut args)?;
                options.depth = match text.as_str() {
                    "all" => Depth::Unlimited,
                    number => match number.parse::<usize>() {
                        Ok(levels) if levels >= 1 => Depth::Levels(levels),
                        _ => {
                            return Err(format!(
                                "--depth takes a number from 1 or `all`, not `{text}`"
                            ))
                        }
                    },
                };
            }
            "--outside" => {
                let text = value(&mut args)?;
                options.outside = match text.as_str() {
                    "collapse" => Outside::Collapse,
                    "files" => Outside::Files,
                    "hide" => Outside::Hide,
                    _ => {
                        return Err(format!(
                            "--outside takes collapse, files or hide, not `{text}`"
                        ))
                    }
                };
            }
            "--exclude" => excludes.push(value(&mut args)?),
            "--no-default-excludes" => default_excludes = false,
            "--search-path" => options.search_paths.push(value(&mut args)?),
            "--no-code-analysis" => options.code_analysis = false,
            "--title" => options.title = Some(value(&mut args)?),
            "-o" | "--output" => output = Some(value(&mut args)?),
            "--strict" => strict = true,
            "-q" | "--quiet" => quiet = true,
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option `{flag}`; try --help"));
            }
            _ => {
                if module.replace(arg).is_some() {
                    return Err("only one module path can be given; try --help".to_string());
                }
            }
        }
    }

    if !default_excludes {
        options.exclude.clear();
    }
    options.exclude.extend(excludes);
    let module = module.ok_or("no module path given; try --help")?;
    Ok(Command::Run(Box::new(Cli {
        module,
        options,
        output,
        strict,
        quiet,
    })))
}
