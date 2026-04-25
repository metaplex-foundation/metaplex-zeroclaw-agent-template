# PR 2 — PlexChat WebSocket Route Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a `/ws/plexchat` route to `zeroclaw-gateway` that speaks the SPEC §6 PlexChat wire protocol — handshake auth (3 ways), `connected`/`message`/`typing`/`wallet_connect`/`wallet_disconnect`/`transaction`/`tx_result`/`tx_error`/`error` enums, per-session state (history, pending-tx correlation map, rate limit, alive check), autonomous-mode connection gate, `MAX_CONNECTIONS` cap, and preflight. **Agent invocation is stubbed** with a placeholder reply — real agent loop with tools lands in PR 3.

**Architecture:** A new module `crates/zeroclaw-gateway/src/ws_plexchat.rs` parallel to existing `ws.rs`. Reuses `extract_ws_token`, `AppState`, `GatewayRateLimiter`. Per-connection `PlexChatSession` owns all per-WS state (no shared mutable singletons). Tool dispatch + LLM streaming is intentionally deferred to PR 3.

**Tech Stack:** axum 0.x, tokio, tokio-tungstenite (already deps), serde + serde_json, uuid, `zeroclaw-solana` (workspace dep added). All inside the existing `zeroclaw-gateway` crate — no new crate.

**Risk tier:** High (gateway WS surface + auth gates).

---

## Pre-work (already done before plan)

- [x] Worktree created at `.worktrees/feat-plexchat-ws-route/` on branch `feat/plexchat-ws-route`.
- [x] Master at `0c848b0f` (PR 1 merged) — `zeroclaw-solana` available as a workspace dep.

---

## Task 1: Add `zeroclaw-solana` dep + wire enums + snapshot tests

**Files:**
- Modify: `crates/zeroclaw-gateway/Cargo.toml` (add `zeroclaw-solana.workspace = true`).
- Create: `crates/zeroclaw-gateway/src/ws_plexchat/mod.rs` (or `ws_plexchat.rs` as a single file).
- Create: `crates/zeroclaw-gateway/src/ws_plexchat/wire.rs` with `ClientMsg` + `ServerMsg` enums (serde-tagged).

**`wire.rs` content (TDD):**

```rust
//! PlexChat wire enums (SPEC §6.3, §6.4, Appendix A).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Message {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_name: Option<String>,
    },
    WalletConnect { address: String },
    WalletDisconnect,
    TxResult {
        #[serde(rename = "correlationId")]
        correlation_id: String,
        signature: String,
    },
    TxError {
        #[serde(rename = "correlationId")]
        correlation_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Connected {
        jid: String,
    },
    Message {
        content: String,
        sender: String,
    },
    Typing {
        #[serde(rename = "isTyping")]
        is_typing: bool,
    },
    Transaction {
        transaction: String,
        #[serde(rename = "correlationId")]
        correlation_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u32>,
        #[serde(rename = "feeSol", skip_serializing_if = "Option::is_none")]
        fee_sol: Option<f64>,
    },
    WalletConnected { address: String },
    WalletDisconnected,
    Error {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}
```

**Tests** (snapshot-style on JSON output to catch wire-format regressions):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_message_round_trip() {
        let v = json!({"type": "message", "content": "hi", "sender_name": "alice"});
        let m: ClientMsg = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(m, ClientMsg::Message { content: "hi".into(), sender_name: Some("alice".into()) });
        assert_eq!(serde_json::to_value(&m).unwrap(), v);
    }

    #[test]
    fn client_wallet_connect_with_address() {
        let v = json!({"type": "wallet_connect", "address": "ABC"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(m, ClientMsg::WalletConnect { address: "ABC".into() });
    }

    #[test]
    fn client_tx_result_uses_correlation_id_camel() {
        let v = json!({"type": "tx_result", "correlationId": "c1", "signature": "S"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(m, ClientMsg::TxResult { correlation_id: "c1".into(), signature: "S".into() });
    }

    #[test]
    fn client_tx_error_reason_optional() {
        let v = json!({"type": "tx_error", "correlationId": "c1"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(m, ClientMsg::TxError { correlation_id: "c1".into(), reason: None });
    }

    #[test]
    fn server_transaction_uses_camel_fields() {
        let m = ServerMsg::Transaction {
            transaction: "b64".into(),
            correlation_id: "c1".into(),
            message: Some("Buyback".into()),
            index: Some(1),
            total: Some(3),
            fee_sol: Some(0.001),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["correlationId"], "c1");
        assert_eq!(v["feeSol"], 0.001);
        assert_eq!(v["index"], 1);
        assert_eq!(v["type"], "transaction");
    }

    #[test]
    fn server_typing_camel() {
        let m = ServerMsg::Typing { is_typing: true };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["isTyping"], true);
        assert_eq!(v["type"], "typing");
    }

    #[test]
    fn server_error_omits_code_when_none() {
        let m = ServerMsg::Error { error: "x".into(), code: None };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("\"code\""));
    }

    #[test]
    fn unknown_client_type_is_rejected() {
        let v = json!({"type": "unknown_type"});
        let r: Result<ClientMsg, _> = serde_json::from_value(v);
        assert!(r.is_err());
    }
}
```

**`mod.rs` (placeholder for now):**

```rust
//! PlexChat WebSocket route — port of SPEC §6 PlexChat protocol.
//!
//! Sibling to `ws.rs`: separate route at `/ws/plexchat` with its own
//! wire format, session state, and auth gate semantics. Tool dispatch
//! and LLM streaming wire in PR 3.

pub mod wire;
```

**Validate:**

```bash
cargo check -p zeroclaw-gateway
cargo test -p zeroclaw-gateway --lib ws_plexchat
cargo clippy -p zeroclaw-gateway --all-targets -- -D warnings
cargo fmt --all -- --check
```

**Commit:**

```bash
git add crates/zeroclaw-gateway/Cargo.toml crates/zeroclaw-gateway/src/ws_plexchat/
git commit -m "feat(gateway): add PlexChat wire enums (SPEC §6.3/§6.4)

ClientMsg + ServerMsg serde-tagged enums matching SPEC Appendix A.
Snapshot tests pin wire format (camelCase fields, snake_case type
discriminator). No route wiring yet — that lands in Task 6."
```

---

## Task 2: `PlexChatSession` state struct + close codes

**Files:**
- Create: `crates/zeroclaw-gateway/src/ws_plexchat/session.rs`.
- Modify: `crates/zeroclaw-gateway/src/ws_plexchat/mod.rs` (add `pub mod session;`).

**Content:**

```rust
//! Per-WebSocket-connection session state for the PlexChat route.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use zeroclaw_solana::context::{AgentMode, TxSendError};

/// Maximum chat history retained per session (SPEC §6.7).
pub const MAX_HISTORY: usize = 50;
/// Per-message content cap (SPEC §6.2 / §8.1).
pub const MAX_MESSAGE_CONTENT_DEFAULT: usize = 8000;
/// Sliding-window rate limit: 20 client messages per 10 seconds.
pub const RATE_WINDOW: Duration = Duration::from_secs(10);
pub const RATE_LIMIT: usize = 20;
/// Aliveness check intervals.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const PONG_TIMEOUT: Duration = Duration::from_secs(60);
/// Pending-tx oneshot timeout.
pub const TX_AWAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Close codes (SPEC §6.2).
pub const CLOSE_UNAUTHORIZED: u16 = 4001;
pub const CLOSE_TOO_MANY: u16 = 4002;
pub const CLOSE_SERVER_SHUTDOWN: u16 = 1001;

#[derive(Clone, Debug)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

/// Pending oneshot for an awaited user-signed transaction.
pub struct PendingTx {
    pub responder: oneshot::Sender<Result<Signature, TxSendError>>,
    pub created_at: Instant,
}

pub struct PlexChatSession {
    pub jid: String,
    pub mode: AgentMode,
    pub wallet_address: Mutex<Option<Pubkey>>,
    pub is_owner_verified: Mutex<bool>,
    pub history: Mutex<VecDeque<ChatTurn>>,
    pub pending_txs: Mutex<HashMap<String, PendingTx>>,
    pub rate_window: Mutex<VecDeque<Instant>>,
    pub abort: CancellationToken,
}

impl PlexChatSession {
    pub fn new(mode: AgentMode) -> Arc<Self> {
        Arc::new(Self {
            jid: uuid::Uuid::new_v4().to_string(),
            mode,
            wallet_address: Mutex::new(None),
            is_owner_verified: Mutex::new(false),
            history: Mutex::new(VecDeque::with_capacity(MAX_HISTORY)),
            pending_txs: Mutex::new(HashMap::new()),
            rate_window: Mutex::new(VecDeque::with_capacity(RATE_LIMIT)),
            abort: CancellationToken::new(),
        })
    }

    /// Sliding-window check: returns false if the session is over the rate cap.
    pub fn check_rate_limit(&self) -> bool {
        let now = Instant::now();
        let mut w = self.rate_window.lock();
        let cutoff = now - RATE_WINDOW;
        while w.front().is_some_and(|t| *t < cutoff) {
            w.pop_front();
        }
        if w.len() >= RATE_LIMIT { return false; }
        w.push_back(now);
        true
    }

    /// Append a turn, dropping the oldest if at MAX_HISTORY.
    pub fn push_history(&self, role: ChatRole, content: String) {
        let mut h = self.history.lock();
        if h.len() == MAX_HISTORY { h.pop_front(); }
        h.push_back(ChatTurn { role, content });
    }

    /// Reject all pending transactions with `Disconnected`. Called on cleanup.
    pub fn reject_all_pending(&self) {
        let mut g = self.pending_txs.lock();
        let drained: Vec<_> = g.drain().collect();
        drop(g);
        for (_id, p) in drained {
            let _ = p.responder.send(Err(TxSendError::Disconnected));
        }
    }
}
```

**Tests** (focus on the rate-limit window + history cap):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_starts_with_clean_state() {
        let s = PlexChatSession::new(AgentMode::Public);
        assert!(s.wallet_address.lock().is_none());
        assert!(!*s.is_owner_verified.lock());
        assert!(s.history.lock().is_empty());
        assert!(s.pending_txs.lock().is_empty());
        assert!(!s.jid.is_empty());
    }

    #[test]
    fn rate_limit_allows_up_to_cap() {
        let s = PlexChatSession::new(AgentMode::Public);
        for _ in 0..RATE_LIMIT {
            assert!(s.check_rate_limit());
        }
        // 21st call within the window → reject
        assert!(!s.check_rate_limit());
    }

    #[test]
    fn history_caps_at_max() {
        let s = PlexChatSession::new(AgentMode::Public);
        for i in 0..(MAX_HISTORY + 5) {
            s.push_history(ChatRole::User, format!("m{i}"));
        }
        let h = s.history.lock();
        assert_eq!(h.len(), MAX_HISTORY);
        assert_eq!(h.front().unwrap().content, format!("m{}", 5));
        assert_eq!(h.back().unwrap().content, format!("m{}", MAX_HISTORY + 4));
    }

    #[tokio::test]
    async fn reject_all_pending_drains_map() {
        let s = PlexChatSession::new(AgentMode::Public);
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        s.pending_txs.lock().insert("a".into(), PendingTx { responder: tx1, created_at: Instant::now() });
        s.pending_txs.lock().insert("b".into(), PendingTx { responder: tx2, created_at: Instant::now() });
        s.reject_all_pending();
        assert!(matches!(rx1.await.unwrap(), Err(TxSendError::Disconnected)));
        assert!(matches!(rx2.await.unwrap(), Err(TxSendError::Disconnected)));
        assert!(s.pending_txs.lock().is_empty());
    }
}
```

**Validate:** `cargo test -p zeroclaw-gateway --lib ws_plexchat` (count 8 + 4 = 12 tests).

**Commit:** `feat(gateway): add PlexChatSession + close codes`.

---

## Task 3: Connection handler — handshake, auth, MAX_CONNECTIONS

**Files:**
- Create: `crates/zeroclaw-gateway/src/ws_plexchat/handler.rs`.
- Modify: `mod.rs` (add `pub mod handler;`).

**What to implement:**

- `pub async fn handle_ws_plexchat(state, query, headers, ws) -> impl IntoResponse` — token gate via `extract_ws_token` (factor it out of `ws.rs` into a shared `auth.rs` if needed; for PR 2 it's fine to duplicate behind a `#[allow(dead_code)]` if extraction is risky).
- Echo `bearer` subprotocol if present (matches SPEC §6.2).
- Reject with 401 if token missing/wrong (use `constant_time_eq` for comparison).
- Origin check using existing `state.config.security` allowed-origins or a new field — for PR 2 just allow all and log a TODO.
- After upgrade, generate `jid`, send `ServerMsg::Connected { jid }`, install `PlexChatSession`.
- `MAX_CONNECTIONS` cap (read from config; default 10): track active count in an `AtomicUsize` on `AppState` (extend `AppState` with `plexchat_active: Arc<AtomicUsize>`); over the cap → close `4002`.
- Aliveness: tokio `interval(PING_INTERVAL)` ping; if no pong in `PONG_TIMEOUT`, terminate.
- On close, `session.abort.cancel()` + `session.reject_all_pending()`.

**Skip for now (PR 3+):** message/tx_result/tx_error handling (Task 4 covers handler dispatch).

**Tests:** Integration smoke test using `tokio-tungstenite` + axum `TestServer` (or `axum_test::TestServer`) — open a connection with valid bearer token, expect `connected` message, close.

**Validate + commit:** `feat(gateway): add PlexChat connection handshake + handler scaffold`.

---

## Task 4: Message dispatch — wallet_connect/disconnect, tx_result/tx_error, message stub

**Files:**
- Modify: `handler.rs` (add the receive loop's match on `ClientMsg`).

**Behaviour:**

- On `Message { content }`:
  - If `content.len() > MAX_MESSAGE_CONTENT_DEFAULT` → `Error { code: "MESSAGE_TOO_LONG", error: ... }`, do not append to history.
  - If rate-limited → `Error { code: "RATE_LIMIT" }`, no close.
  - Else: `push_history(User, sanitize(content))`. Send `Typing { is_typing: true }`. **Stub agent reply:** send `ServerMsg::Message { content: format!("[{} stub] echo: {}", assistant_name, sanitized), sender: assistant_name }`. Send `Typing { is_typing: false }`. Push to history as Assistant. Real agent loop wires in PR 3.
  - Sanitization: strip `\r\n[]` from content before adding to history (SPEC §6.2 prompt-injection mitigation surface).
- On `WalletConnect { address }`:
  - Parse `Pubkey::from_str(&address)`. On failure → `Error { code: "INVALID_WALLET" }`.
  - In autonomous mode: look up owner via `zeroclaw_solana::owner::resolve_owner` (use process-global `OwnerCache` on `AppState`). If owner != supplied address → close `4001`. Else mark `is_owner_verified = true`.
  - Public mode: just store the address.
  - Send `WalletConnected { address }`.
- On `WalletDisconnect`: clear wallet, mark `is_owner_verified = false`. Send `WalletDisconnected`.
- On `TxResult { correlation_id, signature }`:
  - Validate `signature` matches `BASE58_SIGNATURE_RE` (52–88 base58 chars).
  - Look up `pending_txs[correlation_id]`. If present → `responder.send(Ok(sig))`. Remove entry.
  - If absent: SPEC §6.6 late-ok handling — if signature is well-formed, log `[late-ok]` at info level and silently drop. If malformed → `Error { code: "UNKNOWN_CORRELATION" }`.
  - In autonomous mode: any `tx_result` from client → `Error { code: "INVALID_MODE" }`.
- On `TxError { correlation_id, reason }`:
  - Sanitize `reason` against `SAFE_REASON_RE` (`[\w\s.,:;'"()?!\-]{0,200}`); replace with `"User rejected or wallet error"` if missing/malformed.
  - Look up pending; `responder.send(Err(TxSendError::Rejected(sanitized)))`.
  - Unknown correlation → `Error { code: "UNKNOWN_CORRELATION" }`.
  - In autonomous mode: → `Error { code: "INVALID_MODE" }`.

**Tests:** unit tests for `sanitize_content`, `sanitize_reason`, signature regex match. One integration test that opens a connection, sends a `wallet_connect`, expects `wallet_connected`.

**Validate + commit:** `feat(gateway): wire PlexChat client message dispatch`.

---

## Task 5: `PlexChatTxSender` (impl `zeroclaw_solana::TxSender`) + correlation map

**Files:**
- Create: `crates/zeroclaw-gateway/src/ws_plexchat/tx_sender.rs`.
- Modify: `mod.rs` (add `pub mod tx_sender;`).

**Behaviour:**

```rust
pub struct PlexChatTxSender {
    session: Weak<PlexChatSession>,
    outbound: tokio::sync::mpsc::Sender<ServerMsg>,
}

#[async_trait]
impl TxSender for PlexChatTxSender {
    async fn send_and_await(&self, tx_b64: String, opts: TxSendOpts)
        -> Result<Signature, TxSendError>
    {
        let session = self.session.upgrade().ok_or(TxSendError::Disconnected)?;
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        session.pending_txs.lock().insert(
            correlation_id.clone(),
            PendingTx { responder: tx, created_at: Instant::now() },
        );
        // emit Transaction server message
        let msg = ServerMsg::Transaction {
            transaction: tx_b64,
            correlation_id: correlation_id.clone(),
            message: opts.message,
            index: opts.index,
            total: opts.total,
            fee_sol: opts.fee_sol,
        };
        if self.outbound.send(msg).await.is_err() {
            session.pending_txs.lock().remove(&correlation_id);
            return Err(TxSendError::Disconnected);
        }
        // await with timeout
        match tokio::time::timeout(TX_AWAIT_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(TxSendError::Disconnected),
            Err(_) => {
                session.pending_txs.lock().remove(&correlation_id);
                Err(TxSendError::Timeout)
            }
        }
    }
}
```

**No real callers in PR 2** — tools using `submit_or_send` come in PR 3. Add a unit test that constructs a `PlexChatTxSender` with a fake outbound channel + session, calls `send_and_await`, drives the pending map manually, and verifies success and timeout paths.

**Validate + commit:** `feat(gateway): impl PlexChatTxSender for transaction prompt path`.

---

## Task 6: Mount route + preflight + final integration test

**Files:**
- Modify: `crates/zeroclaw-gateway/src/lib.rs`:
  - Add `pub mod ws_plexchat;` near the existing `pub mod ws;`.
  - Add `.route("/ws/plexchat", get(ws_plexchat::handler::handle_ws_plexchat))` next to the `/ws/chat` route.
  - Extend `AppState` with `plexchat_active: Arc<AtomicUsize>` and (if needed) `solana_client: Arc<zeroclaw_solana::client::SolanaClient>` + `owner_cache: Arc<zeroclaw_solana::owner::OwnerCache>`.
  - Optional: under a feature flag `plexchat`, only enable the route when `AGENT_KEYPAIR` + `WEB_CHANNEL_TOKEN` are set in config. For PR 2 wire it unconditionally (it can sit unused if the env isn't configured).
- Add a config schema entry for `plexchat` in `crates/zeroclaw-config/src/schema.rs` (just the bare minimum: `agent_mode`, `agent_keypair`, `web_channel_token`, `solana_rpc_url`, `bootstrap_wallet`, `agent_asset_address`). The full SolanaAgentConfig (SPEC §8.1) lands in PR 3.
- Preflight: in lib.rs's startup path, when `plexchat` config is set, run `keypair::decode(agent_keypair)` and `solana_client.preflight()` before binding. Failure → `process::exit(1)` with descriptive message.

**Integration tests:** spawn axum server in-process, open a `tokio-tungstenite` client, exercise:
- Bearer subprotocol auth → `connected`.
- `wallet_connect` (public mode) → `wallet_connected`.
- `message` → typing-true → stub-reply → typing-false.
- Send `tx_result` for an unknown correlation_id → `Error { code: "UNKNOWN_CORRELATION" }`.
- `MAX_CONNECTIONS` cap → 4002 close.
- Wrong token → 401 (no upgrade).

**Validate:**

```bash
cargo fmt --all -- --check
cargo clippy -p zeroclaw-gateway --all-targets -- -D warnings
cargo test -p zeroclaw-gateway
cargo check --workspace
```

**Commit:** `feat(gateway): mount /ws/plexchat route + preflight`.

---

## Task 7: Final CI gate + PR readiness

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
git log --oneline master..HEAD
```

Stop before any push. Master should remain clean.

---

## Append-only notes

(filled during execution)

### Task 6 (2026-04-24)

- `PLEXCHAT_ACTIVE` moved off the file-static onto `AppState` as
  `pub plexchat_active: Arc<AtomicUsize>`. Production wiring constructs
  it fresh in `run_gateway`; tests construct one per `AppState`. The
  `connected_emitted_on_accept` test was updated to assert the per-state
  gauge moves from 0 → 1 → 0 instead of asserting against a process-global.
- `PlexChatConfig` added with `Option<String>` fields only. Serde uses
  the schema-default snake_case (no `rename_all` override) — most peer
  sub-structs in `schema.rs` follow that convention; the plan's
  kebab-case suggestion is reserved for the prefix on `Configurable`
  (`#[prefix = "plexchat"]`). The two secret fields (`agent_keypair`,
  `web_channel_token`) carry `#[secret]` so they encrypt-at-rest and are
  masked by `every_prop_is_gettable_and_settable`.
- The pre-existing `every_prop_is_gettable_and_settable` failure on
  `autonomy.allowed-commands` reproduces on master and is unrelated to
  the PR 2 schema changes — `565 passed; 1 failed` both before and after
  this task. No new fields hit the round-trip path because all PlexChat
  fields are `Option<String>` and default to `<unset>`, which the test
  skips.
- Preflight: SPEC §6.8 lives in `plexchat_preflight()` in
  `zeroclaw-gateway/src/lib.rs`. It is a no-op until both
  `web_channel_token` and `agent_keypair` are populated. The token
  length floor is 32 chars; the keypair is parsed via
  `zeroclaw_solana::keypair::decode`; the RPC is pinged via
  `SolanaClient::preflight()` (`get_slot`). Failure returns an
  `anyhow::Error` so the gateway's existing `Result<()>`-returning
  startup propagates it — we did not introduce `process::exit`.
- `solana_rpc_url` defaults to mainnet-beta when unset (with a
  `tracing::warn!`) so operators can experiment with a token + keypair
  without picking an RPC explicitly. This is a soft default; PR 3 may
  promote it to a required field once the rest of `SolanaAgentConfig`
  lands.
- Integration tests added: `wrong_token_returns_401` (uses `reqwest`
  for a bare HTTP upgrade so the 401 is observable — tungstenite
  collapses non-101s into a generic error) and
  `max_connections_returns_4002` (saturates the per-state gauge then
  verifies the surplus connection sees a `Close(4002)` frame).
  `unknown_correlation_tx_result_emits_error` was already covered in
  Task 4 as `tx_result_unknown_correlation_emits_error`.
- `reqwest` added as a dev-dependency in `zeroclaw-gateway/Cargo.toml`
  (version pinned to match the workspace root manifest's `0.12`,
  `default-features = false` + `rustls-tls` so the dev-dep doesn't drag
  in OpenSSL). The workspace root does not declare `reqwest` under
  `[workspace.dependencies]`, so a direct version is required.
