use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use nefor_mag::error::MagError;
use serde::Serialize;
use serde_json::{Map, Value};

const ENVELOPE_VERSION: u8 = 1;

#[derive(Parser)]
#[command(
    name = "mag",
    version,
    about = "Compile MAG programs without starting a Nefor runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a MAG program to an artifact; never executes it
    Compile(CompileArgs),
}

#[derive(Args)]
struct CompileArgs {
    /// Entry file, resolved beneath --source-dir
    entry: String,

    /// Directory containing the entry file and files read by the program
    #[arg(long)]
    source_dir: PathBuf,

    /// MAG module search root; repeat for multiple roots (defaults to --source-dir)
    #[arg(long = "module-root")]
    module_roots: Vec<PathBuf>,

    /// Immutable JSON host input as NAME=PATH; repeat for multiple inputs
    #[arg(long = "input", value_name = "NAME=PATH")]
    inputs: Vec<String>,

    /// Include phase timings and deterministic operation counters in the success envelope
    #[arg(long)]
    profile: bool,
}

#[derive(Serialize)]
struct CliEnvelope<T> {
    version: u8,
    ok: bool,
    #[serde(flatten)]
    payload: T,
}

#[derive(Serialize)]
struct Success {
    artifact: Value,
    hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<nefor_mag::profile::CompileProfile>,
}

#[derive(Serialize)]
struct Failure {
    error: Diagnostic,
}

#[derive(Serialize)]
struct Diagnostic {
    code: &'static str,
    stage: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<Box<nefor_mag::diagnostic::SyntaxDiagnostic>>,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Compile(args) => match compile(args) {
            Ok(success) => print_json(&CliEnvelope {
                version: ENVELOPE_VERSION,
                ok: true,
                payload: success,
            }),
            Err(error) => {
                eprintln!("mag: {}", error.message);
                print_json(&CliEnvelope {
                    version: ENVELOPE_VERSION,
                    ok: false,
                    payload: Failure { error },
                });
                std::process::exit(1);
            }
        },
    }
}

fn compile(args: CompileArgs) -> Result<Success, Diagnostic> {
    require_directory(&args.source_dir, "source_dir")?;
    let module_roots = if args.module_roots.is_empty() {
        vec![args.source_dir.clone()]
    } else {
        args.module_roots
    };
    for root in &module_roots {
        require_directory(root, "module_root")?;
    }
    let inputs = load_inputs(&args.inputs)?;
    let profiler = args.profile.then(nefor_mag::profile::CompileProfiler::new);
    let loaded = if let Some(profiler) = &profiler {
        nefor_mag::load_with_profiler(
            &args.source_dir,
            &args.entry,
            inputs,
            &module_roots,
            profiler,
        )
    } else {
        nefor_mag::load_with_inputs_and_module_roots(
            &args.source_dir,
            &args.entry,
            inputs,
            &module_roots,
        )
    }
    .map_err(mag_diagnostic)?;
    Ok(Success {
        artifact: loaded.artifact,
        hash: loaded.hash,
        profile: profiler.map(|profiler| profiler.snapshot()),
    })
}

fn require_directory(path: &Path, kind: &'static str) -> Result<(), Diagnostic> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(path_diagnostic(
            "path_not_directory",
            path,
            format!("{kind} is not a directory: {}", path.display()),
        )),
        Err(error) => Err(path_diagnostic(
            "path_unavailable",
            path,
            format!("cannot access {kind} {}: {error}", path.display()),
        )),
    }
}

fn load_inputs(specs: &[String]) -> Result<Value, Diagnostic> {
    let mut inputs = Map::new();
    for spec in specs {
        let (name, raw_path) = spec.split_once('=').ok_or_else(|| Diagnostic {
            code: "input_argument",
            stage: "input",
            message: format!("host input must be NAME=PATH, got {spec}"),
            path: None,
            diagnostic: None,
        })?;
        if name.is_empty() || raw_path.is_empty() {
            return Err(Diagnostic {
                code: "input_argument",
                stage: "input",
                message: format!("host input must have a non-empty name and path, got {spec}"),
                path: None,
                diagnostic: None,
            });
        }
        if inputs.contains_key(name) {
            return Err(Diagnostic {
                code: "input_duplicate",
                stage: "input",
                message: format!("host input {name:?} was supplied more than once"),
                path: None,
                diagnostic: None,
            });
        }
        let path = PathBuf::from(raw_path);
        let source = std::fs::read_to_string(&path).map_err(|error| {
            path_diagnostic(
                "input_read",
                &path,
                format!("cannot read host input {}: {error}", path.display()),
            )
        })?;
        let value = serde_json::from_str(&source).map_err(|error| {
            path_diagnostic(
                "input_json",
                &path,
                format!("invalid host input JSON {}: {error}", path.display()),
            )
        })?;
        inputs.insert(name.to_owned(), value);
    }
    Ok(Value::Object(inputs))
}

fn mag_diagnostic(error: MagError) -> Diagnostic {
    if let MagError::Syntax(syntax) = error {
        return Diagnostic {
            code: syntax.code,
            stage: syntax.stage,
            message: syntax.message.clone(),
            path: syntax.path.clone(),
            diagnostic: Some(syntax),
        };
    }
    let (code, stage) = match error {
        MagError::Lex(_) => ("syntax_lex", "lex"),
        MagError::Parse(_) => ("syntax_parse", "parse"),
        MagError::Syntax(_) => unreachable!("handled above"),
        MagError::Type(_) | MagError::Unresolved(_) | MagError::Arity { .. } => {
            ("type_error", "typecheck")
        }
        MagError::Budget(_) => ("evaluation_budget", "evaluate"),
        MagError::Eval(_) => ("evaluation_error", "evaluate"),
    };
    Diagnostic {
        code,
        stage,
        message: error.to_string(),
        path: None,
        diagnostic: None,
    }
}

fn path_diagnostic(code: &'static str, path: &Path, message: String) -> Diagnostic {
    Diagnostic {
        code,
        stage: "input",
        message,
        path: Some(path.display().to_string()),
        diagnostic: None,
    }
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("mag: cannot serialize JSON response: {error}");
            std::process::exit(2);
        }
    }
}
