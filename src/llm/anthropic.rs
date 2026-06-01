//! Anthropic / Anthropic-compatible Messages API provider with tool use.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};
use crate::llm::models::AgentLoopResult;
use crate::llm::provider::{LlmProvider, ToolExecutor, MAX_ITERATIONS};
use crate::llm::util::{order_attempt_from, ToolAccumulator};
use crate::mcp::McpTool;

pub const DEFAULT_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Provider backed by any Anthropic-compatible `/v1/messages` endpoint.
pub struct AnthropicProvider {
    http: reqwest::Client,
    api_key: String,
    model: String,
    url: String,
    temperature: f32,
    max_tokens: u32,
    label: String,
}

impl AnthropicProvider {
    /// Create a provider. Pass the full messages URL (native or compatible).
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        url: impl Into<String>,
        temperature: f32,
        max_tokens: u32,
    ) -> Self {
        let model = model.into();
        let label = format!("anthropic/{model}");
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            model,
            url: url.into(),
            temperature,
            max_tokens,
            label,
        }
    }

    /// Convert MCP tools into Anthropic tool definitions.
    fn tool_defs(tools: &[McpTool]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect()
    }

    async fn messages(
        &self,
        system: &str,
        messages: &[Value],
        tool_defs: &[Value],
    ) -> Result<Value> {
        let body = json!({
            "model": self.model,
            "system": system,
            "messages": messages,
            "tools": tool_defs,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
        });

        let resp = self
            .http
            .post(&self.url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(TraderError::Llm(format!("HTTP {status}: {text}")));
        }
        serde_json::from_str(&text)
            .map_err(|e| TraderError::LlmParse(format!("{e} (body: {text})")))
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
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
        let mut messages = vec![json!({ "role": "user", "content": user_message })];
        let mut acc = ToolAccumulator::default();

        for iteration in 1..=MAX_ITERATIONS {
            let response = self
                .messages(system_prompt, &messages, &tool_defs)
                .await?;
            let content = response
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let stop_reason = response
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("");

            // Collect any text and tool_use blocks from the assistant turn.
            let mut text_out = String::new();
            let mut tool_uses: Vec<&Value> = Vec::new();
            for block in &content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            text_out.push_str(t);
                        }
                    }
                    Some("tool_use") => tool_uses.push(block),
                    _ => {}
                }
            }

            if stop_reason != "tool_use" || tool_uses.is_empty() {
                // Prepend the system prompt so the saved conversation is self-contained.
                let mut conv = vec![json!({ "role": "system", "content": system_prompt })];
                conv.extend(messages.clone());
                conv.push(json!({ "role": "assistant", "content": &content }));
                return Ok(acc.finish(text_out, iteration, conv));
            }

            // Record the assistant turn verbatim, then answer each tool_use.
            messages.push(json!({ "role": "assistant", "content": content }));

            let mut tool_results = Vec::new();
            for block in tool_uses {
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let args = block.get("input").cloned().unwrap_or(json!({}));

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

                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": result.to_string(),
                }));
            }

            messages.push(json!({ "role": "user", "content": tool_results }));
        }

        Err(TraderError::Llm(format!(
            "agent loop exceeded {MAX_ITERATIONS} iterations without a final answer"
        )))
    }
}
