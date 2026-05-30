//! Provider-agnostic LLM agent loop with tool calling.

mod anthropic;
mod models;
mod openai;
mod provider;
mod util;

use std::sync::Arc;

pub use models::{OrderAttempt, ToolCallRecord};
pub use provider::{DynLlmProvider, ToolExecutor};

use crate::config::LlmConfig;

/// Build a provider from configuration, covering all four supported variants.
pub fn build_provider(config: &LlmConfig) -> DynLlmProvider {
    match config {
        LlmConfig::Openai {
            api_key,
            model,
            temperature,
            max_tokens,
        } => Arc::new(openai::OpenAiProvider::new(
            api_key,
            model,
            *temperature,
            *max_tokens,
            None,
        )),
        LlmConfig::OpenaiCompatible {
            api_key,
            model,
            base_url,
            temperature,
            max_tokens,
        } => Arc::new(openai::OpenAiProvider::new(
            api_key,
            model,
            *temperature,
            *max_tokens,
            Some(base_url),
        )),
        LlmConfig::Anthropic {
            api_key,
            model,
            temperature,
            max_tokens,
        } => Arc::new(anthropic::AnthropicProvider::new(
            api_key,
            model,
            anthropic::DEFAULT_MESSAGES_URL,
            *temperature,
            *max_tokens,
        )),
        LlmConfig::AnthropicCompatible {
            api_key,
            model,
            base_url,
            temperature,
            max_tokens,
        } => Arc::new(anthropic::AnthropicProvider::new(
            api_key,
            model,
            base_url,
            *temperature,
            *max_tokens,
        )),
    }
}
