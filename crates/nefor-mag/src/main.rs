use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use nefor_mag::error::MagError;
use serde::Serialize;
use serde_json::{Map, Value};

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

    /// Print phase timings and deterministic operation counters to stderr
    #[arg(long)]
    profile: bool,

    /// Maximum evaluator steps per compilation
    #[arg(long)]
    evaluation_step_limit: Option<u64>,

    /// Maximum nested function calls
    #[arg(long)]
    call_depth_limit: Option<u16>,

    /// Maximum nested expressions
    #[arg(long)]
    expression_depth_limit: Option<u16>,

    /// Maximum memoized function calls; zero disables memoization
    #[arg(long)]
    memoized_call_limit: Option<usize>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<nefor_mag::profile::CompileProfile>,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Compile(args) => match compile(args) {
            Ok((artifact, profile)) => {
                print_json_stdout(&artifact);
                if let Some(profile) = profile {
                    print_json_stderr(&profile);
                }
            }
            Err(error) => {
                print_json_stderr(&error);
                std::process::exit(1);
            }
        },
    }
}

fn compile(
    args: CompileArgs,
) -> Result<(Value, Option<nefor_mag::profile::CompileProfile>), Diagnostic> {
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
    let defaults = nefor_mag::CompilerLimits::default();
    let options = nefor_mag::CompilerOptions {
        limits: nefor_mag::CompilerLimits {
            evaluation_steps: args
                .evaluation_step_limit
                .unwrap_or(defaults.evaluation_steps),
            call_depth: args.call_depth_limit.unwrap_or(defaults.call_depth),
            expression_depth: args
                .expression_depth_limit
                .unwrap_or(defaults.expression_depth),
            memoized_calls: args.memoized_call_limit.unwrap_or(defaults.memoized_calls),
        },
    };
    let profiler = args.profile.then(nefor_mag::profile::CompileProfiler::new);
    let artifact = if let Some(profiler) = &profiler {
        nefor_mag::compile_file_with_profiler_and_options(
            &args.source_dir,
            &args.entry,
            inputs,
            &module_roots,
            profiler,
            options,
        )
    } else {
        nefor_mag::compile_file_with_inputs_and_module_roots_and_options(
            &args.source_dir,
            &args.entry,
            inputs,
            &module_roots,
            options,
        )
    };
    match artifact {
        Ok(artifact) => Ok((artifact, profiler.map(|profiler| profiler.snapshot()))),
        Err(error) => {
            let mut diagnostic = mag_diagnostic(error);
            diagnostic.profile = profiler.map(|profiler| profiler.snapshot());
            Err(diagnostic)
        }
    }
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
            profile: None,
        })?;
        if name.is_empty() || raw_path.is_empty() {
            return Err(Diagnostic {
                code: "input_argument",
                stage: "input",
                message: format!("host input must have a non-empty name and path, got {spec}"),
                path: None,
                diagnostic: None,
                profile: None,
            });
        }
        if inputs.contains_key(name) {
            return Err(Diagnostic {
                code: "input_duplicate",
                stage: "input",
                message: format!("host input {name:?} was supplied more than once"),
                path: None,
                diagnostic: None,
                profile: None,
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
            profile: None,
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
        profile: None,
    }
}

fn path_diagnostic(code: &'static str, path: &Path, message: String) -> Diagnostic {
    Diagnostic {
        code,
        stage: "input",
        message,
        path: Some(path.display().to_string()),
        diagnostic: None,
        profile: None,
    }
}

fn print_json_stdout<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("mag: cannot serialize JSON response: {error}");
            std::process::exit(2);
        }
    }
}

fn print_json_stderr<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(json) => eprintln!("{json}"),
        Err(error) => {
            eprintln!("mag: cannot serialize JSON diagnostic: {error}");
            std::process::exit(2);
        }
    }
}
