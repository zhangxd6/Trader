//! Domain error types for the trading agent.

use thiserror::Error;

/// All errors the trading agent can produce.
#[derive(Debug, Error)]
pub enum TraderError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("MCP protocol error: {0}")]
    Mcp(String),

    #[error("MCP server returned error {code}: {message}")]
    McpServer { code: i64, message: String },

    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("LLM provider error: {0}")]
    Llm(String),

    #[error("LLM response could not be parsed: {0}")]
    LlmParse(String),

    #[error("order rejected by safety validator: {0}")]
    SafetyRejection(String),

    #[error("serialization error: {0}")]
    Serialize(String),

    #[error("simulation error: {0}")]
    Simulation(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for TraderError {
    fn from(e: reqwest::Error) -> Self {
        TraderError::Http(e.to_string())
    }
}

impl From<serde_json::Error> for TraderError {
    fn from(e: serde_json::Error) -> Self {
        TraderError::Serialize(e.to_string())
    }
}

/// Convenient result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, TraderError>;
