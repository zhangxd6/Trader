//! OpenAI / OpenAI-compatible chat-completions provider with tool calling.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};
use crate::llm::models::AgentLoopResult;
use crate::llm::provider::{LlmProvider, ToolExecutor, MAX_ITERATIONS};
use crate::llm::util::{order_attempt_from, ToolAccumulator};
use crate::mcp::McpTool;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Strip verbose `description` fields from JSON schema properties, keeping
/// only `type`, `enum`, and `required` so tool definitions stay compact.
fn trim_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if k == "description" {
                    // Keep a short version at the top level only.
                    let short: String = v.as_str().unwrap_or("").chars().take(60).collect();
                    if !short.is_empty() {
                        out.insert(k.clone(), Value::String(short));
                    }
                } else if k == "properties" {
                    // Strip descriptions inside each property.
                    if let Value::Object(props) = v {
                        let trimmed: serde_json::Map<String, Value> = props
                            .iter()
                            .map(|(pk, pv)| {
                                let pv2 = if let Value::Object(pm) = pv {
                                    let stripped: serde_json::Map<String, Value> = pm
                                        .iter()
                                        .filter(|(fk, _)| *fk != "description")
                                        .map(|(fk, fv)| (fk.clone(), trim_schema(fv)))
                                        .collect();
                                    Value::Object(stripped)
                                } else {
                                    pv.clone()
                                };
                                (pk.clone(), pv2)
                            })
                            .collect();
                        out.insert(k.clone(), Value::Object(trimmed));
                    }
                } else {
                    out.insert(k.clone(), trim_schema(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(trim_schema).collect()),
        other => other.clone(),
    }
}

/// Provider backed by any OpenAI-compatible `/chat/completions` endpoint.
pub struct OpenAiProvider {
    http: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    temperature: f32,
    max_tokens: u32,
    label: String,
}

impl OpenAiProvider {
    /// Create a provider. `base_url` defaults to the public OpenAI API when
    /// `None`; pass `Some(url)` for compatible servers (Groq, Ollama, ...).
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        temperature: f32,
        max_tokens: u32,
        base_url: Option<&str>,
    ) -> Self {
        let model = model.into();
        let base_url = base_url
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        let label = format!("openai/{model}");
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            model,
            base_url,
            temperature,
            max_tokens,
            label,
        }
    }

    /// Convert MCP tools into OpenAI `function` tool definitions.
    /// Descriptions are capped at 120 chars and parameter descriptions stripped
    /// to keep token usage within free-tier limits.
    fn tool_defs(tools: &[McpTool]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                let desc = t.description.chars().take(120).collect::<String>();
                let params = trim_schema(&t.input_schema);
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": desc,
                        "parameters": params,
                    }
                })
            })
            .collect()
    }

    async fn chat(&self, messages: &[Value], tool_defs: &[Value]) -> Result<Value> {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
        });
        if !tool_defs.is_empty() {
            body["tools"] = json!(tool_defs);
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        }

        // Retry up to 5 times on 429; wait 60 s each time to clear the
        // per-minute token-rate window used by Groq's free tier.
        for attempt in 1..=5u32 {
            let resp = self
                .http
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;

            if status.as_u16() == 429 {
                tracing::warn!("rate limited (attempt {attempt}/5); waiting 60 s for TPM window reset");
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                continue;
            }
            if !status.is_success() {
                return Err(TraderError::Llm(format!("HTTP {status}: {text}")));
            }
            return serde_json::from_str(&text)
                .map_err(|e| TraderError::LlmParse(format!("{e} (body: {text})")));
        }
        Err(TraderError::Llm("rate limit exceeded after 5 retries".into()))
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.label
    }

    async fn run_agent_loop(
        &self,
        system_prompt: &str,
        user_message: &str,
        tools: &[McpTool],
        executor: &dyn ToolExecutor,
    ) -> Result<AgentLoopResult> {
        let tool_defs = Self::tool_defs(tools);
        let mut messages = vec![
            json!({ "role": "system", "content": system_prompt }),
            json!({ "role": "user", "content": user_message }),
        ];
        let mut acc = ToolAccumulator::default();

        for iteration in 1..=MAX_ITERATIONS {
            let response = self.chat(&messages, &tool_defs).await?;
            let choice = response
                .get("choices")
                .and_then(|c| c.get(0))
                .ok_or_else(|| TraderError::LlmParse("response had no choices".into()))?;
            let message = &choice["message"];
            let tool_calls = message.get("tool_calls").and_then(Value::as_array).cloned();

            // No tool calls -> this is the final answer.
            let Some(tool_calls) = tool_calls.filter(|tc| !tc.is_empty()) else {
                let final_response = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return Ok(acc.finish(final_response, iteration));
            };

            // Echo the assistant's tool-call message back into the history.
            messages.push(message.clone());

            for call in &tool_calls {
                let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let func = &call["function"];
                let name = func.get("name").and_then(Value::as_str).unwrap_or("");
                let args: Value = func
                    .get("arguments")
                    .and_then(Value::as_str)
                    .map(|s| serde_json::from_str(s).unwrap_or(json!({})))
                    .unwrap_or(json!({}));

                let is_order = executor.is_order_tool(name);
                let (result, intercepted, outcome, detail) =
                    match executor.execute(name, args.clone()).await {
                        Ok(v) => {
                            let intercepted = v.get("status").and_then(Value::as_str)
                                == Some("dry_run")
                                || v.get("simulated").and_then(Value::as_bool) == Some(true);
                            let outcome = if intercepted { "simulated" } else { "placed" };
                            (v, intercepted, outcome.to_string(), None)
                        }
                        Err(e) => (
                            json!({ "error": e.to_string() }),
                            true,
                            "rejected".to_string(),
                            Some(e.to_string()),
                        ),
                    };

                acc.record(name, &args, &result, intercepted);
                if is_order {
                    acc.push_order(order_attempt_from(name, &args, &outcome, detail));
                }

                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": result.to_string(),
                }));
            }
        }

        Err(TraderError::Llm(format!(
            "agent loop exceeded {MAX_ITERATIONS} iterations without a final answer"
        )))
    }
}
