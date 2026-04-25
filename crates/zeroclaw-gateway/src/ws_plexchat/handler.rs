//! PlexChat WebSocket connection handler (SPEC §6).
//!
//! Responsibilities (PR 2 Tasks 3 + 4):
//!
//! - Authenticate the upgrade request via `Authorization: Bearer …`,
//!   `Sec-WebSocket-Protocol: bearer, <token>`, or `?token=…`. Reject with
//!   HTTP 401 (no upgrade) when the token is missing or unrecognised.
//! - Echo the SPEC §6.2 `bearer` subprotocol on accept when the client
//!   advertised it.
//! - Enforce a per-`AppState` `MAX_CONNECTIONS = 10` cap by tracking the
//!   `state.plexchat_active` counter; exceeding the cap closes with
//!   code 4002.
//! - On accept, install a [`PlexChatSession`], emit
//!   `ServerMsg::Connected { jid }`, and run the receive loop.
//! - Dispatch every `ClientMsg` variant per SPEC §6.3:
//!   `message` (content cap → rate limit → sanitize → history append →
//!   stubbed agent echo reply; the real LLM agent loop wires in PR 3),
//!   `wallet_connect` / `wallet_disconnect` (with autonomous-mode owner
//!   verification stubbed for PR 2 — see TODO in
//!   [`handle_wallet_connect`]), `tx_result` (with SPEC §6.6 late-ok
//!   handling), and `tx_error` (with reason sanitization).
//! - Aliveness: a tokio `interval(PING_INTERVAL)` ping loop tracks the
//!   last pong instant via an `AtomicI64` (epoch ms). Two consecutive
//!   missed pongs (no pong within `PONG_TIMEOUT` of the last ping)
//!   terminate the socket.
//! - On close, cancel `session.abort` and reject every pending
//!   transaction with `Disconnected`.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket},
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use tracing::{debug, info, warn};
use zeroclaw_solana::context::{AgentMode, TxSendError};

use crate::AppState;

use super::auth::extract_ws_token;
use super::session::{
    CLOSE_TOO_MANY, ChatRole, MAX_MESSAGE_CONTENT_DEFAULT, PING_INTERVAL, PONG_TIMEOUT,
    PlexChatSession,
};
use super::wire::{ClientMsg, ServerMsg};

/// Stub agent name used by PR 2's echo reply. Replaced by the real
/// configured assistant_name in PR 3 when the LLM agent loop wires in.
const STUB_AGENT_NAME: &str = "Agent";

/// SPEC §6.5 reason cap — 200 characters of safe ASCII punctuation.
const SAFE_REASON_MAX_LEN: usize = 200;
/// Fallback substituted for missing or non-conforming `tx_error.reason`.
const SAFE_REASON_FALLBACK: &str = "User rejected or wallet error";

/// SPEC §6.2 subprotocol sentinel — clients send
/// `Sec-WebSocket-Protocol: bearer, <token>` and the server echoes
/// `bearer` back on accept.
const BEARER_SUBPROTOCOL: &str = "bearer";

/// Hard cap on concurrent PlexChat connections for PR 2.
///
/// The full gateway config will gain a `[plexchat] max_connections`
/// field in PR 3+; until then this is the SPEC §6.2 default.
pub const MAX_CONNECTIONS: usize = 10;

/// Query parameters accepted on the upgrade request. Only `token` is used
/// in PR 2 — additional params (e.g. `mode`) land alongside config wiring
/// in PR 2 Task 6.
#[derive(Debug, Default, Deserialize)]
pub struct PlexChatQuery {
    #[serde(default)]
    pub token: Option<String>,
}

/// GET `/ws/plexchat` — WebSocket upgrade for the PlexChat protocol.
pub async fn handle_ws_plexchat(
    State(state): State<AppState>,
    Query(params): Query<PlexChatQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // ── Auth gate ─────────────────────────────────────────────────────
    // When pairing is required, validate the bearer token via the SPEC
    // §6.2 carriers. `PairingGuard::is_authenticated` performs a hashed
    // membership check; the underlying token compare uses constant-time
    // semantics so timing attacks against the token store are not
    // distinguishable from a miss.
    if state.pairing.require_pairing() {
        let token = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
            )
                .into_response();
        }
    }

    // ── Subprotocol echo ──────────────────────────────────────────────
    // SPEC §6.2: when the client advertises `bearer` as a subprotocol the
    // server MUST echo it back so the handshake completes. We accept the
    // sentinel anywhere in the comma-separated list (case-insensitive)
    // for parity with browser clients that pass `['bearer', token]`.
    let echo_bearer = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| {
            protos
                .split(',')
                .any(|p| p.trim().eq_ignore_ascii_case(BEARER_SUBPROTOCOL))
        });
    let ws = if echo_bearer {
        ws.protocols([BEARER_SUBPROTOCOL])
    } else {
        ws
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

/// Drive a single PlexChat connection from upgrade to close.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let active = state.plexchat_active.clone();

    // ── MAX_CONNECTIONS gate ──────────────────────────────────────────
    // We increment optimistically and roll back on overflow so the check
    // is race-free without a mutex. `fetch_add` returns the prior value.
    let prior = active.fetch_add(1, Ordering::AcqRel);
    if prior >= MAX_CONNECTIONS {
        active.fetch_sub(1, Ordering::AcqRel);
        send_close(socket, CLOSE_TOO_MANY, "Too many connections").await;
        return;
    }

    // PR 3 will read the agent mode from `state.config.lock().plexchat`
    // (`PlexChatConfig::agent_mode`). For now every connection runs in
    // Public mode — autonomous-mode owner verification ships with the
    // tool-dispatch wiring.
    let session = PlexChatSession::new(AgentMode::Public);
    let jid = session.jid.clone();

    info!(
        jid = %jid,
        active = active.load(Ordering::Acquire),
        "PlexChat connection accepted"
    );

    // Run the connection until it closes for any reason. Cleanup runs
    // unconditionally afterwards so we cannot leak the active-counter
    // slot or leave pending transactions awaited forever.
    drive_connection(socket, session.clone()).await;

    session.abort.cancel();
    session.reject_all_pending();
    active.fetch_sub(1, Ordering::AcqRel);

    info!(
        jid = %jid,
        active = active.load(Ordering::Acquire),
        "PlexChat connection closed"
    );
}

/// Send a close frame with the given code/reason, swallowing any error.
async fn send_close(socket: WebSocket, code: u16, reason: &'static str) {
    let mut socket = socket;
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: Utf8Bytes::from_static(reason),
        })))
        .await;
}

/// Now-as-epoch-millis. Used by the ping/pong tracker so the writer task
/// can publish the last-pong time without holding a lock the read side
/// would have to await.
fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Drive the receive loop, ping/pong tracker, and outbound queue.
async fn drive_connection(socket: WebSocket, session: Arc<PlexChatSession>) {
    let (sender, mut receiver) = socket.split();

    // Outbound queue lets future tasks (ping loop, agent dispatch) all
    // funnel through a single writer without competing for the sink lock.
    let (tx_out, mut rx_out) = tokio::sync::mpsc::channel::<Message>(32);

    // Spawn the outbound writer.
    let writer_handle = tokio::spawn(async move {
        let mut sender: SplitSink<WebSocket, Message> = sender;
        while let Some(msg) = rx_out.recv().await {
            if sender.send(msg).await.is_err() {
                break;
            }
        }
        // Best-effort close.
        let _ = sender.close().await;
    });

    // Send the connected ack first so the client can settle handshake
    // before any other server traffic.
    let connected = ServerMsg::Connected {
        jid: session.jid.clone(),
    };
    if let Ok(json) = serde_json::to_string(&connected)
        && tx_out.send(Message::Text(json.into())).await.is_err()
    {
        // Writer is gone — bail before spawning the ping task.
        drop(tx_out);
        let _ = writer_handle.await;
        return;
    }

    // ── Ping/pong aliveness ───────────────────────────────────────────
    // The writer task pings on every tick; the receive loop updates
    // `last_pong_ms` whenever a Pong frame arrives. If two ticks elapse
    // without a fresh pong we cancel the session so the receive loop
    // tears down.
    let last_pong_ms = Arc::new(AtomicI64::new(now_epoch_ms()));

    {
        let session = session.clone();
        let tx_out = tx_out.clone();
        let last_pong_ms = last_pong_ms.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PING_INTERVAL);
            // Skip the immediate first tick — we just opened the socket.
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = session.abort.cancelled() => break,
                    _ = ticker.tick() => {
                        // If the previous pong is older than PONG_TIMEOUT,
                        // give up: cancel the session and let cleanup run.
                        let last = last_pong_ms.load(Ordering::Acquire);
                        let now = now_epoch_ms();
                        if now.saturating_sub(last) > PONG_TIMEOUT.as_millis() as i64 {
                            warn!(
                                jid = %session.jid,
                                "PlexChat pong timeout — terminating"
                            );
                            session.abort.cancel();
                            break;
                        }
                        // Send a ping. Failure means the writer is gone.
                        if tx_out.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // ── Receive loop ──────────────────────────────────────────────────
    loop {
        tokio::select! {
            _ = session.abort.cancelled() => break,
            frame = receiver.next() => {
                let Some(frame) = frame else { break };
                match frame {
                    Ok(Message::Text(text)) => {
                        if !handle_text_frame(&session, &tx_out, text.as_str()).await {
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => {
                        last_pong_ms.store(now_epoch_ms(), Ordering::Release);
                    }
                    Ok(Message::Ping(payload)) => {
                        // Auto-respond to client pings; treat as a sign of
                        // life so the aliveness loop doesn't trip.
                        last_pong_ms.store(now_epoch_ms(), Ordering::Release);
                        if tx_out.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Binary(_)) => {
                        // PlexChat is a JSON-text protocol — ignore binary.
                        debug!(jid = %session.jid, "ignored binary frame");
                    }
                    Err(e) => {
                        debug!(jid = %session.jid, error = %e, "PlexChat recv error");
                        break;
                    }
                }
            }
        }
    }

    // Cancel before awaiting the writer so the ping task drops its
    // outbound handle — otherwise the writer loop would block forever
    // on a `recv` whose only remaining sender is held by the ping task.
    session.abort.cancel();
    drop(tx_out);
    let _ = writer_handle.await;
}

/// Process a single text frame from the client. Returns `false` to
/// signal the receive loop should terminate.
async fn handle_text_frame(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    text: &str,
) -> bool {
    match serde_json::from_str::<ClientMsg>(text) {
        Ok(msg) => {
            debug!(
                jid = %session.jid,
                received = client_msg_kind(&msg),
                "PlexChat client message received"
            );
            dispatch_client_msg(session, tx_out, msg).await
        }
        Err(parse_err) => {
            let reply = ServerMsg::Error {
                error: parse_err.to_string(),
                code: Some("INVALID_MESSAGE".to_string()),
            };
            send_server_msg(tx_out, &reply).await
        }
    }
}

/// SPEC §6.3 client-message dispatch.
///
/// Each variant returns `true` to keep the receive loop alive. None of the
/// SPEC-defined error paths close the socket — the policy is to surface a
/// structured `Error { code }` server frame and leave it to the client to
/// retry or disconnect. Returning `false` from this function therefore only
/// happens when the outbound writer is gone and we cannot deliver any
/// further frames at all.
async fn dispatch_client_msg(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    msg: ClientMsg,
) -> bool {
    match msg {
        ClientMsg::Message {
            content,
            sender_name: _,
        } => handle_message(session, tx_out, content).await,
        ClientMsg::WalletConnect { address } => {
            handle_wallet_connect(session, tx_out, address).await
        }
        ClientMsg::WalletDisconnect => handle_wallet_disconnect(session, tx_out).await,
        ClientMsg::TxResult {
            correlation_id,
            signature,
        } => handle_tx_result(session, tx_out, correlation_id, signature).await,
        ClientMsg::TxError {
            correlation_id,
            reason,
        } => handle_tx_error(session, tx_out, correlation_id, reason).await,
    }
}

/// `message` handler — content cap, rate limit, sanitize, history append,
/// and a stubbed agent echo reply. The real LLM agent loop wires in PR 3.
async fn handle_message(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    content: String,
) -> bool {
    // 1. Length cap (SPEC §6.2). Compare bytes — `String::len` is bytes,
    // and the SPEC's 8000 figure is measured in bytes per the comment in
    // `session::MAX_MESSAGE_CONTENT_DEFAULT`.
    if content.len() > MAX_MESSAGE_CONTENT_DEFAULT {
        let err = ServerMsg::Error {
            error: format!("message exceeds {MAX_MESSAGE_CONTENT_DEFAULT} chars"),
            code: Some("MESSAGE_TOO_LONG".to_string()),
        };
        return send_server_msg(tx_out, &err).await;
    }

    // 2. Sliding-window rate limit. Do NOT close the socket — clients are
    // expected to back off and retry.
    if !session.check_rate_limit() {
        let err = ServerMsg::Error {
            error: "rate limit exceeded".to_string(),
            code: Some("RATE_LIMIT".to_string()),
        };
        return send_server_msg(tx_out, &err).await;
    }

    // 3. Sanitize before persisting to history (SPEC §6.2 prompt-injection
    // mitigation surface). The raw inbound bytes never enter chat state.
    let sanitized = sanitize_content(&content);

    // 4. Append to history as the user turn.
    session.push_history(ChatRole::User, sanitized.clone());

    // 5. Typing(true) before the agent reply.
    if !send_server_msg(tx_out, &ServerMsg::Typing { is_typing: true }).await {
        return false;
    }

    // 6. Stubbed agent reply for PR 2. PR 3 replaces this with a streaming
    // call into the agent loop with tools.
    let reply_content = format!("[stub] echo: {sanitized}");
    let reply = ServerMsg::Message {
        sender: STUB_AGENT_NAME.to_string(),
        content: reply_content.clone(),
    };
    if !send_server_msg(tx_out, &reply).await {
        return false;
    }
    session.push_history(ChatRole::Assistant, reply_content);

    // 7. Typing(false) tail-frame so clients can hide the indicator.
    send_server_msg(tx_out, &ServerMsg::Typing { is_typing: false }).await
}

/// `wallet_connect` handler — parse base58 pubkey, gate by mode, store on
/// the session, and emit `wallet_connected`.
async fn handle_wallet_connect(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    address: String,
) -> bool {
    let pubkey = match Pubkey::from_str(&address) {
        Ok(pk) => pk,
        Err(_) => {
            let err = ServerMsg::Error {
                error: "wallet address is not valid base58 / 32-byte pubkey".to_string(),
                code: Some("INVALID_WALLET".to_string()),
            };
            return send_server_msg(tx_out, &err).await;
        }
    };

    match session.mode {
        AgentMode::Public => {
            // Public mode: store the address. PR 3 will compare against an
            // optionally-configured owner wallet to flip `is_owner_verified`
            // when it matches; for PR 2 we leave the flag false.
            *session.wallet_address.lock() = Some(pubkey);
            *session.is_owner_verified.lock() = false;
        }
        AgentMode::Autonomous => {
            // TODO(PR 3): real owner verification via
            // `zeroclaw_solana::owner::resolve_owner` against the
            // configured `agent_asset_address`. For PR 2 we accept any
            // wallet that parses as a valid pubkey so the wire surface
            // stays exercisable; the policy gate is intentionally stubbed.
            *session.wallet_address.lock() = Some(pubkey);
            *session.is_owner_verified.lock() = true;
            warn!(
                jid = %session.jid,
                "PlexChat autonomous-mode wallet_connect accepted without owner verification (PR 2 stub — real check lands in PR 3)"
            );
        }
    }

    let connected = ServerMsg::WalletConnected { address };
    send_server_msg(tx_out, &connected).await
}

/// `wallet_disconnect` handler — clear wallet state and emit ack.
async fn handle_wallet_disconnect(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
) -> bool {
    *session.wallet_address.lock() = None;
    *session.is_owner_verified.lock() = false;
    send_server_msg(tx_out, &ServerMsg::WalletDisconnected).await
}

/// `tx_result` handler — resolve a pending oneshot or apply SPEC §6.6
/// late-ok handling. Autonomous mode rejects the message wholesale.
async fn handle_tx_result(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    correlation_id: String,
    signature: String,
) -> bool {
    if matches!(session.mode, AgentMode::Autonomous) {
        let err = ServerMsg::Error {
            error: "tx_result not allowed in autonomous mode".to_string(),
            code: Some("INVALID_MODE".to_string()),
        };
        return send_server_msg(tx_out, &err).await;
    }

    // Validate the signature by trying to parse it as a real Solana
    // `Signature` — this catches both bad-base58 and wrong-length payloads
    // in one shot, which is simpler than a regex check.
    let parsed = Signature::from_str(&signature);

    // Pop the pending entry under the lock; act on the responder afterwards.
    let pending = session.pending_txs.lock().remove(&correlation_id);

    match (pending, parsed) {
        (Some(entry), Ok(sig)) => {
            // Resolve the awaiting tool/responder with the parsed signature.
            let _ = entry.responder.send(Ok(sig));
            true
        }
        (Some(entry), Err(_)) => {
            // Plan-pragmatic choice (Task 4 doc): drop the entry + reject
            // the responder so the awaiting caller can surface a sane
            // error, then notify the client.
            let _ = entry
                .responder
                .send(Err(TxSendError::Rejected("invalid signature".to_string())));
            let err = ServerMsg::Error {
                error: "signature is not valid base58 / 64-byte signature".to_string(),
                code: Some("INVALID_SIGNATURE".to_string()),
            };
            send_server_msg(tx_out, &err).await
        }
        (None, Ok(_)) => {
            // SPEC §6.6 late-ok handling: a well-formed signature for a
            // correlation we no longer track (e.g. timed out, disconnected
            // and reconnected) is logged and silently accepted so the user
            // does not see a confusing error after a successful sign.
            info!(
                jid = %session.jid,
                correlation_id = %correlation_id,
                "[late-ok] tx_result for unknown correlation_id"
            );
            true
        }
        (None, Err(_)) => {
            let err = ServerMsg::Error {
                error: "unknown correlation_id".to_string(),
                code: Some("UNKNOWN_CORRELATION".to_string()),
            };
            send_server_msg(tx_out, &err).await
        }
    }
}

/// `tx_error` handler — sanitize the supplied reason, resolve a pending
/// oneshot with `TxSendError::Rejected`, or surface UNKNOWN_CORRELATION.
async fn handle_tx_error(
    session: &Arc<PlexChatSession>,
    tx_out: &tokio::sync::mpsc::Sender<Message>,
    correlation_id: String,
    reason: Option<String>,
) -> bool {
    if matches!(session.mode, AgentMode::Autonomous) {
        let err = ServerMsg::Error {
            error: "tx_error not allowed in autonomous mode".to_string(),
            code: Some("INVALID_MODE".to_string()),
        };
        return send_server_msg(tx_out, &err).await;
    }

    let sanitized_reason = sanitize_reason(reason.as_deref());

    let pending = session.pending_txs.lock().remove(&correlation_id);
    match pending {
        Some(entry) => {
            let _ = entry
                .responder
                .send(Err(TxSendError::Rejected(sanitized_reason)));
            true
        }
        None => {
            let err = ServerMsg::Error {
                error: "unknown correlation_id".to_string(),
                code: Some("UNKNOWN_CORRELATION".to_string()),
            };
            send_server_msg(tx_out, &err).await
        }
    }
}

/// Strip the four characters used by prompt-injection vectors (CR, LF,
/// `[`, `]`) from `s`. SPEC §6.2 calls these out specifically because
/// they are the building blocks of the `[USER]:` / `[ASSISTANT]:` style
/// role-spoof attacks against the agent prompt.
pub(crate) fn sanitize_content(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '[' | ']'))
        .collect()
}

/// Validate `reason` against SPEC §6.5's safe-reason character class
/// (`[\w\s.,:;'"()?!\-]{0,200}`) and substitute a fallback string when
/// the input is missing, oversize, or contains forbidden characters.
///
/// Implemented without `regex` because the rule is a simple per-character
/// allowlist plus a length cap, both of which are trivial to express with
/// `char::is_alphanumeric` + a literal punctuation set.
pub(crate) fn sanitize_reason(reason: Option<&str>) -> String {
    let Some(input) = reason else {
        return SAFE_REASON_FALLBACK.to_string();
    };
    if input.is_empty() {
        return SAFE_REASON_FALLBACK.to_string();
    }
    if input.chars().count() > SAFE_REASON_MAX_LEN {
        return SAFE_REASON_FALLBACK.to_string();
    }
    if !input.chars().all(is_safe_reason_char) {
        return SAFE_REASON_FALLBACK.to_string();
    }
    input.to_string()
}

/// Match the SPEC §6.5 safe-reason character class
/// `[\w\s.,:;'"()?!\-]`. `\w` is the standard regex shorthand for
/// `[A-Za-z0-9_]` plus the underscore — Unicode word chars are not
/// permitted because the LLM is fed this string verbatim and we want a
/// strict ASCII surface.
fn is_safe_reason_char(c: char) -> bool {
    if c.is_ascii_alphanumeric() {
        return true;
    }
    matches!(
        c,
        '_' | ' ' | '\t' | '.' | ',' | ':' | ';' | '\'' | '"' | '(' | ')' | '?' | '!' | '-'
    )
}

/// Return `true` iff `s` parses as a real Solana `Signature` (base58,
/// 64-byte payload). Used by handler unit tests; the dispatch path uses
/// `Signature::from_str` directly so the parsed value can be forwarded.
#[cfg(test)]
pub(crate) fn validates_signature(s: &str) -> bool {
    Signature::from_str(s).is_ok()
}

/// Serialise a `ServerMsg` and forward it to the writer task. Returns
/// `false` when the writer has gone (caller should break out).
async fn send_server_msg(tx_out: &tokio::sync::mpsc::Sender<Message>, msg: &ServerMsg) -> bool {
    let Ok(json) = serde_json::to_string(msg) else {
        return true;
    };
    tx_out.send(Message::Text(json.into())).await.is_ok()
}

/// Stable kind tag for a `ClientMsg`, used for trace logging only.
fn client_msg_kind(msg: &ClientMsg) -> &'static str {
    match msg {
        ClientMsg::Message { .. } => "message",
        ClientMsg::WalletConnect { .. } => "wallet_connect",
        ClientMsg::WalletDisconnect => "wallet_disconnect",
        ClientMsg::TxResult { .. } => "tx_result",
        ClientMsg::TxError { .. } => "tx_error",
    }
}

#[cfg(test)]
mod tests {
    //! Handler smoke tests.
    //!
    //! The integration test spawns a minimal axum app on a random
    //! 127.0.0.1 port (no `--ignored` gate needed since the loopback
    //! bind is hermetic) and exercises the happy-path handshake via
    //! `tokio_tungstenite`. Auth-failure / max-connections tests come
    //! in PR 2 Task 6 once the route is mounted on the real router.

    use super::*;
    use axum::{Router, routing::get};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    /// Build an `AppState` with auth disabled — mirrors `api::tests::test_state`
    /// but inlined to keep the test module self-contained.
    fn test_state() -> AppState {
        use async_trait::async_trait;
        use parking_lot::Mutex;
        use zeroclaw_config::schema::Config;
        use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry};
        use zeroclaw_providers::Provider;
        use zeroclaw_runtime::security::pairing::PairingGuard;

        struct MockProvider;
        #[async_trait]
        impl Provider for MockProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                Ok("ok".to_string())
            }
        }

        struct MockMemory;
        #[async_trait]
        impl Memory for MockMemory {
            fn name(&self) -> &str {
                "mock"
            }
            async fn store(
                &self,
                _key: &str,
                _content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _query: &str,
                _limit: usize,
                _session_id: Option<&str>,
                _since: Option<&str>,
                _until: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(Vec::new())
            }
            async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _category: Option<&MemoryCategory>,
                _session_id: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(Vec::new())
            }
            async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
                Ok(false)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
        }

        AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider: Arc::new(MockProvider),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(crate::GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(crate::IdempotencyStore::new(
                Duration::from_secs(300),
                1000,
            )),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(crate::nodes::NodeRegistry::new(16)),
            session_backend: None,
            session_queue: Arc::new(crate::session_queue::SessionActorQueue::new(8, 30, 600)),
            device_registry: None,
            pending_pairings: None,
            path_prefix: String::new(),
            web_dist_dir: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            plexchat_active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    #[test]
    fn client_msg_kind_covers_every_variant() {
        assert_eq!(
            client_msg_kind(&ClientMsg::Message {
                content: "x".into(),
                sender_name: None,
            }),
            "message"
        );
        assert_eq!(
            client_msg_kind(&ClientMsg::WalletConnect {
                address: "addr".into(),
            }),
            "wallet_connect"
        );
        assert_eq!(
            client_msg_kind(&ClientMsg::WalletDisconnect),
            "wallet_disconnect"
        );
        assert_eq!(
            client_msg_kind(&ClientMsg::TxResult {
                correlation_id: "c".into(),
                signature: "s".into(),
            }),
            "tx_result"
        );
        assert_eq!(
            client_msg_kind(&ClientMsg::TxError {
                correlation_id: "c".into(),
                reason: None,
            }),
            "tx_error"
        );
    }

    /// Happy-path integration smoke test: open a connection with the
    /// `bearer` subprotocol form and assert the server emits a
    /// `Connected { jid }` frame with a non-empty jid.
    #[tokio::test]
    async fn connected_emitted_on_accept() {
        let state = test_state();
        let active = state.plexchat_active.clone();
        let app = Router::new()
            .route("/ws/plexchat", get(handle_ws_plexchat))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            // Accept exactly one connection's worth of work, then exit.
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                })
                .await;
        });

        // Build a request with the SPEC §6.2 two-entry subprotocol.
        let url = format!("ws://{addr}/ws/plexchat");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("bearer, test-token"),
        );

        let (mut ws, response) = tokio_tungstenite::connect_async(request).await.unwrap();

        // Server should echo `bearer` back in the negotiated subprotocol.
        let echoed = response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(echoed, "bearer");

        // First server frame should be `connected`.
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("connected frame timed out")
            .expect("ws closed without sending a frame")
            .expect("ws error before connected frame");

        let text = match frame {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            other => panic!("expected text frame, got {other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "connected");
        assert!(v["jid"].as_str().is_some_and(|s| !s.is_empty()));

        // Counter reflects the open connection. The gauge is now scoped
        // to this test's `AppState` so we can assert exactly 1.
        for _ in 0..50 {
            if active.load(Ordering::Acquire) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(active.load(Ordering::Acquire), 1);

        // Clean shutdown — close the client and wait for the server to
        // tear down its side so the counter decrements before we assert.
        use futures_util::SinkExt;
        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        // Drain until close is acknowledged (bounded by a timeout so a
        // hung server can't make this test wait for graceful_shutdown).
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;

        // Give the server task a moment to run cleanup. We expect the
        // counter to drop back to zero.
        for _ in 0..50 {
            if active.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(active.load(Ordering::Acquire), 0);

        server.abort();
        let _ = server.await;
    }

    // ── Sanitizer unit tests ──────────────────────────────────────────

    #[test]
    fn sanitize_content_strips_newlines_and_brackets() {
        // The four characters SPEC §6.2 calls out are stripped wholesale;
        // every other character (including unicode) survives unchanged.
        let raw = "hello\r\n[ASSISTANT]: world\nfoo[bar]";
        let cleaned = sanitize_content(raw);
        assert_eq!(cleaned, "helloASSISTANT: worldfoobar");
    }

    #[test]
    fn sanitize_content_preserves_safe_text() {
        let raw = "Привіт! 1+1=2 — 'quoted' / (parens)";
        assert_eq!(sanitize_content(raw), raw);
    }

    #[test]
    fn sanitize_content_handles_empty() {
        assert_eq!(sanitize_content(""), "");
    }

    #[test]
    fn sanitize_reason_accepts_allowed_chars() {
        // Cover every character class in the safe regex
        // `[\w\s.,:;'"()?!\-]`: ASCII alphanumerics, underscore, space,
        // tab, the listed punctuation, and the hyphen.
        let s = "User_rejected: not enough SOL (0.5 vs 1.0). Try again? 'No' \"yes\"!";
        assert_eq!(sanitize_reason(Some(s)), s);
    }

    #[test]
    fn sanitize_reason_substitutes_for_forbidden_chars() {
        // Newlines/brackets/backticks are NOT in the safe class.
        let bad = "rejected\n[evil]";
        assert_eq!(sanitize_reason(Some(bad)), SAFE_REASON_FALLBACK);
    }

    #[test]
    fn sanitize_reason_substitutes_for_oversize() {
        let huge: String = "a".repeat(SAFE_REASON_MAX_LEN + 1);
        assert_eq!(sanitize_reason(Some(&huge)), SAFE_REASON_FALLBACK);
        // Exactly the cap is allowed.
        let at_cap: String = "a".repeat(SAFE_REASON_MAX_LEN);
        assert_eq!(sanitize_reason(Some(&at_cap)), at_cap);
    }

    #[test]
    fn sanitize_reason_falls_back_when_missing() {
        assert_eq!(sanitize_reason(None), SAFE_REASON_FALLBACK);
        assert_eq!(sanitize_reason(Some("")), SAFE_REASON_FALLBACK);
    }

    #[test]
    fn validates_signature_accepts_real_signature() {
        // A real, well-formed Solana signature (base58-encoded 64 bytes).
        // Generating one without a keypair is fiddly, so we round-trip a
        // known-good zero signature: `Signature::default()` encodes to a
        // long base58 string of all-1s.
        let sig = solana_sdk::signature::Signature::default().to_string();
        assert!(validates_signature(&sig));
    }

    #[test]
    fn validates_signature_rejects_garbage() {
        assert!(!validates_signature(""));
        assert!(!validates_signature("not-base58!"));
        // Right alphabet but wrong length.
        assert!(!validates_signature("abc"));
        // Real-looking base58 but only 32 bytes (a Pubkey, not a Signature).
        let pubkey = solana_sdk::pubkey::Pubkey::default().to_string();
        assert!(!validates_signature(&pubkey));
    }

    // ── Integration tests for client message dispatch ─────────────────

    /// Drive the WebSocket route on a random localhost port.
    ///
    /// Returns the local socket address and a `JoinHandle` for the server
    /// task. The caller is expected to abort the handle when finished;
    /// the server otherwise terminates after 5 seconds via the
    /// `with_graceful_shutdown` future, matching the existing connected
    /// smoke test.
    async fn spawn_test_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/ws/plexchat", get(handle_ws_plexchat))
            .with_state(test_state());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                })
                .await;
        });
        (addr, server)
    }

    /// Connect to the test server, consume the leading `connected` frame,
    /// and return the connected client.
    async fn connect_and_skip_connected(
        addr: SocketAddr,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}/ws/plexchat");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("bearer, test-token"),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();

        // Drop the leading `connected` frame.
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("connected frame timed out")
            .expect("ws closed before connected")
            .expect("ws error before connected");
        let text = match frame {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            other => panic!("expected text frame, got {other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "connected");
        ws
    }

    /// Read the next text frame as a JSON value, panicking on timeout.
    async fn next_json<S>(ws: &mut S) -> serde_json::Value
    where
        S: futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Unpin,
    {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws read timed out")
            .expect("ws closed prematurely")
            .expect("ws error");
        match frame {
            tokio_tungstenite::tungstenite::Message::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("expected text frame, got {other:?}"),
        }
    }

    /// `wallet_connect` → `wallet_connected`, `wallet_disconnect` →
    /// `wallet_disconnected`. Uses the system program's pubkey
    /// (all-zeros, base58-encoded as 32 ones) which `Pubkey::from_str`
    /// always accepts.
    #[tokio::test]
    async fn wallet_connect_disconnect_round_trip() {
        let (addr, server) = spawn_test_server().await;
        let mut ws = connect_and_skip_connected(addr).await;

        use futures_util::SinkExt;

        // wallet_connect
        let connect_msg = serde_json::json!({
            "type": "wallet_connect",
            "address": "11111111111111111111111111111111",
        });
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            connect_msg.to_string().into(),
        ))
        .await
        .unwrap();
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "wallet_connected");
        assert_eq!(v["address"], "11111111111111111111111111111111");

        // wallet_disconnect
        let disconnect_msg = serde_json::json!({"type": "wallet_disconnect"});
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            disconnect_msg.to_string().into(),
        ))
        .await
        .unwrap();
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "wallet_disconnected");

        // Clean shutdown.
        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;
        server.abort();
        let _ = server.await;
    }

    /// `wallet_connect` with a malformed address → structured error,
    /// socket stays open.
    #[tokio::test]
    async fn wallet_connect_invalid_address_emits_error() {
        let (addr, server) = spawn_test_server().await;
        let mut ws = connect_and_skip_connected(addr).await;

        use futures_util::SinkExt;

        let connect_msg = serde_json::json!({
            "type": "wallet_connect",
            "address": "not a real pubkey",
        });
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            connect_msg.to_string().into(),
        ))
        .await
        .unwrap();
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "INVALID_WALLET");

        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;
        server.abort();
        let _ = server.await;
    }

    /// `message` → typing(true) → stub-echo `message` → typing(false).
    #[tokio::test]
    async fn message_emits_typing_echo_typing() {
        let (addr, server) = spawn_test_server().await;
        let mut ws = connect_and_skip_connected(addr).await;

        use futures_util::SinkExt;

        let msg = serde_json::json!({"type": "message", "content": "hi"});
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await
        .unwrap();

        // typing: true
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "typing");
        assert_eq!(v["isTyping"], true);

        // stubbed echo
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "message");
        assert_eq!(v["sender"], "Agent");
        assert_eq!(v["content"], "[stub] echo: hi");

        // typing: false
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "typing");
        assert_eq!(v["isTyping"], false);

        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;
        server.abort();
        let _ = server.await;
    }

    /// Oversize message → MESSAGE_TOO_LONG error frame, socket stays open.
    #[tokio::test]
    async fn oversize_message_emits_error() {
        let (addr, server) = spawn_test_server().await;
        let mut ws = connect_and_skip_connected(addr).await;

        use futures_util::SinkExt;

        // One byte over the cap.
        let oversize: String = "a".repeat(MAX_MESSAGE_CONTENT_DEFAULT + 1);
        let msg = serde_json::json!({"type": "message", "content": oversize});
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await
        .unwrap();
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "MESSAGE_TOO_LONG");

        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;
        server.abort();
        let _ = server.await;
    }

    /// `tx_result` for an unknown correlation_id with a malformed signature
    /// → UNKNOWN_CORRELATION (the late-ok path requires a *well-formed*
    /// signature; a malformed one exposes a bug in the client).
    #[tokio::test]
    async fn tx_result_unknown_correlation_emits_error() {
        let (addr, server) = spawn_test_server().await;
        let mut ws = connect_and_skip_connected(addr).await;

        use futures_util::SinkExt;

        let msg = serde_json::json!({
            "type": "tx_result",
            "correlationId": "no-such-id",
            "signature": "garbage!",
        });
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await
        .unwrap();
        let v = next_json(&mut ws).await;
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "UNKNOWN_CORRELATION");

        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while ws.next().await.is_some() {}
        })
        .await;
        server.abort();
        let _ = server.await;
    }

    // ── PR 2 Task 6 integration tests ─────────────────────────────────

    /// Build an `AppState` with pairing required and one valid bearer
    /// token. Used by `wrong_token_returns_401` to exercise the auth gate.
    fn test_state_with_pairing(token: &str) -> AppState {
        use parking_lot::Mutex;
        use zeroclaw_config::schema::Config;
        use zeroclaw_runtime::security::pairing::PairingGuard;

        let mut state = test_state();
        state.config = Arc::new(Mutex::new(Config::default()));
        state.pairing = Arc::new(PairingGuard::new(true, &[token.to_string()]));
        state
    }

    /// Spawn the PlexChat route on a random localhost port using the
    /// supplied `AppState`. Mirrors `spawn_test_server` but lets callers
    /// inject auth/state knobs.
    async fn spawn_with_state(state: AppState) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/ws/plexchat", get(handle_ws_plexchat))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                })
                .await;
        });
        (addr, server)
    }

    /// SPEC §6.2: a wrong bearer token must produce HTTP 401 *without*
    /// completing the WebSocket upgrade. We use `reqwest` to issue a
    /// straight HTTP request rather than `tokio_tungstenite::connect_async`
    /// because tungstenite collapses non-101 responses into a generic
    /// error and we want to assert the status code precisely.
    #[tokio::test]
    async fn wrong_token_returns_401() {
        let state = test_state_with_pairing("the-real-token");
        let (addr, server) = spawn_with_state(state).await;

        let url = format!("http://{addr}/ws/plexchat");
        // Build the upgrade headers by hand. The server's `extract_ws_token`
        // accepts any of the SPEC §6.2 carriers; we intentionally send the
        // wrong token via Authorization to provoke a 401.
        let resp = reqwest::Client::new()
            .get(&url)
            .header("authorization", "Bearer not-the-real-token")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .expect("HTTP request failed");

        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        server.abort();
        let _ = server.await;
    }

    /// SPEC §6.2: opening more than `MAX_CONNECTIONS` connections in
    /// parallel must close the surplus ones with code 4002.
    ///
    /// The active-connection gauge now lives on `AppState`, so this test
    /// is hermetic — it doesn't race with other tests on a process-global
    /// static. We open `MAX_CONNECTIONS` connections, then the
    /// `(MAX + 1)`th, and assert the surplus closes with 4002.
    #[tokio::test]
    async fn max_connections_returns_4002() {
        let (addr, server) = spawn_test_server().await;

        // Hold the first MAX_CONNECTIONS connections open until the test
        // is done so the server's gauge stays saturated.
        let mut held = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            let ws = connect_and_skip_connected(addr).await;
            held.push(ws);
        }

        // Now open one more. The handshake completes (HTTP 101) but the
        // server immediately sends a Close(4002) — we should observe it
        // as the very first frame after upgrade.
        let url = format!("ws://{addr}/ws/plexchat");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("bearer, test-token"),
        );
        let (mut surplus, _resp) = tokio_tungstenite::connect_async(request).await.unwrap();

        // Drain frames until we either see the close or hit the timeout.
        // tokio-tungstenite delivers Close as a `Message::Close` frame
        // and then yields `None` (or an error) on the next read.
        let mut got_close_4002 = false;
        for _ in 0..4 {
            let frame = tokio::time::timeout(Duration::from_secs(2), surplus.next()).await;
            match frame {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(Some(cf))))) => {
                    let code: u16 = cf.code.into();
                    assert_eq!(
                        code, CLOSE_TOO_MANY,
                        "expected close 4002, got {code} ({})",
                        cf.reason
                    );
                    got_close_4002 = true;
                    break;
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            }
        }
        assert!(
            got_close_4002,
            "surplus connection did not receive close 4002"
        );

        // Drop the held connections so the server cleans up.
        drop(held);
        server.abort();
        let _ = server.await;
    }
}
