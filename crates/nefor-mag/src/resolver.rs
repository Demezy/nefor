use crate::error::MagError;
use std::path::{Component, Path, PathBuf};

pub(crate) fn resolve_workspace_path(root: &Path, relative: &str) -> Result<PathBuf, MagError> {
    let p = Path::new(relative);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(MagError::Eval(format!(
            "path escapes workspace: {relative}"
        )));
    }
    let joined = root.join(p);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.into());
    if let Some(parent) = joined.parent() {
        let canonical_parent = parent.canonicalize().unwrap_or_else(|_| parent.into());
        if !canonical_parent.starts_with(canonical_root) {
            return Err(MagError::Eval(format!(
                "path escapes workspace: {relative}"
            )));
        }
    }
    Ok(joined)
}

fn module_stem(name: &str) -> Result<String, MagError> {
    if name.split('.').any(|part| {
        part.is_empty()
            || !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) {
        return Err(MagError::Eval(format!("invalid module name: {name}")));
    }
    Ok(name.replace('.', "/"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedModule {
    pub path: PathBuf,
    pub syntax: crate::frontend::SyntaxMode,
}

pub(crate) fn resolve_module(roots: &[PathBuf], name: &str) -> Result<ResolvedModule, MagError> {
    let stem = module_stem(name)?;
    let mut matches = roots
        .iter()
        .flat_map(|root| {
            [
                (format!("{stem}.mag"), crate::frontend::SyntaxMode::New),
                (format!("{stem}.magl"), crate::frontend::SyntaxMode::Lisp),
            ]
            .into_iter()
            .filter_map(|(relative, syntax)| {
                let path = resolve_workspace_path(root, &relative).ok()?;
                path.is_file().then(|| ResolvedModule {
                    path: path.canonicalize().unwrap_or(path),
                    syntax,
                })
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    matches.dedup_by(|left, right| left.path == right.path && left.syntax == right.syntax);
    match matches.as_slice() {
        [resolved] => Ok(resolved.clone()),
        [] => Err(MagError::Eval(format!(
            "cannot find module {name} in search roots"
        ))),
        paths => Err(MagError::Eval(format!(
            "module {name} is ambiguous across search roots or syntax suffixes: {}",
            paths
                .iter()
                .map(|resolved| resolved.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub(crate) fn resolve_json(roots: &[PathBuf], path: &str) -> Result<PathBuf, MagError> {
    let mut matches = roots
        .iter()
        .map(PathBuf::as_path)
        .filter_map(|root| {
            let candidate = resolve_workspace_path(root, path).ok()?;
            candidate
                .is_file()
                .then(|| candidate.canonicalize().unwrap_or(candidate))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    let full = match matches.as_slice() {
        [path] => path,
        [] => {
            return Err(MagError::Eval(format!(
                "cannot find JSON data {path} in source or module roots"
            )))
        }
        paths => {
            return Err(MagError::Eval(format!(
                "JSON data {path} is ambiguous across source and module roots: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    };
    Ok(full.clone())
}

/// Statically indexes direct declarations for advisory unresolved-name diagnostics.
/// Candidate modules are parsed but never evaluated, imported, observed, or recursed into.
pub(crate) fn import_suggestions(roots: &[PathBuf], unresolved: &str) -> Vec<String> {
    let mut identities = std::collections::BTreeSet::new();
    for root in roots {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        collect_module_identities(
            root,
            root,
            &canonical_root,
            &mut Vec::new(),
            &mut identities,
        );
    }

    let qualified = unresolved.rsplit_once('.');
    let mut suggestions = std::collections::BTreeSet::new();
    for identity in identities {
        if qualified.is_some_and(|(module, _)| module != identity) {
            continue;
        }
        let Ok(resolved) = resolve_module(roots, &identity) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(&resolved.path) else {
            continue;
        };
        let snapshot = crate::diagnostic::SourceSnapshot::file(&resolved.path, &source);
        let Ok(module) = crate::frontend::compile_source(
            resolved.syntax,
            &snapshot,
            None,
            crate::frontend::SourceRole::Module,
        ) else {
            continue;
        };
        let export = qualified.map_or(unresolved, |(_, export)| export);
        let directly_declares = module.forms.iter().any(|form| match form {
            crate::authored::Form::Type(declaration) => {
                declaration.name == export && !declaration.name.contains('.')
            }
            crate::authored::Form::Block(crate::authored::BlockItem::Let { name, .. }) => {
                name == export
            }
            _ => false,
        });
        if directly_declares {
            suggestions.insert(if qualified.is_some() {
                format!("import {identity}.{{}}")
            } else {
                format!("import {identity}.{{{unresolved}}}")
            });
        }
    }
    suggestions.into_iter().collect()
}

fn collect_module_identities(
    root: &Path,
    directory: &Path,
    canonical_root: &Path,
    ancestry: &mut Vec<PathBuf>,
    out: &mut std::collections::BTreeSet<String>,
) {
    let Ok(canonical_directory) = directory.canonicalize() else {
        return;
    };
    if !canonical_directory.starts_with(canonical_root) || ancestry.contains(&canonical_directory) {
        return;
    }
    ancestry.push(canonical_directory);
    let Ok(entries) = std::fs::read_dir(directory) else {
        ancestry.pop();
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() || file_type.is_symlink() && path.is_dir() {
            collect_module_identities(root, &path, canonical_root, ancestry, out);
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(extension, "mag" | "magl") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let identity = relative
            .with_extension("")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, ".");
        if !identity.is_empty() && module_stem(&identity).is_ok() {
            out.insert(identity);
        }
    }
    ancestry.pop();
}
