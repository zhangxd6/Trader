//! Yahoo Finance RSS news fetcher.
//!
//! Parses RSS responses using simple string extraction — no XML crate required.

use reqwest::Client;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};

const YAHOO_RSS_URL: &str = "https://finance.yahoo.com/rss/headline";

/// Fetch recent news headlines for `symbol` from the Yahoo Finance RSS feed.
///
/// Returns a JSON array of objects with `title`, `pub_date`, and `link` fields.
pub async fn get_stock_news(client: &Client, symbol: &str, max_items: usize) -> Result<Value> {
    let url = format!("{}?s={}", YAHOO_RSS_URL, symbol.to_uppercase());

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TraderError::Http(format!("fetching RSS for {symbol}: {e}")))?;

    if !response.status().is_success() {
        return Err(TraderError::Http(format!(
            "Yahoo Finance RSS returned HTTP {} for {symbol}",
            response.status()
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|e| TraderError::Http(format!("reading RSS body for {symbol}: {e}")))?;

    let items = parse_rss_items(&body, max_items);

    Ok(json!({
        "symbol": symbol.to_uppercase(),
        "items": items,
        "count": items.len()
    }))
}

/// Parse up to `max_items` news items from an RSS XML body using string extraction.
fn parse_rss_items(body: &str, max_items: usize) -> Vec<Value> {
    // Split on <item> boundaries; first element is the preamble.
    let raw_items: Vec<&str> = body.split("<item>").skip(1).collect();

    raw_items
        .into_iter()
        .take(max_items)
        .map(|item_block| {
            // Each item_block is everything after <item> up to (and including) </item>
            let end = item_block.find("</item>").unwrap_or(item_block.len());
            let item = &item_block[..end];

            let title = extract_tag(item, "title");
            let pub_date = extract_tag(item, "pubDate");
            let link = extract_link(item);

            json!({
                "title": title,
                "pub_date": pub_date,
                "link": link,
            })
        })
        .collect()
}

/// Extract the text content of the first occurrence of `<tag>…</tag>`.
///
/// Handles both plain text and CDATA sections (`<![CDATA[…]]>`).
fn extract_tag(block: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    let start = match block.find(&open) {
        Some(i) => i + open.len(),
        None => return String::new(),
    };
    let end = match block[start..].find(&close) {
        Some(i) => start + i,
        None => return String::new(),
    };
    let raw = &block[start..end];
    strip_cdata(raw.trim()).to_string()
}

/// RSS feeds sometimes embed a bare `<link>` element that is not a proper
/// paired tag but rather a self-closing URL. Try the standard paired approach
/// first, then fall back to extracting the URL between `<link>` and the next
/// `<` character.
fn extract_link(block: &str) -> String {
    // Try paired <link>…</link>
    let paired = extract_tag(block, "link");
    if !paired.is_empty() {
        return paired;
    }

    // Fallback: bare <link>URL<nextTag>
    if let Some(start) = block.find("<link>") {
        let after = &block[start + "<link>".len()..];
        if let Some(end) = after.find('<') {
            let candidate = after[..end].trim();
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    String::new()
}

/// Strip `<![CDATA[…]]>` wrappers from a string value.
fn strip_cdata(s: &str) -> &str {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("<![CDATA[") {
        if let Some(inner) = inner.strip_suffix("]]>") {
            return inner;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tag_plain() {
        let block = "<title>Apple hits all-time high</title><pubDate>Mon, 01 Jun 2026</pubDate>";
        assert_eq!(extract_tag(block, "title"), "Apple hits all-time high");
        assert_eq!(extract_tag(block, "pubDate"), "Mon, 01 Jun 2026");
    }

    #[test]
    fn extract_tag_cdata() {
        let block = "<title><![CDATA[Apple Q2 beats estimates]]></title>";
        assert_eq!(extract_tag(block, "title"), "Apple Q2 beats estimates");
    }

    #[test]
    fn extract_tag_missing() {
        assert_eq!(extract_tag("<foo>bar</foo>", "baz"), "");
    }

    #[test]
    fn parse_rss_items_respects_max() {
        let rss = "\
<rss><channel>\
<item><title>News 1</title><pubDate>Mon</pubDate><link>http://a.com</link></item>\
<item><title>News 2</title><pubDate>Tue</pubDate><link>http://b.com</link></item>\
<item><title>News 3</title><pubDate>Wed</pubDate><link>http://c.com</link></item>\
</channel></rss>";
        let items = parse_rss_items(rss, 2);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "News 1");
        assert_eq!(items[1]["title"], "News 2");
    }
}
