//! OpenAI / OpenAI-compatible chat-completions provider with tool calling.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};
use crate::llm::models::AgentLoopResult;
use crate::llm::provider::{LlmProvider, ToolExecutor, MAX_ITERATIONS};
use crate::llm::util::{order_attempt_from, ToolAccumulator};
use crate::mcp::McpTool;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

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
    fn tool_defs(tools: &[McpTool]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
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
        }

        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
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
