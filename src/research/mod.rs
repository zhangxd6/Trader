//! Research capability: web search and news fetching tools for pre-trade
//! analysis, injected into the executor chain between the SafetyValidator
//! and the inner (live/simulation) executor.

mod executor;
mod news;
mod search;

pub use executor::ResearchExecutor;

use crate::mcp::McpTool;

/// Return the research-specific tools to advertise to the LLM.
pub fn research_tools() -> Vec<McpTool> {
    vec![
        McpTool {
            name: "web_search".to_string(),
            description: "Search the web for recent news, analyst reports, and market analysis. \
                Use this before trading to understand recent developments."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "default": 5 }
                },
                "required": ["query"]
            }),
        },
        McpTool {
            name: "get_stock_news".to_string(),
            description: "Fetch recent news headlines for a specific stock symbol from Yahoo \
                Finance RSS."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "max_items": { "type": "integer", "default": 8 }
                },
                "required": ["symbol"]
            }),
        },
    ]
}
