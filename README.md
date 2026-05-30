# Trader

An LLM-driven trading agent for **Robinhood's agentic trading** platform.

`trader` connects to the official [Robinhood Agentic Trading MCP](https://robinhood.com/us/en/agentic-trading/)
server, hands its tools to an LLM, and lets the model analyse your portfolio and
execute a trading strategy you define — while a Rust safety layer enforces hard
risk limits on every order the model tries to place.

> ⚠️ **Trading involves real financial risk.** This software is provided as-is
> with no warranty. Always start in **simulation** or **dry-run** mode. You are
> solely responsible for any trades placed through your account. Not affiliated
> with Robinhood Markets, Inc.

## How it works

```
scheduler tick ──> TradingAgent
                     │  build system prompt from your strategy
                     ▼
                   LlmProvider.run_agent_loop()   (OpenAI / Anthropic / compatible)
                     │  the LLM calls Robinhood MCP tools to read data & place orders
                     ▼
                   SafetyValidator   (enforces hard limits; blocks dry-run)
                     │  ├─ read tools  ─> forwarded, results observed
                     │  └─ order tools ─> validated, then forwarded / blocked
                     ▼
                   Robinhood MCP  (live)   or   SimulationExecutor (paper)
                     │
                     ▼
                   AuditLogger  (logs/audit.jsonl — every tool call + reasoning)
```

The agent is an **MCP bridge**: it does not re-implement the Robinhood API. The
LLM drives the tools; we sit in the middle and enforce safety.

## Strategy: hybrid structured + free-text

Strategies live in a YAML file (`config/strategy.yaml`). They combine:

- **Structured thresholds** enforced by Rust regardless of what the LLM decides
  (stop-loss, take-profit, position caps, per-trade caps, buy filters,
  confidence floor, cash reserve).
- **Free-text judgment rules** passed verbatim to the LLM for the calls that
  need reasoning.

See [`config/strategy.example.yaml`](config/strategy.example.yaml) for a fully
documented example.

## LLM providers

Any of four `provider:` values in the config — the same agent loop runs for all:

| provider               | endpoint                | example models                   |
| ---------------------- | ----------------------- | -------------------------------- |
| `openai`               | api.openai.com          | `gpt-4o`                         |
| `anthropic`            | api.anthropic.com       | `claude-sonnet-4-6`              |
| `openai-compatible`    | any (set `base_url`)    | Groq, Together, Azure, Ollama, … |
| `anthropic-compatible` | any (set `base_url`)    | Claude proxies / gateways        |

## Setup

1. Create a **Robinhood Agent Account** and obtain its MCP OAuth token.
2. Configure credentials and strategy:
   ```sh
   cp .env.example .env                       # fill in tokens/keys
   cp config/strategy.example.yaml config/strategy.yaml
   ```
3. Build:
   ```sh
   cargo build --release
   ```

## Usage

```sh
# Verify the MCP connection and list available tools
trader auth
trader tools

# Inspect account / market data
trader portfolio
trader quotes

# Paper-trade with REAL market data against a virtual portfolio
trader simulate --reset          # start fresh at the configured cash balance
trader simulate --once           # run a single cycle
trader simulate --status         # show virtual P&L
trader simulate --tui            # live dashboard

# Live / dry-run (dry_run defaults to true in the config)
trader once                      # one cycle
trader run                       # scheduled loop, headless
trader run --tui                 # scheduled loop with the dashboard
trader once --dry-run            # force dry-run regardless of config
```

### Recommended workflow

1. `trader simulate` over several sessions; review `logs/audit.jsonl`.
2. Tune `config/strategy.yaml` until the simulated results look right.
3. Run live in dry-run (`dry_run: true`) to confirm the LLM's intended orders.
4. Set `dry_run: false` and start with conservative caps.

## TUI dashboard

`--tui` shows three live panels:

- **Strategy** — name, mode, and the active hard/judgment rules.
- **Portfolio** — cash, equity, total value, return, and per-position P&L.
- **Logs** — the last 20 events, colour-coded (orders, safety, LLM, MCP…).

Keys: `q`/`Esc` quit, `p` pause.

## Safety model

Two independent layers protect you:

1. **Robinhood's** structural isolation — the agent account is separate from your
   main portfolio and limited to its pre-loaded balance.
2. **This program's** `SafetyValidator`, which on every order checks: watchlist
   membership, buy/sell permissions, daily trade cap, per-trade USD cap, position
   concentration, and minimum cash reserve — and blocks all orders entirely while
   `dry_run` is true.

Every cycle is written to `logs/audit.jsonl` as one JSON line including the LLM's
reasoning, each tool call, and each order outcome.

## Development

```sh
cargo test            # unit tests
cargo clippy          # lints
cargo build --release
```

Module layout:

| path              | responsibility                                   |
| ----------------- | ------------------------------------------------ |
| `src/mcp/`        | MCP client (Streamable HTTP, JSON-RPC + SSE)     |
| `src/llm/`        | provider-agnostic agent loop; OpenAI & Anthropic |
| `src/safety/`     | risk enforcement (`ToolExecutor`)                |
| `src/simulation/` | paper trading with a persisted virtual portfolio |
| `src/agent/`      | cycle orchestration + prompt building            |
| `src/scheduler/`  | interval loop + market-hours guard               |
| `src/tui/`        | ratatui dashboard                                |
| `src/audit/`      | JSONL audit trail                                |

## License

Apache-2.0 — see [LICENSE](LICENSE).
