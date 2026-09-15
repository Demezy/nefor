use crate::authored;
use crate::diagnostic::SourceSnapshot;
use crate::error::MagError;
use crate::profile::CompileProfiler;

/// The source grammar selected for one MAG source snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyntaxMode {
    /// The nominal, expression-oriented MAG syntax.
    #[default]
    New,
    /// The explicit compatibility frontend for prefix Lisp MAG.
    Lisp,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SourceRole {
    Entry,
    Module,
}

pub(crate) fn compile_source(
    syntax: SyntaxMode,
    source: &SourceSnapshot,
    profiler: Option<&CompileProfiler>,
    role: SourceRole,
) -> Result<authored::Module, MagError> {
    let result = match syntax {
        SyntaxMode::New => crate::new_syntax::compile_source(source, profiler, role),
        SyntaxMode::Lisp => crate::lisp::compile_source(source, profiler, role),
    };
    result.map_err(|mut error| {
        if let MagError::Syntax(diagnostic) = &mut error {
            diagnostic.syntax = Some(
                match syntax {
                    SyntaxMode::New => "new",
                    SyntaxMode::Lisp => "lisp",
                }
                .into(),
            );
        }
        error
    })
}
