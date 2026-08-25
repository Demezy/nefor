// `write_file` — write a UTF-8 string to a file.
//
// Behavior:
//
// - `path` is required; `content` is required (may be empty — that's a
//   legitimate "truncate to 0" call).
// - Parent directory is created if it doesn't exist (`mkdir -p` semantics).
//   This makes `write_file new/dir/file.txt` work without a separate
//   `mkdir` tool — the LLM's most common shape.
// - If `path` already resolves to a directory the call is rejected
//   ([`ToolError::IsDirectory`]) — overwriting a directory with file content
//   is never the intent.
//
// Trust model matches `read_file` v1: basic-tools is trusted on the bus.
// Path-traversal / sandboxing decisions live in the gate, not here.

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::error::ToolError;

pub const NAME: &str = "write_file";
pub const DESCRIPTION: &str =
    "Write text content to a file, creating parent directories as needed. Overwrites existing files.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute or relative path to the destination file."
            },
            "content": {
                "type": "string",
                "description": "UTF-8 text to write. Existing file is overwritten."
            },
            "cwd": {
                "type": "string",
                "description": "Working directory. Relative paths are resolved against this."
            }
        },
        "required": ["path", "content"]
    })
}

/// Declarative presentation metadata advertised with this tool.
pub fn display() -> Value {
    json!({
        "compact": { "label": "write file", "primary": { "label": "path", "select": { "source": "args", "path": "path" }, "kind": "path" } },
        "expanded": { "label": "write file", "fields": [] },
        "result": { "kind": "receipt", "text": "file written", "fields": [] }
    })
}

pub async fn run(args: &Value) -> Result<String, ToolError> {
    let parsed = parse_args(args)?;
    write_text_file(&parsed.path, &parsed.content).await
}

struct ParsedArgs {
    path: String,
    content: String,
}

fn parse_args(args: &Value) -> Result<ParsedArgs, ToolError> {
    let obj = args.as_object().ok_or_else(|| ToolError::BadArgs {
        tool: NAME.into(),
        message: "args must be a JSON object".into(),
    })?;
    let raw_path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArgs {
            tool: NAME.into(),
            message: "missing required string field `path`".into(),
        })?;
    if raw_path.is_empty() {
        return Err(ToolError::BadArgs {
            tool: NAME.into(),
            message: "`path` must be non-empty".into(),
        });
    }
    let content = obj
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArgs {
            tool: NAME.into(),
            message: "missing required string field `content`".into(),
        })?;
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let path = resolve_path(raw_path, cwd);
    Ok(ParsedArgs {
        path,
        content: content.to_owned(),
    })
}

fn resolve_path(path: &str, cwd: Option<&str>) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_owned();
    }
    match cwd {
        Some(dir) => std::path::Path::new(dir)
            .join(p)
            .to_string_lossy()
            .into_owned(),
        None => path.to_owned(),
    }
}

async fn write_text_file(path: &str, content: &str) -> Result<String, ToolError> {
    // If the path already exists as a directory, refuse — overwriting a
    // directory with file bytes is never the intent and produces a
    // confusing IO error.
    if let Ok(meta) = tokio::fs::metadata(path).await {
        if meta.is_dir() {
            return Err(ToolError::IsDirectory { path: path.into() });
        }
    }

    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::Io {
                    path: path.into(),
                    message: format!("creating parent directory: {e}"),
                })?;
        }
    }

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| ToolError::Io {
            path: path.into(),
            message: e.to_string(),
        })?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| ToolError::Io {
            path: path.into(),
            message: e.to_string(),
        })?;
    file.flush().await.map_err(|e| ToolError::Io {
        path: path.into(),
        message: e.to_string(),
    })?;

    Ok(format!("wrote {} bytes to {}", content.len(), path))
}
