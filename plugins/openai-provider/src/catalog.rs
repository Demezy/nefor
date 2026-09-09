//! Tool catalog assembled from normalized `tool.register` events.

use std::collections::HashMap;

use serde_json::Value;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolExecution {
    Routed,
    ProviderNative { provider: String },
}

impl ToolExecution {
    fn parse(value: Option<&Value>) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self::Routed);
        };
        let object = value.as_object()?;
        if !object
            .keys()
            .all(|key| matches!(key.as_str(), "kind" | "provider"))
        {
            return None;
        }
        match object.get("kind").and_then(Value::as_str) {
            Some("routed") if !object.contains_key("provider") => Some(Self::Routed),
            Some("provider_native") => object
                .get("provider")
                .and_then(Value::as_str)
                .filter(|provider| !provider.trim().is_empty())
                .map(|provider| Self::ProviderNative {
                    provider: provider.to_owned(),
                }),
            _ => None,
        }
    }

    pub fn is_routed(&self) -> bool {
        matches!(self, Self::Routed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub owner: String,
    pub description: String,
    pub parameters: Value,
    pub execution: ToolExecution,
}

#[derive(Debug, Default)]
pub struct ToolCatalog {
    inner: Mutex<HashMap<String, Vec<ToolSpec>>>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register_from(&self, from: &str, mut tools: Vec<ToolSpec>) {
        for tool in &mut tools {
            if tool.owner.is_empty() {
                tool.owner = from.to_owned();
            }
        }
        let mut catalog = self.inner.lock().await;
        if tools.is_empty() {
            catalog.remove(from);
        } else {
            catalog.insert(from.to_owned(), tools);
        }
    }

    pub async fn to_openai_tools(&self) -> Vec<Value> {
        let catalog = self.inner.lock().await;
        Self::format_openai_tools(catalog.values().flatten())
    }

    pub async fn project_names(&self, names: &[String]) -> Vec<ToolSpec> {
        let catalog = self.inner.lock().await;
        names
            .iter()
            .filter_map(|name| {
                catalog
                    .values()
                    .flat_map(|tools| tools.iter())
                    .find(|tool| &tool.name == name)
                    .cloned()
            })
            .collect()
    }

    pub fn format_openai_tools<'a>(tools: impl IntoIterator<Item = &'a ToolSpec>) -> Vec<Value> {
        tools
            .into_iter()
            .filter(|tool| tool.execution.is_routed())
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect()
    }

    pub async fn owner_of(&self, name: &str) -> Option<String> {
        self.inner
            .lock()
            .await
            .values()
            .flat_map(|tools| tools.iter())
            .find(|tool| tool.name == name && tool.execution.is_routed())
            .map(|tool| tool.owner.clone())
    }

    pub fn parse_tools(value: &Value) -> Vec<ToolSpec> {
        let Some(tools) = value.as_array() else {
            return Vec::new();
        };
        tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?.to_owned();
                let owner = tool
                    .get("owner")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let parameters = tool
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                let execution = ToolExecution::parse(tool.get("execution"))?;
                Some(ToolSpec {
                    name,
                    owner,
                    description,
                    parameters,
                    execution,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn routed(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            owner: "basic-tools".into(),
            description: "tool".into(),
            parameters: json!({"type":"object"}),
            execution: ToolExecution::Routed,
        }
    }

    #[tokio::test]
    async fn generic_openai_excludes_provider_native_from_schema_and_routing() {
        let catalog = ToolCatalog::new();
        catalog
            .register_from(
                "tool-gate",
                vec![
                    routed("read_file"),
                    ToolSpec {
                        name: "web_search".into(),
                        owner: "chatgpt".into(),
                        description: "search".into(),
                        parameters: json!({}),
                        execution: ToolExecution::ProviderNative {
                            provider: "chatgpt".into(),
                        },
                    },
                ],
            )
            .await;
        let tools = catalog.to_openai_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(catalog.owner_of("web_search").await, None);
    }

    #[test]
    fn parses_closed_execution_and_defaults_omission_to_routed() {
        let parsed = ToolCatalog::parse_tools(&json!([
            {"name":"legacy","parameters":{}},
            {"name":"web_search","owner":"chatgpt","parameters":{},"execution":{"kind":"provider_native","provider":"chatgpt"}},
            {"name":"bad","execution":{"kind":"future"}}
        ]));
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].execution.is_routed());
        assert!(!parsed[1].execution.is_routed());
    }
}
