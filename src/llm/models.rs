//! Shared data types for the LLM agent loop.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A record of one tool invocation made by the LLM during a cycle, captured for
/// the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool: String,
    pub arguments: Value,
    pub result: Value,
    /// Whether the safety layer blocked or simulated this call.
    pub intercepted: bool,
}

/// A best-effort summary of an order the LLM attempted to place, extracted from
/// order-tool calls for quick auditing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAttempt {
    pub tool: String,
    pub symbol: Option<String>,
    pub side: Option<String>,
    pub quantity: Option<f64>,
    pub arguments: Value,
    /// Outcome label: "placed", "simulated", "rejected", or "error".
    pub outcome: String,
    pub detail: Option<String>,
}

/// The result of running a full agentic tool-calling loop for one cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLoopResult {
    /// The LLM's final natural-language summary.
    pub final_response: String,
    /// Every tool call made during the loop.
    pub tool_calls_made: Vec<ToolCallRecord>,
    /// Order attempts extracted from the tool calls.
    pub orders_attempted: Vec<OrderAttempt>,
    /// Number of round-trips made to the LLM.
    pub iterations: u32,
}
