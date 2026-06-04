//! Helpers shared by the LLM provider implementations.

use serde_json::Value;

use crate::llm::models::{AgentLoopResult, OrderAttempt, ToolCallRecord};

/// Accumulates tool-call records and order attempts during an agent loop.
#[derive(Default)]
pub struct ToolAccumulator {
    tool_calls: Vec<ToolCallRecord>,
    orders: Vec<OrderAttempt>,
    /// All assistant text from every turn (intermediate + final).
    text_parts: Vec<String>,
}

impl ToolAccumulator {
    /// Record a completed tool call.
    pub fn record(&mut self, tool: &str, args: &Value, result: &Value, intercepted: bool) {
        self.tool_calls.push(ToolCallRecord {
            tool: tool.to_string(),
            arguments: args.clone(),
            result: result.clone(),
            intercepted,
        });
    }

    /// Record an order attempt extracted from a tool call.
    pub fn push_order(&mut self, order: OrderAttempt) {
        self.orders.push(order);
    }

    /// Append assistant text emitted during an intermediate turn (alongside tool calls).
    pub fn push_text(&mut self, text: &str) {
        if !text.trim().is_empty() {
            self.text_parts.push(text.to_string());
        }
    }

    /// Finalise into an [`AgentLoopResult`].
    pub fn finish(
        mut self,
        final_response: String,
        iterations: u32,
        conversation: Vec<serde_json::Value>,
    ) -> AgentLoopResult {
        // Include the final response in the full reasoning too.
        if !final_response.trim().is_empty() {
            self.text_parts.push(final_response.clone());
        }
        let full_reasoning = self.text_parts.join("\n\n");
        AgentLoopResult {
            final_response,
            full_reasoning,
            tool_calls_made: self.tool_calls,
            orders_attempted: self.orders,
            iterations,
            conversation,
        }
    }
}

/// Build an [`OrderAttempt`] from an order-tool call, pulling common fields out
/// of the arguments in a schema-tolerant way.
pub fn order_attempt_from(
    tool: &str,
    args: &Value,
    outcome: &str,
    detail: Option<String>,
) -> OrderAttempt {
    let symbol = first_str(args, &["symbol", "ticker", "instrument"]);
    let side = first_str(args, &["side", "action", "direction"]).or_else(|| {
        let t = tool.to_lowercase();
        if t.contains("buy") {
            Some("buy".to_string())
        } else if t.contains("sell") {
            Some("sell".to_string())
        } else {
            None
        }
    });
    let quantity = first_f64(args, &["quantity", "qty", "shares", "amount"]);
    OrderAttempt {
        tool: tool.to_string(),
        symbol,
        side,
        quantity,
        arguments: args.clone(),
        outcome: outcome.to_string(),
        detail,
    }
}

/// Return the first present string-valued key (case-insensitive on value type).
fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = value.get(*k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

/// Return the first present numeric key, tolerating numbers encoded as strings.
fn first_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        match value.get(*k) {
            Some(Value::Number(n)) => return n.as_f64(),
            Some(Value::String(s)) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn order_attempt_extracts_fields() {
        let args = json!({ "symbol": "AAPL", "quantity": 2.5 });
        let oa = order_attempt_from("place_order", &args, "placed", None);
        assert_eq!(oa.symbol.as_deref(), Some("AAPL"));
        assert_eq!(oa.quantity, Some(2.5));
    }

    #[test]
    fn order_attempt_infers_side_from_tool_name() {
        let args = json!({ "ticker": "MSFT", "shares": "3" });
        let oa = order_attempt_from("buy_stock", &args, "simulated", None);
        assert_eq!(oa.side.as_deref(), Some("buy"));
        assert_eq!(oa.symbol.as_deref(), Some("MSFT"));
        assert_eq!(oa.quantity, Some(3.0));
    }
}
