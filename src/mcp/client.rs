//! MCP client over the Streamable HTTP transport.
//!
//! Robinhood exposes its agentic-trading tools as an MCP server. We act as an
//! MCP client: `initialize` to open a session, `tools/list` to enumerate the
//! available tools, and `tools/call` to invoke them. Responses may be returned
//! either as `application/json` or as a `text/event-stream` (SSE) body; both
//! are handled here.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::error::{Result, TraderError};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// A tool advertised by the MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema describing the tool's arguments. Passed through verbatim to
    /// the LLM tool-definition payloads.
    #[serde(rename = "inputSchema", default = "empty_schema")]
    pub input_schema: Value,
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// Client for the Robinhood MCP server.
pub struct RobinhoodMcpClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    session_id: RwLock<Option<String>>,
    next_id: AtomicI64,
}

impl RobinhoodMcpClient {
    /// Build a new client. Call [`connect`](Self::connect) before issuing
    /// tool calls.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            token: token.into(),
            session_id: RwLock::new(None),
            next_id: AtomicI64::new(1),
        })
    }

    /// Perform the MCP initialization handshake and capture the session id.
    pub async fn connect(&self) -> Result<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "trader", "version": env!("CARGO_PKG_VERSION") },
        });
        let (result, session) = self.request_capture_session("initialize", params).await?;
        if let Some(session) = session {
            *self.session_id.write().await = Some(session);
        }
        // Notify the server that initialization is complete (best-effort).
        let _ = self.notify("notifications/initialized", json!({})).await;
        Ok(result)
    }

    /// List the tools advertised by the server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .ok_or_else(|| TraderError::Mcp("tools/list response missing `tools`".into()))?;
        let tools: Vec<McpTool> = serde_json::from_value(tools.clone())
            .map_err(|e| TraderError::Mcp(format!("decoding tool list: {e}")))?;
        Ok(tools)
    }

    /// Invoke a tool by name with the given arguments. Returns the raw `result`
    /// object from the MCP response.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let params = json!({ "name": name, "arguments": arguments });
        self.request("tools/call", params).await
    }

    /// Issue a JSON-RPC request and return the `result` field.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let (result, _) = self.request_capture_session(method, params).await?;
        Ok(result)
    }

    /// Issue a JSON-RPC request, returning both the `result` field and any
    /// `Mcp-Session-Id` header the server set on the response.
    async fn request_capture_session(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(Value, Option<String>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut req = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&body);

        if let Some(session) = self.session_id.read().await.clone() {
            req = req.header("Mcp-Session-Id", session);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let session = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().await?;

        if !status.is_success() {
            return Err(TraderError::Mcp(format!("HTTP {status}: {text}")));
        }

        let message = if content_type.contains("text/event-stream") {
            parse_sse(&text)?
        } else {
            serde_json::from_str(&text)
                .map_err(|e| TraderError::Mcp(format!("decoding JSON response: {e} (body: {text})")))?
        };

        if let Some(error) = message.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let msg = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            return Err(TraderError::McpServer { code, message: msg });
        }

        let result = message
            .get("result")
            .cloned()
            .ok_or_else(|| TraderError::Mcp(format!("response missing `result`: {text}")))?;
        Ok((result, session))
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut req = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&body);
        if let Some(session) = self.session_id.read().await.clone() {
            req = req.header("Mcp-Session-Id", session);
        }
        req.send().await?;
        Ok(())
    }
}

/// Extract the first JSON-RPC message from an SSE stream body.
///
/// SSE frames are separated by blank lines; data payloads are carried on
/// `data:` lines. We concatenate the `data:` lines of the first frame that
/// parses as a JSON object containing `result` or `error`.
fn parse_sse(body: &str) -> Result<Value> {
    let mut data = String::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
        } else if line.trim().is_empty() && !data.is_empty() {
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                if value.get("result").is_some() || value.get("error").is_some() {
                    return Ok(value);
                }
            }
            data.clear();
        }
    }
    if !data.is_empty() {
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            return Ok(value);
        }
    }
    Err(TraderError::Mcp(format!(
        "no JSON-RPC message found in SSE stream: {body}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_extracts_json_message() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let value = parse_sse(body).unwrap();
        assert_eq!(value["result"]["ok"], json!(true));
    }

    #[test]
    fn parse_sse_handles_multiline_data() {
        let body = "data: {\"jsonrpc\":\"2.0\",\ndata: \"result\":{\"v\":2}}\n\n";
        let value = parse_sse(body).unwrap();
        assert_eq!(value["result"]["v"], json!(2));
    }

    #[test]
    fn mcp_tool_deserializes_with_input_schema() {
        let raw = json!({
            "name": "get_portfolio",
            "description": "Get the portfolio",
            "inputSchema": { "type": "object", "properties": {} }
        });
        let tool: McpTool = serde_json::from_value(raw).unwrap();
        assert_eq!(tool.name, "get_portfolio");
        assert_eq!(tool.input_schema["type"], json!("object"));
    }
}
