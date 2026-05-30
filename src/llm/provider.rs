//! The provider-agnostic LLM interface and the tool-execution callback.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::Result;
use crate::llm::models::AgentLoopResult;
use crate::mcp::McpTool;

/// Executes a tool call requested by the LLM.
///
/// Implementations decide whether to forward the call to the real broker, block
/// it (dry-run), simulate it (paper trading), or reject it (safety violation).
/// The returned JSON is fed back to the LLM as the tool result.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute `name` with `args`, returning a JSON result for the LLM.
    async fn execute(&self, name: &str, args: Value) -> Result<Value>;

    /// Whether a tool name denotes an order-placement action. Used only for
    /// audit categorisation; safety enforcement lives inside `execute`.
    fn is_order_tool(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        n.contains("order")
            || n.contains("buy")
            || n.contains("sell")
            || n.contains("trade")
    }
}

/// A chat LLM capable of multi-turn tool calling.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Human-readable provider/model label, e.g. `openai/gpt-4o`.
    fn name(&self) -> &str;

    /// Run an agentic tool-calling loop until the model produces a final
    /// response with no further tool calls.
    async fn run_agent_loop(
        &self,
        system_prompt: &str,
        user_message: &str,
        tools: &[McpTool],
        executor: &dyn ToolExecutor,
    ) -> Result<AgentLoopResult>;
}

/// Shared handle to a provider.
pub type DynLlmProvider = std::sync::Arc<dyn LlmProvider>;

/// Maximum agent-loop iterations before we abort to avoid runaway tool calling.
pub const MAX_ITERATIONS: u32 = 12;
