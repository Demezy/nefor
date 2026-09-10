use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::responses::{Reasoning, ResponseItem};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchRequest {
    pub id: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<SearchInput>,
    pub commands: WebCommand,
    pub settings: SearchSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

impl SearchRequest {
    pub fn new(
        session_id: impl Into<String>,
        model: impl Into<String>,
        command: WebCommand,
    ) -> Self {
        Self {
            id: session_id.into(),
            model: model.into(),
            reasoning: None,
            input: None,
            commands: command,
            settings: SearchSettings::direct_live(),
            max_output_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum SearchInput {
    Text(String),
    Items(Vec<ResponseItem>),
}

/// The project-owned command surface is closed over the six routed operations.
/// Serializing a value always produces an object with exactly one operation family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebCommand {
    Search(SearchQuery),
    ImageSearch(SearchQuery),
    Open(OpenOperation),
    Click(ClickOperation),
    Find(FindOperation),
    Screenshot(ScreenshotOperation),
}

impl Serialize for WebCommand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (name, operation) = match self {
            Self::Search(operation) => ("search_query", serde_json::to_value(operation)),
            Self::ImageSearch(operation) => ("image_query", serde_json::to_value(operation)),
            Self::Open(operation) => ("open", serde_json::to_value(operation)),
            Self::Click(operation) => ("click", serde_json::to_value(operation)),
            Self::Find(operation) => ("find", serde_json::to_value(operation)),
            Self::Screenshot(operation) => ("screenshot", serde_json::to_value(operation)),
        };
        let operation = operation.map_err(serde::ser::Error::custom)?;
        let mut commands = Map::new();
        commands.insert(name.to_string(), Value::Array(vec![operation]));
        commands.serialize(serializer)
    }
}

impl WebCommand {
    pub fn from_commands(value: &Value) -> Result<Self, String> {
        let commands = value
            .as_object()
            .ok_or_else(|| "web request `commands` must be an object".to_owned())?;
        if commands.len() != 1 {
            return Err("web request `commands` must contain exactly one operation family".into());
        }
        let (family, operations) = commands.iter().next().ok_or_else(|| {
            "web request `commands` must contain exactly one operation family".to_owned()
        })?;
        let operations = operations
            .as_array()
            .ok_or_else(|| format!("web request command `{family}` must be a one-element array"))?;
        if operations.len() != 1 {
            return Err(format!(
                "web request command `{family}` must contain exactly one operation"
            ));
        }
        let operation = operations[0].clone();
        let decode =
            |error: serde_json::Error| format!("invalid web request command `{family}`: {error}");
        let command = match family.as_str() {
            "search_query" => serde_json::from_value(operation)
                .map(Self::Search)
                .map_err(decode),
            "image_query" => serde_json::from_value(operation)
                .map(Self::ImageSearch)
                .map_err(decode),
            "open" => serde_json::from_value(operation)
                .map(Self::Open)
                .map_err(decode),
            "click" => serde_json::from_value(operation)
                .map(Self::Click)
                .map_err(decode),
            "find" => serde_json::from_value(operation)
                .map(Self::Find)
                .map_err(decode),
            "screenshot" => serde_json::from_value(operation)
                .map(Self::Screenshot)
                .map_err(decode),
            _ => Err(format!("unsupported web request command family `{family}`")),
        }?;
        command.validate()?;
        Ok(command)
    }

    fn validate(&self) -> Result<(), String> {
        fn nonempty(value: &str, field: &str) -> Result<(), String> {
            if value.trim().is_empty() {
                Err(format!("web request field `{field}` must be non-empty"))
            } else {
                Ok(())
            }
        }

        match self {
            Self::Search(query) | Self::ImageSearch(query) => {
                nonempty(&query.q, "q")?;
                if let Some(domains) = &query.domains {
                    for domain in domains {
                        nonempty(domain, "domains[]")?;
                    }
                }
            }
            Self::Open(operation) => nonempty(&operation.ref_id, "ref_id")?,
            Self::Click(operation) => nonempty(&operation.ref_id, "ref_id")?,
            Self::Find(operation) => {
                nonempty(&operation.ref_id, "ref_id")?;
                nonempty(&operation.pattern, "pattern")?;
            }
            Self::Screenshot(operation) => nonempty(&operation.ref_id, "ref_id")?,
        }
        Ok(())
    }

    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Search(_) => "web_search",
            Self::ImageSearch(_) => "web_image_search",
            Self::Open(_) => "web_open",
            Self::Click(_) => "web_click",
            Self::Find(_) => "web_find",
            Self::Screenshot(_) => "web_screenshot",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub q: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recency: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenOperation {
    pub ref_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClickOperation {
    pub ref_id: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FindOperation {
    pub ref_id: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotOperation {
    pub ref_id: String,
    /// Zero-indexed PDF page number.
    pub pageno: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchSettings {
    pub allowed_callers: Vec<AllowedCaller>,
    pub external_web_access: bool,
}

impl SearchSettings {
    pub fn direct_live() -> Self {
        Self {
            allowed_callers: vec![AllowedCaller::Direct],
            external_web_access: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AllowedCaller {
    Direct,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub encrypted_output: Option<String>,
    pub output: String,
    /// Endpoint result DTOs remain opaque so unknown variants and fields survive.
    #[serde(default)]
    pub results: Option<Vec<Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_supported_command_serializes_one_operation_family() {
        let cases = [
            (
                WebCommand::Search(SearchQuery {
                    q: "OpenAI".into(),
                    recency: Some(7),
                    domains: Some(vec!["openai.com".into()]),
                }),
                json!({"search_query": [{"q": "OpenAI", "recency": 7, "domains": ["openai.com"]}]}),
            ),
            (
                WebCommand::ImageSearch(SearchQuery {
                    q: "waterfalls".into(),
                    recency: None,
                    domains: None,
                }),
                json!({"image_query": [{"q": "waterfalls"}]}),
            ),
            (
                WebCommand::Open(OpenOperation {
                    ref_id: "turn0search0".into(),
                    lineno: Some(12),
                }),
                json!({"open": [{"ref_id": "turn0search0", "lineno": 12}]}),
            ),
            (
                WebCommand::Click(ClickOperation {
                    ref_id: "turn0fetch0".into(),
                    id: 4,
                }),
                json!({"click": [{"ref_id": "turn0fetch0", "id": 4}]}),
            ),
            (
                WebCommand::Find(FindOperation {
                    ref_id: "turn0fetch0".into(),
                    pattern: "installation".into(),
                }),
                json!({"find": [{"ref_id": "turn0fetch0", "pattern": "installation"}]}),
            ),
            (
                WebCommand::Screenshot(ScreenshotOperation {
                    ref_id: "turn0view0".into(),
                    pageno: 0,
                }),
                json!({"screenshot": [{"ref_id": "turn0view0", "pageno": 0}]}),
            ),
        ];

        for (command, expected) in cases {
            assert_eq!(
                serde_json::to_value(command).expect("serialize command"),
                expected
            );
        }
    }

    #[test]
    fn request_defaults_to_direct_external_access() {
        let request = SearchRequest::new(
            "stable-session",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );

        assert_eq!(
            serde_json::to_value(request).expect("serialize request"),
            json!({
                "id": "stable-session",
                "model": "gpt-test",
                "commands": {"search_query": [{"q": "query"}]},
                "settings": {
                    "allowed_callers": ["direct"],
                    "external_web_access": true
                }
            })
        );
    }

    #[test]
    fn private_commands_parse_only_the_closed_six_family_surface() {
        let image = WebCommand::from_commands(&json!({
            "image_query": [{"q": "waterfalls", "recency": 2}]
        }))
        .expect("image command");
        assert!(matches!(image, WebCommand::ImageSearch(_)));
        assert_eq!(image.tool_name(), "web_image_search");

        for invalid in [
            json!({}),
            json!({"search_query": [{"q":"a"}], "open": [{"ref_id":"x"}]}),
            json!({"weather": [{"location":"Paris"}]}),
            json!({"screenshot": []}),
            json!({"search_query": [{"q":""}]}),
            json!({"search_query": [{"q":"ok", "domains":[""]}]}),
            json!({"open": [{"ref_id":""}]}),
            json!({"find": [{"ref_id":"ref", "pattern":""}]}),
            json!({"click": [{"ref_id":"ref", "id":1, "unknown":true}]}),
            json!({"image_query": [{"q":"images", "unknown":true}]}),
        ] {
            assert!(WebCommand::from_commands(&invalid).is_err(), "{invalid}");
        }
    }
}
