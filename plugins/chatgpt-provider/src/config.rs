//! CLI surface for the chatgpt-provider binary.
//!
//! The binary runs in one of two modes:
//!
//! - Plugin mode (default — no subcommand). Engines spawn the binary
//!   directly with `--name <prefix>` and optional `--base-url`. The
//!   binary takes over stdio for NCP. **No `--model` flag**: the model
//!   list is fetched from the backend at runtime and the user picks
//!   via `/model` in the chat surface.
//! - `login` subcommand. Interactive OAuth bootstrap; persists tokens
//!   to `$XDG_DATA_HOME/nefor/chatgpt-auth.json` and exits.

use clap::{Parser, Subcommand, ValueEnum};

/// Default plugin identity / event-kind prefix.
pub const DEFAULT_PROVIDER_NAME: &str = "chatgpt";

/// Default base URL for the Responses endpoint on the ChatGPT-
/// subscription path. Overridable via `--base-url` so tests can point
/// at a wiremock instance.
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone, Parser)]
#[command(
    name = "chatgpt-provider",
    about = "NCP plugin: talks to OpenAI's Responses API with ChatGPT-subscription OAuth credentials."
)]
pub struct Cli {
    /// Per-instance identity used as the event-kind prefix. With
    /// `--name chatgpt` the plugin emits `chatgpt.hello`,
    /// `chatgpt.stream.delta`, … and consumes `chatgpt.prompt`.
    #[arg(long = "name", default_value = DEFAULT_PROVIDER_NAME, global = true)]
    pub provider_name: String,

    /// Override the Responses endpoint base URL. The full URL becomes
    /// `{base}/responses` and `{base}/models`.
    #[arg(long = "base-url", default_value = DEFAULT_BASE_URL, value_parser = trim_trailing_slash, global = true)]
    pub base_url: String,

    /// Provider-hosted web search mode for every Responses request.
    #[arg(long = "web-search", value_enum, default_value_t = WebSearchMode::Disabled, global = true)]
    pub web_search: WebSearchMode,

    /// Optional elapsed limit for recovering a transient Responses stream.
    /// Omit it for autonomous recovery that continues until success or cancel.
    #[arg(long = "stream-retry-timeout-seconds", global = true)]
    pub stream_retry_timeout_seconds: Option<u64>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum WebSearchMode {
    #[default]
    Disabled,
    Cached,
    Live,
}

impl WebSearchMode {
    pub fn append_tool(self, tools: &mut Vec<serde_json::Value>) {
        let external_web_access = match self {
            Self::Disabled => return,
            Self::Cached => false,
            Self::Live => true,
        };
        tools.push(serde_json::json!({
            "type": "web_search",
            "external_web_access": external_web_access,
        }));
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Run the OAuth PKCE login flow and persist tokens to disk.
    Login(LoginArgs),
}

#[derive(Debug, Clone, clap::Args)]
pub struct LoginArgs {
    /// Print the authorize URL instead of opening a browser (useful
    /// over SSH).
    #[arg(long, default_value_t = true)]
    pub open_browser: bool,
}

/// Plugin runtime configuration. Built from [`Cli`] at startup; not
/// itself a clap-parsed struct so that downstream callers (dispatcher,
/// tests) can construct it directly.
#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub provider_name: String,
    pub base_url: String,
    pub web_search: WebSearchMode,
    pub stream_retry_timeout_seconds: Option<u64>,
}

impl ServeArgs {
    /// Event-kind prefix derived from `provider_name`, including the
    /// trailing dot. e.g. `provider_name = "chatgpt"` → `"chatgpt."`.
    pub fn event_prefix(&self) -> String {
        format!("{}.", self.provider_name)
    }
}

impl From<&Cli> for ServeArgs {
    fn from(cli: &Cli) -> Self {
        Self {
            provider_name: cli.provider_name.clone(),
            base_url: cli.base_url.clone(),
            web_search: cli.web_search,
            stream_retry_timeout_seconds: cli.stream_retry_timeout_seconds,
        }
    }
}

fn trim_trailing_slash(s: &str) -> Result<String, String> {
    Ok(s.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_parses_with_defaults() {
        let cli = Cli::try_parse_from(["chatgpt-provider"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.provider_name, DEFAULT_PROVIDER_NAME);
        assert_eq!(cli.base_url, DEFAULT_BASE_URL);
        assert_eq!(cli.web_search, WebSearchMode::Disabled);
        assert_eq!(cli.stream_retry_timeout_seconds, None);
    }

    #[test]
    fn name_flag_overrides_provider_name() {
        let cli = Cli::try_parse_from(["chatgpt-provider", "--name", "alt"]).expect("parse");
        assert_eq!(cli.provider_name, "alt");
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        let cli =
            Cli::try_parse_from(["chatgpt-provider", "--base-url", "https://example.com/api/"])
                .expect("parse");
        assert_eq!(cli.base_url, "https://example.com/api");
    }

    #[test]
    fn web_search_accepts_only_closed_modes() {
        for (raw, expected) in [
            ("disabled", WebSearchMode::Disabled),
            ("cached", WebSearchMode::Cached),
            ("live", WebSearchMode::Live),
        ] {
            let cli =
                Cli::try_parse_from(["chatgpt-provider", "--web-search", raw]).expect("parse mode");
            assert_eq!(cli.web_search, expected);
        }
        assert!(Cli::try_parse_from(["chatgpt-provider", "--web-search", "maybe"]).is_err());
    }

    #[test]
    fn stream_retry_timeout_is_optional() {
        let cli =
            Cli::try_parse_from(["chatgpt-provider", "--stream-retry-timeout-seconds", "120"])
                .expect("parse timeout");
        assert_eq!(cli.stream_retry_timeout_seconds, Some(120));
    }

    #[test]
    fn login_subcommand_parses() {
        let cli = Cli::try_parse_from(["chatgpt-provider", "login"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Login(_))));
    }

    #[test]
    fn web_search_tool_is_provider_owned_and_additive() {
        let local = serde_json::json!({"type": "function", "name": "read_file"});
        for (mode, expected) in [
            (WebSearchMode::Disabled, None),
            (WebSearchMode::Cached, Some(false)),
            (WebSearchMode::Live, Some(true)),
        ] {
            let mut tools = vec![local.clone()];
            mode.append_tool(&mut tools);
            assert_eq!(tools[0], local);
            assert_eq!(tools.len(), if expected.is_some() { 2 } else { 1 });
            if let Some(external) = expected {
                assert_eq!(
                    tools[1],
                    serde_json::json!({
                        "type": "web_search",
                        "external_web_access": external,
                    })
                );
            }
        }
    }

    #[test]
    fn serve_args_built_from_cli() {
        let cli = Cli::try_parse_from(["chatgpt-provider", "--name", "alt"]).expect("parse");
        let serve: ServeArgs = (&cli).into();
        assert_eq!(serve.provider_name, "alt");
        assert_eq!(serve.event_prefix(), "alt.");
    }
}
