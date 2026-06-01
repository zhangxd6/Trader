//! [`ResearchExecutor`] — a [`ToolExecutor`] middleware that handles research
//! tools (`web_search`, `get_stock_news`) locally and forwards everything else
//! to an inner executor.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::error::Result;
use crate::llm::ToolExecutor;
use super::{news, search};

/// A [`ToolExecutor`] that intercepts research tool calls and satisfies them
/// locally (via HTTP to Yahoo Finance / Brave / DuckDuckGo), forwarding all
/// other tool calls to the wrapped `inner` executor.
pub struct ResearchExecutor {
    inner: Arc<dyn ToolExecutor>,
    brave_api_key: Option<String>,
    client: Client,
}

impl ResearchExecutor {
    /// Create a new `ResearchExecutor`.
    ///
    /// - `inner` receives all non-research tool calls.
    /// - `brave_api_key` enables the Brave Search backend; when `None` the
    ///   executor falls back to DuckDuckGo Instant Answer.
    pub fn new(inner: Arc<dyn ToolExecutor>, brave_api_key: Option<String>) -> Self {
        Self {
            inner,
            brave_api_key,
            client: Client::builder()
                .user_agent("trader-bot/1.0")
                .build()
                .expect("failed to build research HTTP client"),
        }
    }
}

#[async_trait]
impl ToolExecutor for ResearchExecutor {
    async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "get_stock_news" => {
                let symbol = args
                    .get("symbol")
                    .and_then(Value::as_str)
                    .unwrap_or("UNKNOWN");
                let max_items = args
                    .get("max_items")
                    .and_then(Value::as_u64)
                    .unwrap_or(8) as usize;
                news::get_stock_news(&self.client, symbol, max_items).await
            }
            "web_search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let count = args
                    .get("max_results")
                    .and_then(Value::as_u64)
                    .unwrap_or(5) as usize;

                if let Some(key) = &self.brave_api_key {
                    search::brave_search(&self.client, key, query, count).await
                } else {
                    search::ddg_search(&self.client, query).await
                }
            }
            other => self.inner.execute(other, args).await,
        }
    }

    fn is_order_tool(&self, name: &str) -> bool {
        self.inner.is_order_tool(name)
    }
}
