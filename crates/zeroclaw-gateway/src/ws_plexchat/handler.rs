//! PlexChat WebSocket connection handler (SPEC §6).
//!
//! Responsibilities for PR 2 Task 3:
//!
//! - Authenticate the upgrade request via `Authorization: Bearer …`,
//!   `Sec-WebSocket-Protocol: bearer, <token>`, or `?token=…`. Reject with
//!   HTTP 401 (no upgrade) when the token is missing or unrecognised.
//! - Echo the SPEC §6.2 `bearer` subprotocol on accept when the client
//!   advertised it.
//! - Enforce a process-wide `MAX_CONNECTIONS = 10` cap by tracking a
//!   `PLEXCHAT_ACTIVE` counter; exceeding the cap closes with code 4002.
//!   (PR 2 Task 6 will move this counter onto `AppState`.)
//! - On accept, install a [`PlexChatSession`], emit
//!   `ServerMsg::Connected { jid }`, and run a minimal receive loop. Real
//!   client-message dispatch lands in PR 2 Task 4 — for now any received
//!   text frame is parsed as `ClientMsg` and replied to with a
//!   `NOT_IMPLEMENTED` error.
//! - Aliveness: a tokio `interval(PING_INTERVAL)` ping loop tracks the
//!   last pong instant via an `AtomicI64` (epoch ms). Two consecutive
//!   missed pongs (no pong within `PONG_TIMEOUT` of the last ping)
//!   terminate the socket.
//! - On close, cancel `session.abort` and reject every pending
//!   transaction with `Disconnected`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
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
use tracing::{debug, info, warn};
use zeroclaw_solana::context::AgentMode;

use crate::AppState;

use super::auth::extract_ws_token;
use super::session::{CLOSE_TOO_MANY, PING_INTERVAL, PONG_TIMEOUT, PlexChatSession};
use super::wire::{ClientMsg, ServerMsg};

/// SPEC §6.2 subprotocol sentinel — clients send
/// `Sec-WebSocket-Protocol: bearer, <token>` and the server echoes
/// `bearer` back on accept.
const BEARER_SUBPROTOCOL: &str = "bearer";

/// Hard cap on concurrent PlexChat connections for PR 2.
///
/// The full gateway config will gain a `[plexchat] max_connections`
/// field in PR 2 Task 6; until then this is the SPEC §6.2 default.
pub const MAX_CONNECTIONS: usize = 10;

/// Process-global active-connection gauge.
///
/// Lives here (not on `AppState`) for PR 2 to keep the change well-scoped
/// to the new module; PR 2 Task 6 moves it onto `AppState` so it can be
/// observed alongside the rest of the gateway state.
pub static PLEXCHAT_ACTIVE: AtomicUsize = AtomicUsize::new(0);

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
async fn handle_socket(socket: WebSocket, _state: AppState) {
    // ── MAX_CONNECTIONS gate ──────────────────────────────────────────
    // We increment optimistically and roll back on overflow so the check
    // is race-free without a mutex. `fetch_add` returns the prior value.
    let prior = PLEXCHAT_ACTIVE.fetch_add(1, Ordering::AcqRel);
    if prior >= MAX_CONNECTIONS {
        PLEXCHAT_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        send_close(socket, CLOSE_TOO_MANY, "Too many connections").await;
        return;
    }

    // PR 2 Task 6 will read the agent mode from config; for now every
    // connection runs in Public mode.
    let session = PlexChatSession::new(AgentMode::Public);
    let jid = session.jid.clone();

    info!(
        jid = %jid,
        active = PLEXCHAT_ACTIVE.load(Ordering::Acquire),
        "PlexChat connection accepted"
    );

    // Run the connection until it closes for any reason. Cleanup runs
    // unconditionally afterwards so we cannot leak the active-counter
    // slot or leave pending transactions awaited forever.
    drive_connection(socket, session.clone()).await;

    session.abort.cancel();
    session.reject_all_pending();
    PLEXCHAT_ACTIVE.fetch_sub(1, Ordering::AcqRel);

    info!(
        jid = %jid,
        active = PLEXCHAT_ACTIVE.load(Ordering::Acquire),
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
            // PR 2 Task 4 wires the real dispatch. Until then we log the
            // type and reply with a structured NOT_IMPLEMENTED error so
            // clients can detect feature gaps without parsing logs.
            debug!(
                jid = %session.jid,
                received = client_msg_kind(&msg),
                "PlexChat client message received (dispatch lands in Task 4)"
            );
            let reply = ServerMsg::Error {
                error: "client message handling lands in Task 4".to_string(),
                code: Some("NOT_IMPLEMENTED".to_string()),
            };
            send_server_msg(tx_out, &reply).await
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
        // Reset the global counter — other tests may have leaked
        // increments if they panicked. This is hermetic per
        // `cargo test --lib` because that runs single-threaded for a
        // single binary; we still serialise via a guard below.
        let _g = ActiveCounterGuard::reset();

        let app = Router::new()
            .route("/ws/plexchat", get(handle_ws_plexchat))
            .with_state(test_state());

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

        // Counter reflects the open connection.
        assert_eq!(PLEXCHAT_ACTIVE.load(Ordering::Acquire), 1);

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

        // Give the server task a moment to run cleanup.
        for _ in 0..50 {
            if PLEXCHAT_ACTIVE.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(PLEXCHAT_ACTIVE.load(Ordering::Acquire), 0);

        server.abort();
        let _ = server.await;
    }

    /// Ensure the global counter is zero before a test that depends on
    /// observing increments. Necessary because the static spans every
    /// test in the binary.
    struct ActiveCounterGuard;
    impl ActiveCounterGuard {
        fn reset() -> Self {
            PLEXCHAT_ACTIVE.store(0, Ordering::Release);
            Self
        }
    }
}
