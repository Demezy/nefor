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

    pub fn native_provider(&self) -> Option<&str> {
        match self {
            Self::Routed => None,
            Self::ProviderNative { provider } => Some(provider),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub owner: String,
    pub description: String,
    pub input_schema: Value,
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

    pub async fn all(&self) -> Vec<ToolSpec> {
        self.inner
            .lock()
            .await
            .values()
            .flat_map(|tools| tools.iter().cloned())
            .collect()
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
                let input_schema = tool
                    .get("input_schema")
                    .or_else(|| tool.get("parameters"))
                    .cloned()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                let execution = ToolExecution::parse(tool.get("execution"))?;
                Some(ToolSpec {
                    name,
                    owner,
                    description,
                    input_schema,
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
            input_schema: json!({"type":"object"}),
            execution: ToolExecution::Routed,
        }
    }

    #[tokio::test]
    async fn projects_names_and_routes_only_routed_owners() {
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
                        input_schema: json!({"type":"object"}),
                        execution: ToolExecution::ProviderNative {
                            provider: "chatgpt".into(),
                        },
                    },
                ],
            )
            .await;
        assert_eq!(catalog.project_names(&["web_search".into()]).await.len(), 1);
        assert_eq!(
            catalog.owner_of("read_file").await.as_deref(),
            Some("basic-tools")
        );
        assert_eq!(catalog.owner_of("web_search").await, None);
    }

    #[test]
    fn parses_owner_and_closed_execution_with_routed_omission() {
        let parsed = ToolCatalog::parse_tools(&json!([
            {"name":"read_file","owner":"basic-tools","parameters":{},"execution":{"kind":"routed"}},
            {"name":"legacy","parameters":{}},
            {"name":"web_search","owner":"chatgpt","parameters":{},"execution":{"kind":"provider_native","provider":"chatgpt"}},
            {"name":"bad","execution":{"kind":"future"}}
        ]));
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].owner, "basic-tools");
        assert!(parsed[1].execution.is_routed());
        assert_eq!(parsed[2].execution.native_provider(), Some("chatgpt"));
    }

    #[tokio::test]
    async fn registration_supplies_legacy_owner() {
        let catalog = ToolCatalog::new();
        let mut legacy = routed("read_file");
        legacy.owner.clear();
        catalog.register_from("tool-gate", vec![legacy]).await;
        assert_eq!(
            catalog.owner_of("read_file").await.as_deref(),
            Some("tool-gate")
        );
    }
}
