//! CLI surface for the chatgpt-provider binary.

use clap::{Parser, Subcommand};

pub const DEFAULT_PROVIDER_NAME: &str = "chatgpt";
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone, Parser)]
#[command(
    name = "chatgpt-provider",
    about = "NCP plugin: talks to OpenAI's Responses API with ChatGPT-subscription OAuth credentials."
)]
pub struct Cli {
    #[arg(long = "name", default_value = DEFAULT_PROVIDER_NAME, global = true)]
    pub provider_name: String,

    #[arg(long = "base-url", default_value = DEFAULT_BASE_URL, value_parser = trim_trailing_slash, global = true)]
    pub base_url: String,

    /// Optional elapsed limit for recovering a transient Responses stream.
    /// Omit it for autonomous recovery that continues until success or cancel.
    #[arg(long = "stream-retry-timeout-seconds", global = true)]
    pub stream_retry_timeout_seconds: Option<u64>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Run the OAuth PKCE login flow and persist tokens to disk.
    Login(LoginArgs),
}

#[derive(Debug, Clone, clap::Args)]
pub struct LoginArgs {
    #[arg(long, default_value_t = true)]
    pub open_browser: bool,
}

#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub provider_name: String,
    pub base_url: String,
    pub stream_retry_timeout_seconds: Option<u64>,
}

impl ServeArgs {
    pub fn event_prefix(&self) -> String {
        format!("{}.", self.provider_name)
    }
}

impl From<&Cli> for ServeArgs {
    fn from(cli: &Cli) -> Self {
        Self {
            provider_name: cli.provider_name.clone(),
            base_url: cli.base_url.clone(),
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
        assert_eq!(cli.stream_retry_timeout_seconds, None);
    }

    #[test]
    fn removed_web_search_switch_is_rejected() {
        assert!(Cli::try_parse_from(["chatgpt-provider", "--web-search", "cached"]).is_err());
    }

    #[test]
    fn name_and_base_url_are_configurable() {
        let cli = Cli::try_parse_from([
            "chatgpt-provider",
            "--name",
            "alt",
            "--base-url",
            "https://example.com/api/",
        ])
        .expect("parse");
        assert_eq!(cli.provider_name, "alt");
        assert_eq!(cli.base_url, "https://example.com/api");
        let serve: ServeArgs = (&cli).into();
        assert_eq!(serve.event_prefix(), "alt.");
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
}
