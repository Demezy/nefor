//! OpenAI-compatible chat-completions request/response shapes and SSE
//! parser.
//!
//! Wire shape notes:
//!
//! - Request body is the standard `{model, messages, stream}` object. We
//!   don't expose temperature/top-p in v1 — Ollama defaults are fine and
//!   each provider accepts a different superset of fields.
//! - Streaming responses come back as Server-Sent Events: each frame is a
//!   `data: {…}\n\n` block, terminated by `data: [DONE]`. Each JSON frame
//!   carries one `choices[0].delta.content` chunk. The final frame
//!   (before `[DONE]`) typically carries `finish_reason` and may carry
//!   `usage` (Ollama does include it; OpenAI requires
//!   `stream_options.include_usage`).
//! - Tool-calling responses interleave `choices[0].delta.tool_calls[*]`
//!   chunks: the first chunk per tool-call carries `function.name` + `id`,
//!   subsequent chunks carry incremental `function.arguments` string
//!   fragments. The terminating chunk's `finish_reason` is `"tool_calls"`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One assistant tool call as the model returned it. Used both in the
/// outgoing assistant message (when feeding the model's prior call back
/// in to a follow-up turn) and as the assembled output of the SSE
/// accumulator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// Always `"function"` in v1 — OpenAI's only tool type today. Hard-
    /// coded on the wire so the field shape matches the API exactly.
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// Argument JSON as a single string. Provider-bound serialization
    /// preserves valid JSON verbatim and wraps malformed model output as
    /// a JSON string, so every request satisfies the OpenAI wire contract.
    pub arguments: String,
}

impl Serialize for ToolCallFunction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("ToolCallFunction", 2)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("arguments", &provider_arguments(&self.arguments))?;
        state.end()
    }
}

/// Return an OpenAI-compatible `function.arguments` string without double
/// encoding already-valid JSON. Malformed source remains available to the
/// model as the value of a JSON string.
pub fn provider_arguments(arguments: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(arguments).is_ok() {
        arguments.to_owned()
    } else {
        serde_json::to_string(arguments).unwrap_or_else(|_| "null".to_owned())
    }
}

/// Versioned provider-context format for an OpenAI-compatible assistant
/// message's opaque native reasoning continuation.
pub const REASONING_CONTEXT_FORMAT: &str = "openai-chat-reasoning-v1";

/// Completed structured reasoning blocks for native continuation.
///
/// Restored arrays are kept verbatim. During streaming, adjacent compatible
/// text/summary fragments form one block; opaque details remain discrete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasoningDetails(Vec<Value>);

impl ReasoningDetails {
    pub fn from_value(value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }
        let values = value
            .as_array()
            .ok_or_else(|| "assistant `reasoning_details` must be an array or null".to_owned())?;
        if values.is_empty() {
            return Ok(None);
        }
        if values.iter().any(|detail| !detail.is_object()) {
            return Err("assistant `reasoning_details` entries must be objects".to_owned());
        }
        Ok(Some(Self(values.clone())))
    }

    pub fn from_chunks(chunks: Vec<Value>) -> Option<Self> {
        if chunks.is_empty() {
            return None;
        }
        let mut details = Self(Vec::new());
        details.append_chunks(chunks);
        Some(details)
    }

    pub fn append_chunks(&mut self, chunks: Vec<Value>) {
        for chunk in chunks {
            if self
                .0
                .last_mut()
                .is_some_and(|previous| merge_reasoning_fragment(previous, &chunk))
            {
                continue;
            }
            self.0.push(chunk);
        }
    }

    pub fn as_slice(&self) -> &[Value] {
        &self.0
    }
}

fn reasoning_text_field(detail: &Map<String, Value>) -> Option<&'static str> {
    match detail.get("type").and_then(Value::as_str) {
        Some("reasoning.text") => Some("text"),
        Some("reasoning.summary") => Some("summary"),
        _ => None,
    }
}

fn fragment_text<'a>(detail: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    match detail.get(field) {
        None | Some(Value::Null) => Some(""),
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

fn missing_reasoning_metadata(key: &str, value: &Value) -> bool {
    match key {
        "id" | "format" | "signature" => value.is_null() || value.as_str() == Some(""),
        "index" => value.is_null(),
        _ => false,
    }
}

fn merge_reasoning_fragment(previous: &mut Value, next: &Value) -> bool {
    let (Some(previous), Some(next)) = (previous.as_object_mut(), next.as_object()) else {
        return false;
    };
    let Some(field) = reasoning_text_field(previous) else {
        return false;
    };
    if reasoning_text_field(next) != Some(field) || fragment_text(previous, field).is_none() {
        return false;
    }
    let Some(text) = fragment_text(next, field) else {
        return false;
    };
    // Only the adjacent block can continue. Explicit identity changes and
    // conflicting metadata are boundaries, even when an index is reused.
    if next.iter().any(|(key, value)| {
        key != field
            && previous.get(key).is_some_and(|old| {
                old != value
                    && !missing_reasoning_metadata(key, old)
                    && !missing_reasoning_metadata(key, value)
            })
    }) {
        return false;
    }
    for (key, value) in next {
        if key == field {
            continue;
        }
        if previous
            .get(key)
            .is_none_or(|old| missing_reasoning_metadata(key, old))
        {
            previous.insert(key.clone(), value.clone());
        }
    }
    match previous.get_mut(field) {
        Some(Value::String(accumulated)) => accumulated.push_str(text),
        _ => {
            previous.insert(field.to_owned(), Value::String(text.to_owned()));
        }
    }
    true
}

/// Provider-native assistant reasoning that must be replayed on continuation.
/// The variants are mutually exclusive on the outgoing wire: structured
/// details take precedence over the plaintext `reasoning_content` and
/// `reasoning` aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningContinuation {
    Details { reasoning_details: ReasoningDetails },
    Content { reasoning_content: String },
    Reasoning { reasoning: String },
}

impl ReasoningContinuation {
    pub fn from_object(object: &Map<String, Value>) -> Result<Option<Self>, String> {
        if let Some(value) = object.get("reasoning_details") {
            if let Some(reasoning_details) = ReasoningDetails::from_value(value)? {
                return Ok(Some(Self::Details { reasoning_details }));
            }
        }
        Ok(object
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|content| !content.is_empty())
            .map(|content| Self::Content {
                reasoning_content: content.to_owned(),
            })
            .or_else(|| {
                object
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .filter(|reasoning| !reasoning.is_empty())
                    .map(|reasoning| Self::Reasoning {
                        reasoning: reasoning.to_owned(),
                    })
            }))
    }

    pub fn artifact(&self) -> Value {
        match self {
            Self::Details { reasoning_details } => {
                let mut artifact = Map::new();
                artifact.insert(
                    "reasoning_details".into(),
                    Value::Array(reasoning_details.as_slice().to_vec()),
                );
                Value::Object(artifact)
            }
            Self::Content { reasoning_content } => {
                let mut artifact = Map::new();
                artifact.insert(
                    "reasoning_content".into(),
                    Value::String(reasoning_content.clone()),
                );
                Value::Object(artifact)
            }
            Self::Reasoning { reasoning } => {
                let mut artifact = Map::new();
                artifact.insert("reasoning".into(), Value::String(reasoning.clone()));
                Value::Object(artifact)
            }
        }
    }
}

/// Single chat message in the conversation.
///
/// The OpenAI chat schema has four roles, each with a different field
/// set. This enum encodes that shape directly so invalid combinations
/// (e.g. a user message with tool_calls) are unrepresentable.
///
/// Serde shape: internally tagged on `"role"`, variant names lowercased
/// to match the wire (`{"role": "user", "content": "…"}`).
///
/// Wire serialization quirk: Ollama's `/api/chat` validator rejects
/// `{"role": "assistant", "content": null, "tool_calls": [...]}`
/// with `invalid message content type: <nil>`. The OpenAI spec says
/// null is correct on a tool-calls-only assistant turn, but Ollama's
/// JSON unmarshal trips before reaching the spec-defined branch. We
/// `skip_serializing_if = Option::is_none` on the assistant's content
/// so the field is omitted entirely on that shape — both OpenAI and
/// Ollama accept the missing-field form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        tool_calls: Vec<ToolCall>,
        #[serde(flatten, skip_serializing_if = "Option::is_none", default)]
        reasoning: Option<ReasoningContinuation>,
    },
    System {
        content: String,
    },
    Tool {
        content: String,
        tool_call_id: String,
    },
}

impl Message {
    // --- convenience accessors (minimise diff at call sites) -----------

    pub fn role(&self) -> &str {
        match self {
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::System { .. } => "system",
            Message::Tool { .. } => "tool",
        }
    }

    pub fn content(&self) -> Option<&str> {
        match self {
            Message::User { content, .. }
            | Message::System { content, .. }
            | Message::Tool { content, .. } => Some(content),
            Message::Assistant { content, .. } => content.as_deref(),
        }
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls,
            _ => &[],
        }
    }

    pub fn reasoning(&self) -> Option<&ReasoningContinuation> {
        match self {
            Message::Assistant { reasoning, .. } => reasoning.as_ref(),
            _ => None,
        }
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Message::Tool { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        }
    }

    // --- factory methods (associated functions) ------------------------

    pub fn user<S: Into<String>>(text: S) -> Self {
        Message::User {
            content: text.into(),
        }
    }

    pub fn system<S: Into<String>>(text: S) -> Self {
        Message::System {
            content: text.into(),
        }
    }

    pub fn assistant<S: Into<String>>(text: S) -> Self {
        Message::Assistant {
            content: Some(text.into()),
            tool_calls: Vec::new(),
            reasoning: None,
        }
    }

    /// Assistant message that only emitted tool calls (no prose). The
    /// OpenAI API requires `content: null` rather than `""` on this
    /// shape.
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Message::Assistant {
            content: None,
            tool_calls,
            reasoning: None,
        }
    }

    /// Assistant message that combined prose + tool calls. Used when the
    /// model interleaves text deltas with tool-call deltas in the same
    /// turn.
    pub fn assistant_with_tool_calls<S: Into<String>>(text: S, tool_calls: Vec<ToolCall>) -> Self {
        Message::Assistant {
            content: Some(text.into()),
            tool_calls,
            reasoning: None,
        }
    }

    pub fn with_reasoning(self, reasoning: Option<ReasoningContinuation>) -> Self {
        match self {
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => Message::Assistant {
                content,
                tool_calls,
                reasoning,
            },
            other => other,
        }
    }

    /// Tool result message. `content` carries either the tool's output
    /// string OR an error message (the model doesn't distinguish on the
    /// wire — both are just "what the tool said"). `tool_call_id` MUST
    /// match the corresponding assistant tool_calls entry's `id`.
    pub fn tool_result<S: Into<String>>(tool_call_id: String, content: S) -> Self {
        Message::Tool {
            content: content.into(),
            tool_call_id,
        }
    }
}

/// Body of a streaming chat-completions request.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'a str>,
    /// `{"include_usage": true}` so the final frame carries `usage`. Ollama
    /// includes it unconditionally; OpenAI honours this opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Tool catalog in OpenAI format — each entry is
    /// `{"type":"function","function":{"name":..,"description":..,"parameters":..}}`.
    /// Skip-serialized when empty so the request shape stays identical
    /// to v1 when no tool plugins are attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [serde_json::Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<&'a serde_json::Value>,
}

impl ChatRequest<'_> {
    pub fn into_value_with_additions(
        &self,
        additions: Option<&Map<String, Value>>,
    ) -> Result<Value, RequestAdditionsError> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| RequestAdditionsError::Serialize(error.to_string()))?;
        let object = value
            .as_object_mut()
            .ok_or(RequestAdditionsError::CanonicalRequestNotObject)?;
        if let Some(additions) = additions {
            if let Some(field) = additions.keys().find(|field| object.contains_key(*field)) {
                return Err(RequestAdditionsError::CanonicalFieldCollision(
                    field.clone(),
                ));
            }
            object.extend(additions.clone());
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestAdditionsError {
    #[error("failed to serialize canonical request: {0}")]
    Serialize(String),
    #[error("canonical request did not serialize to a JSON object")]
    CanonicalRequestNotObject,
    #[error("request addition collides with canonical field `{0}`")]
    CanonicalFieldCollision(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// One semantic item carried by an SSE data frame. A frame may contain
/// several items; `Batch` preserves their deterministic wire-semantic order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    CompletionId(String),
    Delta(String),
    Refusal(String),
    ReasoningDelta(String),
    ReasoningContentDelta(String),
    ReasoningDetails(Vec<Value>),
    ToolCallFragment {
        index: usize,
        id: Option<String>,
        kind: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Finish(FinishReason),
    Usage(Usage),
    Error {
        message: String,
        kind: Option<String>,
        code: Option<String>,
    },
    Malformed(String),
    Done,
    Batch(Vec<SseEvent>),
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

impl FinishReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::FunctionCall => "function_call",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "stop" => Some(Self::Stop),
            "length" => Some(Self::Length),
            "tool_calls" => Some(Self::ToolCalls),
            "content_filter" => Some(Self::ContentFilter),
            "function_call" => Some(Self::FunctionCall),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl Usage {
    pub fn into_ncp_value(self) -> Value {
        let mut usage = Map::new();
        if let Some(tokens) = self.prompt_tokens {
            usage.insert("prompt_tokens".into(), Value::Number(tokens.into()));
        }
        if let Some(tokens) = self.completion_tokens {
            usage.insert("completion_tokens".into(), Value::Number(tokens.into()));
        }
        if let Some(tokens) = self.total_tokens {
            usage.insert("total_tokens".into(), Value::Number(tokens.into()));
        }
        if !self.extensions.is_empty() {
            usage.insert(
                "extensions".into(),
                Value::Object(self.extensions.into_iter().collect()),
            );
        }
        Value::Object(usage)
    }
}

/// Parsed model info from `/v1/models`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    /// Context window size if the backend reports it (vLLM, LiteLLM, etc.).
    pub context_window: Option<u64>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
}

/// Parse the `data` array from a `GET /v1/models` response into a sorted
/// alphabetical list of model info. Skips entries without a string `id`.
/// Opportunistically extracts context window from common extension fields
/// (`max_model_len`, `max_input_tokens`, `context_length`, `context_window`).
pub fn parse_models_response(payload: &str) -> Vec<ModelInfo> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = value.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut models: Vec<ModelInfo> = arr
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|v| v.as_str())?.to_owned();
            let context_window = m
                .get("max_model_len")
                .or_else(|| m.get("max_input_tokens"))
                .or_else(|| m.get("context_length"))
                .or_else(|| m.get("context_window"))
                .and_then(|v| v.as_u64());
            let reasoning_efforts = parse_string_array(
                m.get("reasoning_efforts")
                    .or_else(|| m.get("supported_reasoning_efforts"))
                    .or_else(|| m.get("reasoning_effort_levels"))
                    .or_else(|| m.get("thinking_levels")),
            );
            let default_reasoning_effort = m
                .get("default_reasoning_effort")
                .or_else(|| m.get("default_thinking_level"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Some(ModelInfo {
                id,
                context_window,
                reasoning_efforts,
                default_reasoning_effort,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn parse_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse one SSE `data:` payload without discarding co-occurring fields.
/// Semantic items are ordered content, refusal, reasoning, tool-call fragments
/// in array order, finish reason, then usage.
pub fn parse_sse_chunk(payload: &str) -> SseEvent {
    let payload = payload.trim();
    if payload.is_empty() {
        return SseEvent::Empty;
    }
    if payload == "[DONE]" {
        return SseEvent::Done;
    }
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => return SseEvent::Malformed(error.to_string()),
    };

    if let Some(error) = value.get("error").and_then(|value| value.as_object()) {
        let message = error
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("provider returned an error")
            .to_owned();
        let kind = error
            .get("type")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let code = error
            .get("code")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        return SseEvent::Error {
            message,
            kind,
            code,
        };
    }

    let first_choice = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first());
    let delta = first_choice.and_then(|choice| choice.get("delta"));
    let mut events = Vec::new();

    if let Some(id) = value.get("id").and_then(Value::as_str) {
        if !id.is_empty() {
            events.push(SseEvent::CompletionId(id.to_owned()));
        }
    }
    if let Some(content) = delta
        .and_then(|delta| delta.get("content"))
        .and_then(|v| v.as_str())
    {
        if !content.is_empty() {
            events.push(SseEvent::Delta(content.to_owned()));
        }
    }
    if let Some(refusal) = delta
        .and_then(|delta| delta.get("refusal"))
        .and_then(|v| v.as_str())
    {
        if !refusal.is_empty() {
            events.push(SseEvent::Refusal(refusal.to_owned()));
        }
    }
    if let Some(reasoning) = delta
        .and_then(|delta| delta.get("reasoning_content"))
        .and_then(Value::as_str)
    {
        if !reasoning.is_empty() {
            events.push(SseEvent::ReasoningContentDelta(reasoning.to_owned()));
        }
    } else if let Some(reasoning) = delta
        .and_then(|delta| delta.get("reasoning"))
        .and_then(Value::as_str)
    {
        if !reasoning.is_empty() {
            events.push(SseEvent::ReasoningDelta(reasoning.to_owned()));
        }
    }
    if let Some(details) = delta
        .and_then(|delta| delta.get("reasoning_details"))
        .filter(|details| !details.is_null())
    {
        let Some(details) = details.as_array() else {
            return SseEvent::Malformed("reasoning_details chunk must be an array".to_owned());
        };
        if details.iter().any(|detail| !detail.is_object()) {
            return SseEvent::Malformed(
                "reasoning_details chunk entries must be objects".to_owned(),
            );
        }
        if !details.is_empty() {
            events.push(SseEvent::ReasoningDetails(details.clone()));
        }
    }
    if let Some(tool_calls) = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(|value| value.as_array())
    {
        for call in tool_calls {
            let Some(index) = call.get("index").and_then(|value| value.as_u64()) else {
                return SseEvent::Malformed("tool-call chunk is missing required index".to_owned());
            };
            let function = call.get("function");
            events.push(SseEvent::ToolCallFragment {
                index: index as usize,
                id: call.get("id").and_then(|v| v.as_str()).map(str::to_owned),
                kind: call.get("type").and_then(|v| v.as_str()).map(str::to_owned),
                name: function
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                arguments: function
                    .and_then(|v| v.get("arguments"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            });
        }
    }
    if let Some(reason) = first_choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|value| value.as_str())
    {
        let Some(reason) = FinishReason::parse(reason) else {
            return SseEvent::Malformed(format!("unknown finish_reason {reason:?}"));
        };
        events.push(SseEvent::Finish(reason));
    }
    if let Some(raw_usage) = value.get("usage") {
        if raw_usage.is_null() {
            // OpenAI-compatible streams may carry usage: null on every
            // non-terminal chunk when usage reporting is enabled.
        } else if !raw_usage.is_object() {
            return SseEvent::Malformed("usage must be a JSON object or null".to_owned());
        } else {
            match serde_json::from_value::<Usage>(raw_usage.clone()) {
                Ok(usage) => events.push(SseEvent::Usage(usage)),
                Err(error) => {
                    return SseEvent::Malformed(format!("invalid usage payload: {error}"));
                }
            }
        }
    }

    match events.len() {
        0 => SseEvent::Empty,
        1 => events.remove(0),
        _ => SseEvent::Batch(events),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request<'a>(messages: &'a [Message]) -> ChatRequest<'a> {
        ChatRequest {
            model: "test-model",
            messages,
            stream: true,
            reasoning_effort: None,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            tools: None,
            response_format: None,
        }
    }

    #[test]
    fn request_additions_merge_without_overwriting_canonical_fields() {
        let messages = vec![Message::user("answer")];
        let additions = Map::from_iter([
            ("provider_preference".into(), json!({"mode": "balanced"})),
            ("metadata".into(), json!({"trace": false})),
        ]);
        let value = request(&messages)
            .into_value_with_additions(Some(&additions))
            .expect("non-colliding additions");
        assert_eq!(value["model"], "test-model");
        assert_eq!(value["provider_preference"]["mode"], "balanced");
        assert_eq!(value["metadata"]["trace"], false);
    }

    #[test]
    fn request_additions_reject_canonical_field_collision() {
        let messages = vec![Message::user("answer")];
        let additions = Map::from_iter([("model".into(), json!("replacement"))]);
        assert_eq!(
            request(&messages).into_value_with_additions(Some(&additions)),
            Err(RequestAdditionsError::CanonicalFieldCollision(
                "model".into()
            ))
        );
    }

    #[test]
    fn usage_preserves_extensions_and_keeps_missing_standard_fields_absent() {
        let event = parse_sse_chunk(include_str!("../tests/fixtures/terminal_usage_chunk.json"));
        let SseEvent::Batch(events) = event else {
            panic!("fixture should contain id, finish, and usage");
        };
        assert_eq!(
            events[0],
            SseEvent::CompletionId("completion-fixture".into())
        );
        let usage = events
            .into_iter()
            .find_map(|event| match event {
                SseEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage event");
        assert_eq!(usage.prompt_tokens, Some(11));
        assert_eq!(usage.completion_tokens, None);
        assert_eq!(usage.total_tokens, None);
        assert_eq!(usage.extensions["vendor_detail"]["cached"], 4);
        assert_eq!(usage.extensions["charged_units"], "0.0042");
        assert_eq!(
            usage.into_ncp_value(),
            json!({
                "prompt_tokens": 11,
                "extensions": {
                    "vendor_detail": {"cached": 4},
                    "charged_units": "0.0042"
                }
            })
        );
    }

    #[test]
    fn null_usage_is_absent_while_non_null_malformed_usage_is_rejected() {
        assert_eq!(
            parse_sse_chunk(r#"{"choices":[{"delta":{"content":"hello"}}],"usage":null}"#),
            SseEvent::Delta("hello".into())
        );
        assert_eq!(
            parse_sse_chunk(r#"{"choices":[],"usage":null}"#),
            SseEvent::Empty
        );
        for payload in [
            r#"{"choices":[],"usage":"unknown"}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":"eleven"}}"#,
        ] {
            assert!(
                matches!(parse_sse_chunk(payload), SseEvent::Malformed(_)),
                "payload should fail: {payload}"
            );
        }
    }

    #[test]
    fn sse_completion_id_is_optional_and_not_synthesized() {
        assert_eq!(
            parse_sse_chunk(r#"{"choices":[{"finish_reason":"stop"}]}"#),
            SseEvent::Finish(FinishReason::Stop)
        );
    }

    #[test]
    fn chat_request_serializes_native_json_schema_response_format() {
        let messages = vec![Message::user("answer")];
        let response_format = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "mag_output",
                "strict": true,
                "schema": {"type": "string"}
            }
        });
        let request = ChatRequest {
            model: "gpt-5",
            messages: &messages,
            stream: true,
            reasoning_effort: None,
            stream_options: None,
            tools: None,
            response_format: Some(&response_format),
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["response_format"]["type"], "json_schema");
        assert_eq!(
            value["response_format"]["json_schema"]["schema"]["type"],
            "string"
        );
    }

    #[test]
    fn parse_sse_chunk_preserves_mixed_fields_in_order() {
        let payload = r#"{"choices":[{"delta":{"content":"hi","refusal":"no","reasoning":"why","tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
        assert_eq!(
            parse_sse_chunk(payload),
            SseEvent::Batch(vec![
                SseEvent::Delta("hi".into()),
                SseEvent::Refusal("no".into()),
                SseEvent::ReasoningDelta("why".into()),
                SseEvent::ToolCallFragment {
                    index: 0,
                    id: Some("call".into()),
                    kind: Some("function".into()),
                    name: Some("f".into()),
                    arguments: Some("{}".into())
                },
                SseEvent::Finish(FinishReason::ToolCalls),
                SseEvent::Usage(Usage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(2),
                    total_tokens: Some(3),
                    extensions: BTreeMap::new(),
                }),
            ])
        );
    }

    #[test]
    fn parse_sse_chunk_accepts_reasoning_content_without_reclassifying_content() {
        let payload =
            r#"{"choices":[{"delta":{"content":"answer","reasoning_content":"thought"}}]}"#;
        assert_eq!(
            parse_sse_chunk(payload),
            SseEvent::Batch(vec![
                SseEvent::Delta("answer".into()),
                SseEvent::ReasoningContentDelta("thought".into()),
            ])
        );
    }

    #[test]
    fn reasoning_details_absence_null_and_empty_are_absent_but_bad_shapes_fail() {
        assert_eq!(ReasoningContinuation::from_object(&Map::new()), Ok(None));
        for value in [Value::Null, json!([])] {
            let object = Map::from_iter([("reasoning_details".into(), value)]);
            assert_eq!(ReasoningContinuation::from_object(&object), Ok(None));
        }
        for value in [json!("bad"), json!(["bad"]), json!([null])] {
            let object = Map::from_iter([("reasoning_details".into(), value)]);
            assert!(ReasoningContinuation::from_object(&object).is_err());
        }
        assert_eq!(
            parse_sse_chunk(r#"{"choices":[{"delta":{"reasoning_details":null}}]}"#),
            SseEvent::Empty
        );
    }

    #[test]
    fn streamed_reasoning_fragments_preserve_boundaries_and_unknown_metadata() {
        let original = json!({
            "type":"reasoning.text", "text":"first", "index":0,
            "id":"block-a", "format":"provider-v1", "future":{"kept":true}
        });
        for boundary in [
            json!({"type":"reasoning.text","text":"next","index":1}),
            json!({"type":"reasoning.text","text":"next","id":"block-b"}),
            json!({"type":"reasoning.text","text":"next","format":"provider-v2"}),
            json!({"type":"reasoning.text","text":"next","future":{"kept":false}}),
            json!({"type":"reasoning.text","text":["opaque-shape"],"index":0}),
            json!({"type":"reasoning.summary","summary":"next","index":0}),
            json!({"type":"reasoning.encrypted","data":"sealed","index":0}),
            json!({"type":"future.reasoning","text":"opaque","index":0}),
        ] {
            let details = ReasoningDetails::from_chunks(vec![original.clone(), boundary.clone()])
                .expect("nonempty stream");
            assert_eq!(details.as_slice(), [original.clone(), boundary]);
        }
        let opaque = json!({"type":"reasoning.encrypted","data":"sealed","index":0});
        let details = ReasoningDetails::from_chunks(vec![
            original.clone(),
            opaque.clone(),
            opaque.clone(),
            original.clone(),
        ])
        .expect("nonempty stream");
        assert_eq!(
            details.as_slice(),
            [original.clone(), opaque.clone(), opaque, original]
        );
    }

    #[test]
    fn completed_reasoning_blocks_are_not_reinterpreted_as_stream_fragments() {
        let completed = json!([
            {"type":"reasoning.text","text":"one","index":0},
            {"type":"reasoning.text","text":"two","index":0}
        ]);
        let details = ReasoningDetails::from_value(&completed)
            .expect("valid completed details")
            .expect("nonempty details");
        assert_eq!(serde_json::to_value(details).expect("serialize"), completed);
    }

    #[test]
    fn assistant_history_serializes_one_native_reasoning_shape_beside_tool_calls() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: r#"{"path":"/x"}"#.into(),
            },
        }];
        let details = ReasoningDetails::from_value(&json!([
            {"type":"reasoning.encrypted","data":"sealed","index":0,"extra":"kept"}
        ]))
        .expect("valid details")
        .expect("nonempty details");
        let message = Message::assistant_tool_calls(calls.clone()).with_reasoning(Some(
            ReasoningContinuation::Details {
                reasoning_details: details,
            },
        ));
        assert_eq!(
            serde_json::to_value(message).expect("serialize"),
            json!({
                "role": "assistant",
                "tool_calls": calls,
                "reasoning_details": [
                    {"type":"reasoning.encrypted","data":"sealed","index":0,"extra":"kept"}
                ]
            })
        );

        let content =
            Message::assistant("answer").with_reasoning(Some(ReasoningContinuation::Content {
                reasoning_content: "full thought".into(),
            }));
        assert_eq!(
            serde_json::to_value(content).expect("serialize"),
            json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "full thought"
            })
        );
    }

    #[test]
    fn malformed_reasoning_details_fail_at_the_provider_boundary() {
        for payload in [
            r#"{"choices":[{"delta":{"reasoning_details":"opaque"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_details":["not-an-object"]}}]}"#,
        ] {
            assert!(matches!(parse_sse_chunk(payload), SseEvent::Malformed(_)));
        }
    }

    #[test]
    fn assistant_without_native_reasoning_omits_reasoning_fields() {
        let wire = serde_json::to_value(Message::assistant("answer")).expect("assistant wire");
        assert!(wire.get("reasoning_details").is_none());
        assert!(wire.get("reasoning_content").is_none());
    }

    #[test]
    fn parse_sse_chunk_parses_every_standard_finish_reason() {
        for (wire, expected) in [
            ("stop", FinishReason::Stop),
            ("length", FinishReason::Length),
            ("tool_calls", FinishReason::ToolCalls),
            ("content_filter", FinishReason::ContentFilter),
            ("function_call", FinishReason::FunctionCall),
        ] {
            let payload = format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{wire}"}}]}}"#);
            assert_eq!(parse_sse_chunk(&payload), SseEvent::Finish(expected));
        }
    }

    #[test]
    fn parse_sse_chunk_rejects_unknown_finish_and_malformed_json() {
        assert!(matches!(
            parse_sse_chunk(r#"{"choices":[{"finish_reason":"weird"}]}"#),
            SseEvent::Malformed(_)
        ));
        assert!(matches!(parse_sse_chunk("{bad"), SseEvent::Malformed(_)));
    }

    #[test]
    fn parse_sse_chunk_recognizes_standard_error_envelope() {
        assert_eq!(
            parse_sse_chunk(
                r#"{"error":{"message":"bad","type":"invalid_request_error","code":"x"}}"#
            ),
            SseEvent::Error {
                message: "bad".into(),
                kind: Some("invalid_request_error".into()),
                code: Some("x".into())
            }
        );
    }

    #[test]
    fn parse_sse_chunk_handles_done_empty_and_usage_only() {
        assert_eq!(parse_sse_chunk("[DONE]"), SseEvent::Done);
        assert_eq!(parse_sse_chunk(" "), SseEvent::Empty);
        assert!(matches!(
            parse_sse_chunk(r#"{"choices":[],"usage":{"prompt_tokens":1}}"#),
            SseEvent::Usage(_)
        ));
    }

    #[test]
    fn message_helpers_set_role() {
        assert_eq!(Message::user("hi").role(), "user");
        assert_eq!(Message::user("hi").content(), Some("hi"));
        assert_eq!(Message::assistant("yo").role(), "assistant");
        assert_eq!(Message::assistant("yo").content(), Some("yo"));
    }

    #[test]
    fn message_assistant_tool_calls_omits_content_field_on_wire() {
        // Updated from prior null-on-wire shape to absent-field shape.
        // Ollama's `/api/chat` validator rejects `content: null` with
        // `invalid message content type: <nil>`, breaking every
        // multi-tool turn after the lead's first dispatch. Both OpenAI
        // and Ollama accept the field-omitted form (Option::None →
        // skip_serializing_if), so omitting is the safe shape across
        // providers.
        let calls = vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: "{\"path\":\"/x\"}".into(),
            },
        }];
        let msg = Message::assistant_tool_calls(calls);
        assert_eq!(msg.role(), "assistant");
        assert!(msg.content().is_none());
        let v = serde_json::to_value(&msg).expect("ser");
        assert!(
            v.get("content").is_none(),
            "content field must be omitted (not null) for Ollama compatibility"
        );
        assert_eq!(
            v.get("tool_calls")
                .and_then(|c| c.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn message_tool_result_carries_tool_call_id() {
        let m = Message::tool_result("call_1".into(), "file contents");
        assert_eq!(m.role(), "tool");
        assert_eq!(m.tool_call_id(), Some("call_1"));
        let v = serde_json::to_value(&m).expect("ser");
        assert_eq!(
            v.get("tool_call_id").and_then(|s| s.as_str()),
            Some("call_1")
        );
        assert_eq!(
            v.get("content").and_then(|s| s.as_str()),
            Some("file contents")
        );
    }

    #[test]
    fn message_user_serializes_without_tool_fields() {
        let m = Message::user("hi");
        let v = serde_json::to_value(&m).expect("ser");
        assert!(v.get("tool_calls").is_none(), "skip-serialized");
        assert!(v.get("tool_call_id").is_none(), "skip-serialized");
    }

    fn model_ids(models: &[ModelInfo]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn list_models_parses_data_array() {
        let payload = r#"{"data":[{"id":"gpt-4"},{"id":"gpt-3.5"}]}"#;
        let models = parse_models_response(payload);
        assert_eq!(model_ids(&models), vec!["gpt-3.5", "gpt-4"]);
        assert!(models.iter().all(|m| m.context_window.is_none()));
    }

    #[test]
    fn list_models_sorts_alphabetically() {
        let payload = r#"{"data":[{"id":"zebra"},{"id":"apple"},{"id":"mango"}]}"#;
        let models = parse_models_response(payload);
        assert_eq!(model_ids(&models), vec!["apple", "mango", "zebra"]);
    }

    #[test]
    fn list_models_handles_empty_array() {
        assert!(parse_models_response(r#"{"data":[]}"#).is_empty());
        assert!(parse_models_response(r#"{}"#).is_empty());
        assert!(parse_models_response("not json").is_empty());
    }

    #[test]
    fn list_models_skips_entries_without_id() {
        let payload = r#"{"data":[{"id":"a"},{"object":"model"},{"id":"b"}]}"#;
        let models = parse_models_response(payload);
        assert_eq!(model_ids(&models), vec!["a", "b"]);
    }

    #[test]
    fn list_models_extracts_context_window() {
        let payload = r#"{"data":[
            {"id":"vllm-model","max_model_len":32768},
            {"id":"litellm-model","max_input_tokens":128000},
            {"id":"plain-model"}
        ]}"#;
        let models = parse_models_response(payload);
        assert_eq!(models[0].context_window, Some(128000));
        assert_eq!(models[1].context_window, None);
        assert_eq!(models[2].context_window, Some(32768));
    }

    #[test]
    fn provider_tool_arguments_preserve_valid_json_without_double_encoding() {
        let call = ToolCallFunction {
            name: "read_file".into(),
            arguments: r#"{"path":"x"}"#.into(),
        };
        let wire = serde_json::to_value(call).expect("serialize tool call");
        assert_eq!(wire["arguments"], r#"{"path":"x"}"#);
    }

    #[test]
    fn provider_tool_arguments_wrap_malformed_source_once() {
        let raw = r#"{"path":"x""#;
        let call = ToolCallFunction {
            name: "read_file".into(),
            arguments: raw.into(),
        };
        let wire = serde_json::to_value(call).expect("serialize tool call");
        let arguments = wire["arguments"].as_str().expect("arguments string");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
            raw
        );
        assert_ne!(
            serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
            serde_json::to_string(raw).unwrap(),
            "malformed source is not double encoded",
        );
    }

    #[test]
    fn parse_sse_chunk_preserves_empty_arguments_fragment() {
        let payload =
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":""}}]}}]}"#;
        assert_eq!(
            parse_sse_chunk(payload),
            SseEvent::ToolCallFragment {
                index: 0,
                id: None,
                kind: None,
                name: None,
                arguments: Some(String::new()),
            }
        );
    }
}
