# Metaplex Agent Template — Port to ZeroClaw: Design

**Date:** 2026-04-24
**Status:** Validated (brainstorming complete)
**Source spec:** [`docs/SPEC.md`](../SPEC.md) (v1.2)
**Approach:** Port the TypeScript Metaplex Agent Template functionality to ZeroClaw's Rust trait-driven architecture. The existing Next.js test UI is retained unchanged and connects over an unchanged PlexChat wire protocol.

## 1. Crate Layout & Dependencies

- **New crate `zeroclaw-solana`** (`crates/zeroclaw-solana/`) — Solana / Metaplex foundation. Thin, no LLM or transport concerns.
  - `client.rs` — `SolanaClient` wrapping `solana_client::nonblocking::rpc_client::RpcClient`.
  - `keypair.rs` — `AGENT_KEYPAIR` decoder (base58 secret or JSON byte array, SPEC §8.1).
  - `state.rs` — atomic read/write of `agent-state.json`, mode `0600`, anchored to workspace root (walk-up for `Cargo.toml`). SPEC §4.3.
  - `context.rs` — `AgentContext { wallet_address, mode, asset_address, token_mint, token_override, fee_sol, owner_wallet, tx_sender, solana }`.
  - `submit_or_send.rs` — mode-aware tx routing (SPEC §10.2/10.3).
  - `funding.rs` — `ensure_agent_funded` seam (SPEC §10.8).
  - `owner.rs` — resolve + cache MPL Core owner; bootstrap fallback (SPEC §3.4).
  - `jupiter.rs` — quote / swap / price helpers + `simulate_and_verify_swap` (SPEC §10.4).
  - `error_codes.rs` — `ToolErrorCode` taxonomy + `to_tool_error` classifier (SPEC §5.3).
  - `tool_result.rs` — `ToolResult<T>` discriminant (`ok` / `info` / `err`) with `status` field.
- **New module `crates/zeroclaw-gateway/src/ws_plexchat.rs`** — PlexChat WS route parallel to existing `ws.rs`. Reuses `extract_ws_token`, `AppState`, rate-limit, TLS.
- **New tool folder `crates/zeroclaw-tools/src/solana/`** — 14 tool files, each a `Tool` impl (`shared/` + `public/` subfolders mirror SPEC §5.1).
- **Workspace deps:** `solana-sdk`, `solana-client`, `spl-token`, `spl-associated-token-account`, `mpl-core`, `mpl-agent-registry` (fallback: IDL-driven hand-rolled client), `mpl-genesis` (same fallback), `bs58`, `reqwest` (already present).

## 2. PlexChat Transport & Per-Session State

- Mount `/ws/plexchat` alongside existing `/ws/chat`; no change to `zeroclaw.v1` chat protocol.
- `PlexChatSession` (per WS connection) owns: `jid`, `wallet_address`, `is_owner_verified`, history (`VecDeque`, cap 50), `pending_txs` (correlationId → `oneshot::Sender<Result<Signature>>`), `pending_msgs`, rate limiter (20/10s sliding), processing flag, `CancellationToken`, `alive_check` handle, mode, `Arc<PlexChatTxSender>`.
- Wire format is a 1:1 port of SPEC §6.3 / §6.4 / Appendix A via serde-tagged enums:
  - `ClientMsg`: `message`, `wallet_connect`, `wallet_disconnect`, `tx_result`, `tx_error`.
  - `ServerMsg`: `connected`, `message`, `typing`, `transaction`, `wallet_connected`, `wallet_disconnected`, `error`, `debug:*`.
- Three-way auth (header / `Sec-WebSocket-Protocol: bearer.<token>` / query) via existing `extract_ws_token`. Token compared with `constant_time_eq`; min length 32 enforced at config load.
- **Autonomous connection gate (Layer 1):** pre-`wallet_connect` only `connected` is sent. On `wallet_connect`, server resolves owner via `zeroclaw_solana::owner::resolve_owner`; mismatch → close `4001 "Unauthorized"`. Public mode: open.
- Close codes: `4001 Unauthorized`, `4002 Too many connections`, `1001 Server shutting down`.

## 3. Agent Context Plumbing

- `Tool` trait unchanged. Plumbing uses `tokio::task_local!`:

  ```rust
  tokio::task_local! { pub(crate) static AGENT_CTX: AgentContext; }
  pub fn current_context() -> Result<AgentContext, ToolError> { … }
  ```

- The WS handler wraps each agent turn: `AGENT_CTX.scope(ctx, async { provider.run(...).await })`.
- Tools call `current_context()?` from their body.
- `TransactionSender` lives inside `AgentContext` as `Option<Arc<dyn TxSender>>`. Public mode instantiates `PlexChatTxSender { session: Weak<Session> }`; autonomous mode passes `None`, and `submit_or_send` detects absence and signs agent-side.
- `TxSender::send_and_await(tx_b64, opts)` generates UUID correlationId, inserts oneshot into session, emits `Transaction` server message, awaits oneshot with 5-min timeout. Client `TxResult` / `TxError` resolves / rejects. Late-ok handling per SPEC §6.6 — well-formed signature post-timeout logs `[late-ok]` and does not re-reject.
- **Tool auth:** `with_auth(tool, level)` wrapper checks `default_auth_policy(level, &current_context()?)` before delegating. Unknown levels fail closed. Matches SPEC §5.4.

## 4. Tool Inventory & Mode Dispatch

- Directory: `crates/zeroclaw-tools/src/solana/{shared,public}/`.
- Composers:

  ```rust
  pub fn public_agent_tools() -> Vec<Arc<dyn Tool>>;       // 14
  pub fn autonomous_agent_tools() -> Vec<Arc<dyn Tool>>;   // 12
  pub const PUBLIC_TOOL_NAMES: &[&str];
  pub const AUTONOMOUS_TOOL_NAMES: &[&str];
  ```

- Each tool = one struct impl'ing `zeroclaw_api::tool::Tool`. Uniform shape: parse args → `current_context()?` → on-chain work → `ok` / `info` / `err`.
- **Bridge to existing `api::ToolResult`:** tools build a rich `ToolResult<T>` internally, then `.into()` converts to `{ success, output, error }` — `output` is serialized JSON containing the `status` discriminant, `error` is set on `err`. Preserves `Tool` trait while giving the LLM the discriminated `status` field.
- Mode selection at WS handler startup: `AGENT_MODE` chooses `public_agent_tools()` vs `autonomous_agent_tools()`. Handed to the provider call as the tool list for that turn.

## 5. Rust Solana + Metaplex + Jupiter Stack

| TS (Umi) | Rust | Notes |
|---|---|---|
| `@solana/web3.js`, Umi | `solana-sdk`, `solana-client::nonblocking` | Tokio throughout. |
| `mpl-toolbox` (transfer SOL, create ATA, transfer SPL) | `spl-token` + `spl-associated-token-account` | Direct instruction composition. |
| `mpl-core` (fetch asset, read `owner`) | `mpl-core` (Rust crate) | Owner resolver + `register-agent`. |
| Jupiter swap/v1, price/v3 | `reqwest` + JSON | No Rust SDK; SPEC already uses HTTP. |
| `mpl-agent-registry` | Rust crate if it exists, else hand-rolled IDL client | `agent_registry_client.rs` in `zeroclaw-solana`. |
| `mpl-genesis` | Same fallback | `genesis.rs` in `zeroclaw-solana`. |

Crate availability is verified as the first step of PR 1; fallback is deterministic work — not a blocker.

- `build_and_send_agent(rpc, ixs, signers) -> Signature` — autonomous and all agent-signed paths (Jupiter, launch, register).
- `build_user_tx(ixs, user_pubkey, fee_sol) -> (tx_b64, agent_fee_ix_prepended)` — public-mode NoopSigner equivalent: unsigned `Transaction` with user as fee payer, recent blockhash fetched, optional `system_instruction::transfer(user → agent_pda, fee_sol)` prepended atomically.
- `submit_or_send(ctx, ixs, opts) -> Signature` — single tool entry; dispatches on `ctx.mode` and `ctx.tx_sender`.
- `jupiter::simulate_and_verify_swap` uses `RpcClient::simulate_transaction_with_config` with `replace_recent_blockhash: true` and `accounts` for agent input/output ATAs; asserts input-debit ≤ `amount + buffer` and output-credit ≥ `expected_min_out - buffer`; rejects on mismatch with `INTEGRITY`. Mirrors SPEC §10.4 exactly.

## 6. State, Owner Resolution, Funding, Debug Stream

- **`agent-state.json`** — workspace-root atomic write (`.tmp` + `fsync` + `rename`), mode `0600`. Resolution order: env → state file → `None`. `register-agent` writes `agent_asset_address`; `launch-token` writes `agent_token_mint`.
- **Owner cache** — process-global `Mutex<HashMap<Pubkey, CachedOwner>>` with TTL `OWNER_CACHE_TTL_MS` (default 5m). Keyed by asset address. Invalidated on `register-agent` success.
- **`ensure_agent_funded(ctx)`** — single mode-aware helper:
  - Balance ≥ `AGENT_FUNDING_THRESHOLD_SOL` → `Funded`.
  - Public mode + `tx_sender.is_some()` + `wallet_address.is_some()` → build user→agent `system_instruction::transfer(AGENT_FUNDING_SOL)`, route via `submit_or_send` with `fee_sol = 0.0`, poll balance 10×1s, return `Funded`.
  - Else → `Err(reason)`; `register-agent` turns into `INSUFFICIENT_FUNDS` with the operator-facing amount + address.
- **Debug event stream (SPEC §6.5)** — `DebugEvent` variants `StepStart`, `ToolCall`, `ToolResult`, `TextDelta`, `StepComplete`, `GenerationComplete`, `Context`. `PlexChatDebugObserver` impls `zeroclaw_api::observability_traits::Observer`, holds `Weak<Session>`, fans debug messages out over the socket. Gated by `ENABLE_DEBUG_EVENTS`. `debug:context` emitted synthetically on connect, wallet change, cleanup, post-generation.
- **Rate limit / budgets** — reuse `zeroclaw_gateway::auth_rate_limit` for 20/10s sliding window. `MAX_TOKENS_PER_MESSAGE` + `MAX_TOOL_EXECUTIONS_PER_MESSAGE` + `MAX_RPC_TIME_BUDGET_MS` enforced in the per-turn loop; exceeding aborts via `CancellationToken::cancel()` and emits `debug:generation_complete { finishReason: "budget_exceeded" }`.

## 7. Config, System Prompt, Preflight

- **Config** extends `zeroclaw-config::schema` with a `SolanaAgentConfig` sub-struct populating every SPEC §8.1 field (required + optional-with-defaults + optional-no-default). Autonomous guard: if `agent_mode == Autonomous` and both `bootstrap_wallet` and `agent_asset_address` are `None` → config validation error, refuses to start.
- **System prompt** at `crates/zeroclaw-runtime/src/agent/solana_prompts.rs`: `build_system_prompt(mode, assistant_name) -> String` returns `BASE + addendum`. `BASE_PROMPT` covers bootstrap, funding retry, identity, tool inventory, launch irreversibility, treasury, price-watch loop, portfolio analysis. `PUBLIC_ADDENDUM` / `AUTONOMOUS_ADDENDUM` describe tx-routing differences.
- **Preflight** in `ws_plexchat::start()` before router bind:
  1. `zeroclaw_solana::keypair::decode(AGENT_KEYPAIR)` — catches malformed keys.
  2. `rpc.get_slot().await` — proves RPC reachable.
  3. `RequestBodyLimitLayer::new(65_536)` + ws message-size cap.
  4. `handle_protocols` bearer echo (shared helper extracted from `ws.rs`).
- Any failure → `tracing::error!` + `process::exit(1)` with descriptive message.

## 8. Testing, Phasing, Rollout

**Testing layers**

- **Unit** — `keypair` decode variants, `state` atomic write, `owner` cache TTL, `submit_or_send` branching, `simulate_and_verify_swap` delta checks with synthetic simulation results, `classify_error`, `with_auth` allow/deny matrix, `PlexChatSession` rate-limit window, `sanitize_reason` regex.
- **Integration (devnet-gated)** — behind `#[ignore]` + `SOLANA_DEVNET_TESTS=1`: `get-balance`, `transfer-sol` round-trip, `register-agent` happy path, `jupiter::quote` against live endpoint.
- **WS protocol** — in-process gateway + `tokio-tungstenite` client: handshake auth variants, autonomous-gate `4001`, `MAX_CONNECTIONS` `4002`, `tx_result` / `tx_error` routing, timeout rejection, late-ok, rate-limit emit without close.
- **Contract** — one snapshot per `ServerMsg` / `ClientMsg` variant; SPEC Appendix A is the authoritative contract.

**Phasing (six PRs; each landable independently; `Depends on #…` chain; conventional commits; size S/M)**

| # | Title | Scope |
|---|---|---|
| 1 | `feat(solana): add zeroclaw-solana crate skeleton` | Crate + deps + `client`, `keypair`, `state`, `context`, `error_codes`, `tool_result`, `owner` with unit tests. No tools, no WS. |
| 2 | `feat(gateway): add PlexChat WS route` | `ws_plexchat.rs`, `PlexChatSession`, wire enums, auth gates, rate-limit, alive check, `MAX_CONNECTIONS`, preflight. Empty tool set. |
| 3 | `feat(tools): solana read-only tools + auth wrapper` | 6 tools: `get-balance`, `get-token-balances`, `get-transaction`, `get-token-price`, `get-token-metadata`, `sleep`. `with_auth`. Task-local context. |
| 4 | `feat(tools): solana write + identity tools` | `submit_or_send`, `funding`, `transfer-sol`, `transfer-token`, `register-agent`, `delegate-execution`. IDL fallback if no Rust `mpl-agent-registry`. |
| 5 | `feat(tools): jupiter trading tools` | `jupiter::{quote,swap,price}` + `simulate_and_verify_swap`. `swap-token`, `buyback-token`, `sell-token`. |
| 6 | `feat(tools): launch-token + debug stream` | `launch-token` (mpl-genesis or IDL) with `confirm_irreversible`. `PlexChatDebugObserver`. Token/tool per-message budgets. UI smoke test against existing Next.js `packages/ui`. |

Each PR validates with `./dev/ci.sh all`. PR 1 and 2 unblock UI protocol development in parallel.

**UI compatibility** — SPEC §6 + Appendix A are the wire contract. PR 2 lands identical shapes so the untouched Next.js UI connects against `ws://localhost:3002/ws/plexchat` (or `/` via proxy) without modification.

## Risk Register

- **`mpl-agent-registry` / `mpl-genesis` Rust availability** — mitigated by IDL-client fallback; verified first step of PR 1 and PR 6.
- **Jupiter response drift** — snapshot tests on a static quote JSON + schema validation; integrity check is the backstop.
- **High-risk surfaces** — all PRs touch `crates/zeroclaw-runtime/src/**` (context/observer), `crates/zeroclaw-gateway/src/**` (WS), and new `zeroclaw-solana` (tx signing). Per AGENTS.md, full `./dev/ci.sh all` on every PR.
