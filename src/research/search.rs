//! Web search backends: Brave Search API and DuckDuckGo Instant Answer fallback.

use reqwest::Client;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const DDG_API_URL: &str = "https://api.duckduckgo.com/";

/// Search the web using the Brave Search API.
///
/// Requires a valid `api_key` (Brave subscription token). Returns a JSON array
/// of results, each with `title`, `description`, `url`, and `age` fields.
pub async fn brave_search(
    client: &Client,
    api_key: &str,
    query: &str,
    count: usize,
) -> Result<Value> {
    let response = client
        .get(BRAVE_SEARCH_URL)
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .query(&[("q", query), ("count", &count.to_string())])
        .send()
        .await
        .map_err(|e| TraderError::Http(format!("Brave search request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(TraderError::Http(format!(
            "Brave Search returned HTTP {}",
            response.status()
        )));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| TraderError::Http(format!("parsing Brave Search response: {e}")))?;

    let results: Vec<Value> = data
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|item| {
                    json!({
                        "title": item.get("title").and_then(Value::as_str).unwrap_or(""),
                        "description": item.get("description").and_then(Value::as_str).unwrap_or(""),
                        "url": item.get("url").and_then(Value::as_str).unwrap_or(""),
                        "age": item.get("age").and_then(Value::as_str).unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "backend": "brave",
        "query": query,
        "results": results,
    }))
}

/// Search using the DuckDuckGo Instant Answer API (no API key required).
///
/// Returns `AbstractText`, `AbstractSource`, and up to 5 `RelatedTopics`.
pub async fn ddg_search(client: &Client, query: &str) -> Result<Value> {
    let response = client
        .get(DDG_API_URL)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .await
        .map_err(|e| TraderError::Http(format!("DuckDuckGo request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(TraderError::Http(format!(
            "DuckDuckGo API returned HTTP {}",
            response.status()
        )));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| TraderError::Http(format!("parsing DuckDuckGo response: {e}")))?;

    let abstract_text = data
        .get("AbstractText")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let abstract_source = data
        .get("AbstractSource")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let related: Vec<Value> = data
        .get("RelatedTopics")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .take(5)
                .filter_map(|item| {
                    let text = item.get("Text").and_then(Value::as_str)?;
                    if text.is_empty() {
                        return None;
                    }
                    Some(json!({ "text": text }))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "backend": "duckduckgo",
        "query": query,
        "abstract_text": abstract_text,
        "abstract_source": abstract_source,
        "related_topics": related,
    }))
}
