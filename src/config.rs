//! Application configuration loaded from a YAML strategy file.
//!
//! Secrets (API tokens/keys) may be provided directly or via `${ENV_VAR}`
//! placeholders that are expanded from the process environment at load time.

use serde::Deserialize;
use std::path::Path;

use crate::error::{Result, TraderError};

/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub robinhood: RobinhoodConfig,
    pub llm: LlmConfig,
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub simulation: SimulationConfig,
}

/// Robinhood agentic-trading MCP connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct RobinhoodConfig {
    /// MCP endpoint, e.g. `https://agent.robinhood.com/mcp/trading`.
    #[serde(default = "default_mcp_url")]
    pub mcp_url: String,
    /// OAuth bearer token for the dedicated agent account.
    pub api_token: String,
}

fn default_mcp_url() -> String {
    "https://agent.robinhood.com/mcp/trading".to_string()
}

/// LLM provider configuration. The `provider` tag selects the variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum LlmConfig {
    /// Native OpenAI (`api.openai.com`).
    Openai {
        api_key: String,
        model: String,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    /// Native Anthropic (`api.anthropic.com`).
    Anthropic {
        api_key: String,
        model: String,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    /// Any OpenAI-compatible endpoint (Ollama, Groq, Together, Azure, ...).
    OpenaiCompatible {
        api_key: String,
        model: String,
        base_url: String,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
    /// Any Anthropic-compatible endpoint (Claude proxies / gateways).
    AnthropicCompatible {
        api_key: String,
        model: String,
        base_url: String,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
}

fn default_temperature() -> f32 {
    0.2
}

fn default_max_tokens() -> u32 {
    2048
}

/// Trading strategy: hybrid structured thresholds + free-text judgment rules.
#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Explicit symbol allow-list. Empty means the LLM may choose any symbol.
    #[serde(default)]
    pub watchlist: Vec<String>,
    /// Industry / sector focus (e.g. "AI", "energy", "semiconductors").
    /// The LLM is instructed to use MCP discovery tools to find candidates
    /// within these sectors. May be combined with or used instead of `watchlist`.
    #[serde(default)]
    pub industries: Vec<String>,
    pub structured: StructuredRules,
    #[serde(default)]
    pub rules: Vec<String>,
}

/// Rust-enforced numeric thresholds.
#[derive(Debug, Clone, Deserialize)]
pub struct StructuredRules {
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub max_positions: usize,
    #[serde(default)]
    pub min_confidence: f64,
    #[serde(default)]
    pub buy_filters: BuyFilters,
}

/// Optional filters applied before a BUY is permitted.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BuyFilters {
    pub max_pe_ratio: Option<f64>,
    pub max_price_vs_52w_high_pct: Option<f64>,
    pub min_volume_ratio: Option<f64>,
}

/// Hard risk limits enforced regardless of the LLM's choices.
#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    /// When true, orders are simulated/blocked rather than placed live.
    #[serde(default = "default_true")]
    pub dry_run: bool,
    pub max_trade_usd: f64,
    #[serde(default = "default_max_position_pct")]
    pub max_position_pct: f64,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
    #[serde(default)]
    pub min_cash_reserve_pct: f64,
    #[serde(default = "default_true")]
    pub allow_buys: bool,
    #[serde(default = "default_true")]
    pub allow_sells: bool,
}

fn default_true() -> bool {
    true
}

fn default_max_position_pct() -> f64 {
    0.10
}

fn default_max_daily_trades() -> u32 {
    20
}

/// Scheduling settings for the periodic trading loop.
#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_interval_minutes(),
        }
    }
}

fn default_interval_minutes() -> u64 {
    15
}

/// Audit log destination.
#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    #[serde(default = "default_log_file")]
    pub log_file: String,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            log_dir: default_log_dir(),
            log_file: default_log_file(),
        }
    }
}

fn default_log_dir() -> String {
    "./logs".to_string()
}

fn default_log_file() -> String {
    "audit.jsonl".to_string()
}

/// Settings for paper-trading simulation mode.
#[derive(Debug, Clone, Deserialize)]
pub struct SimulationConfig {
    /// Starting virtual cash balance.
    #[serde(default = "default_starting_cash")]
    pub starting_cash: f64,
    /// Path to the persisted virtual portfolio.
    #[serde(default = "default_sim_path")]
    pub portfolio_path: String,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            starting_cash: default_starting_cash(),
            portfolio_path: default_sim_path(),
        }
    }
}

fn default_starting_cash() -> f64 {
    10_000.0
}

fn default_sim_path() -> String {
    "./simulation/portfolio.json".to_string()
}

/// If the YAML has a top-level `strategy:` key (singular) but no `strategies:`
/// key, wrap the single mapping in a one-element sequence under `strategies:`.
/// This provides backward compatibility with single-strategy config files.
fn normalize_strategies(value: &mut serde_yaml::Value) {
    if let serde_yaml::Value::Mapping(map) = value {
        let has_strategies = map.contains_key(serde_yaml::Value::String("strategies".into()));
        let has_strategy = map.contains_key(serde_yaml::Value::String("strategy".into()));
        if has_strategy && !has_strategies {
            if let Some(single) = map.remove(serde_yaml::Value::String("strategy".into())) {
                map.insert(
                    serde_yaml::Value::String("strategies".into()),
                    serde_yaml::Value::Sequence(vec![single]),
                );
            }
        }
    }
}

impl AppConfig {
    /// Load and parse configuration from a YAML file, expanding `${ENV}`
    /// placeholders inside string *values* against the process environment.
    ///
    /// Expansion happens after YAML parsing so that placeholders appearing in
    /// comments are ignored.
    ///
    /// For backward compatibility, a top-level `strategy:` (singular) key is
    /// automatically promoted to a one-element `strategies:` list.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| TraderError::Config(format!("reading {}: {e}", path.display())))?;
        let mut value: serde_yaml::Value = serde_yaml::from_str(&raw)
            .map_err(|e| TraderError::Config(format!("parsing {}: {e}", path.display())))?;
        normalize_strategies(&mut value);
        expand_env_value(&mut value)?;
        let config: AppConfig = serde_yaml::from_value(value)
            .map_err(|e| TraderError::Config(format!("parsing {}: {e}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    /// Sanity-check values that serde alone cannot enforce.
    fn validate(&self) -> Result<()> {
        if self.strategies.is_empty() {
            return Err(TraderError::Config(
                "at least one strategy must be defined under `strategies:`".into(),
            ));
        }
        if self.risk.max_trade_usd <= 0.0 {
            return Err(TraderError::Config(
                "risk.max_trade_usd must be positive".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.risk.max_position_pct) {
            return Err(TraderError::Config(
                "risk.max_position_pct must be between 0 and 1".into(),
            ));
        }
        Ok(())
    }

    /// Normalised, uppercase watchlist symbols aggregated across all strategies.
    pub fn watchlist_upper(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.strategies
            .iter()
            .flat_map(|s| s.watchlist.iter())
            .map(|s| s.trim().to_uppercase())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    }
}

/// Recursively expand `${VAR}` placeholders inside every string value of a
/// parsed YAML document.
fn expand_env_value(value: &mut serde_yaml::Value) -> Result<()> {
    match value {
        serde_yaml::Value::String(s) => {
            if s.contains("${") {
                *s = expand_env(s)?;
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                expand_env_value(v)?;
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (_, v) in map.iter_mut() {
                expand_env_value(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Expand `${VAR}` placeholders using environment variables.
///
/// An unset variable is an error so misconfiguration fails loudly rather than
/// silently sending an empty credential.
fn expand_env(input: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            TraderError::Config("unterminated ${...} placeholder in config".into())
        })?;
        let var = &after[..end];
        let value = std::env::var(var).map_err(|_| {
            TraderError::Config(format!("environment variable `{var}` is not set"))
        })?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_replaces_placeholders() {
        std::env::set_var("TRADER_TEST_TOKEN", "secret123");
        let result = expand_env("token: ${TRADER_TEST_TOKEN}").unwrap();
        assert_eq!(result, "token: secret123");
    }

    #[test]
    fn expand_env_errors_on_missing_var() {
        let err = expand_env("${TRADER_DEFINITELY_UNSET_VAR_XYZ}");
        assert!(err.is_err());
    }

    #[test]
    fn expand_env_passes_through_plain_text() {
        let result = expand_env("no placeholders here").unwrap();
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn normalize_strategies_promotes_singular() {
        let yaml = r#"
strategy:
  name: Test
  watchlist: [AAPL]
  structured:
    stop_loss_pct: 5.0
    take_profit_pct: 15.0
    max_positions: 3
"#;
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        normalize_strategies(&mut value);
        let map = value.as_mapping().unwrap();
        assert!(map.contains_key(&serde_yaml::Value::String("strategies".into())));
        assert!(!map.contains_key(&serde_yaml::Value::String("strategy".into())));
        let strategies = map
            .get(&serde_yaml::Value::String("strategies".into()))
            .unwrap();
        assert_eq!(strategies.as_sequence().unwrap().len(), 1);
    }

    #[test]
    fn normalize_strategies_leaves_plural_untouched() {
        let yaml = r#"
strategies:
  - name: Test
    watchlist: [AAPL]
    structured:
      stop_loss_pct: 5.0
      take_profit_pct: 15.0
      max_positions: 3
"#;
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        normalize_strategies(&mut value);
        let map = value.as_mapping().unwrap();
        assert!(map.contains_key(&serde_yaml::Value::String("strategies".into())));
        assert!(!map.contains_key(&serde_yaml::Value::String("strategy".into())));
    }
}
