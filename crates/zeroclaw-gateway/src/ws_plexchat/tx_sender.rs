//! PlexChat transaction sender — implements `TxSender` for public-mode tools.
//!
//! Public-mode tools that need a user signature (`submit_or_send` in PR 3)
//! call [`TxSender::send_and_await`]. This implementation:
//!
//! 1. Generates a fresh `correlation_id` (UUID v4).
//! 2. Inserts a [`PendingTx`] entry on the owning [`PlexChatSession`] keyed by
//!    that id, with a `oneshot::Sender` the WS receive loop fulfils when the
//!    matching `tx_result` / `tx_error` arrives (see `handler::handle_tx_*`).
//! 3. Emits a [`ServerMsg::Transaction`] frame to the outbound writer queue.
//! 4. Awaits the responder under [`TX_AWAIT_TIMEOUT`] (5 minutes per SPEC §6.6).
//! 5. On timeout, removes the pending entry so a late `tx_result` falls into
//!    the SPEC §6.6 "late-ok" branch instead of resolving a stale awaiter.
//!
//! No real callers wire up here in PR 2 — tools land in PR 3. The unit tests
//! below cover success, timeout, and both flavours of "disconnected" (outbound
//! channel closed before send, and the owning session dropped before the call).
//!
//! ## Testability of the timeout path
//!
//! The trait method [`TxSender::send_and_await`] is hard-wired to
//! [`TX_AWAIT_TIMEOUT`] (5 minutes). To keep the timeout test fast and
//! deterministic without pulling in tokio's `test-util` feature for
//! `tokio::time::pause`, the body delegates to a `pub(crate)` helper
//! [`PlexChatTxSender::send_and_await_with_timeout`] that takes the duration
//! as a parameter. The unit test calls the helper directly with a small
//! duration (~50ms); production callers go through the trait and get the
//! full 5-minute window.

use std::sync::Weak;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use solana_sdk::signature::Signature;
use tokio::sync::{mpsc, oneshot};
use zeroclaw_solana::context::{TxSendError, TxSendOpts, TxSender};

use super::session::{PendingTx, PlexChatSession, TX_AWAIT_TIMEOUT};
use super::wire::ServerMsg;

/// `TxSender` implementation that drives a single PlexChat WebSocket session.
///
/// Holds a [`Weak`] reference to the owning session so a dropped connection
/// can fail outstanding sends fast (see [`TxSendError::Disconnected`]) instead
/// of blocking on an `mpsc` whose receiver is gone.
pub struct PlexChatTxSender {
    pub(crate) session: Weak<PlexChatSession>,
    pub(crate) outbound: mpsc::Sender<ServerMsg>,
}

impl PlexChatTxSender {
    /// Construct a new sender bound to `session` writing through `outbound`.
    pub fn new(session: Weak<PlexChatSession>, outbound: mpsc::Sender<ServerMsg>) -> Self {
        Self { session, outbound }
    }

    /// Internal worker that takes the timeout as a parameter so the unit test
    /// can pass a small duration without poking at tokio's `pause`/`advance`
    /// (which would require enabling the `test-util` feature crate-wide).
    ///
    /// The trait impl below delegates to this with [`TX_AWAIT_TIMEOUT`].
    pub(crate) async fn send_and_await_with_timeout(
        &self,
        tx_base64: String,
        opts: TxSendOpts,
        timeout: Duration,
    ) -> Result<Signature, TxSendError> {
        // 1. Resolve the strong session reference. If the connection has
        //    already been torn down there is no point waiting at all.
        let session = self.session.upgrade().ok_or(TxSendError::Disconnected)?;

        // 2. Mint a fresh correlation id. `Uuid::new_v4` is collision-free in
        //    practice for the lifetime of a single session.
        let correlation_id = uuid::Uuid::new_v4().to_string();

        // 3. Register the pending oneshot under the lock before emitting the
        //    server message. Doing this in the opposite order would race a
        //    fast client that replied before we recorded the entry.
        let (responder_tx, responder_rx) = oneshot::channel();
        session.pending_txs.lock().insert(
            correlation_id.clone(),
            PendingTx {
                responder: responder_tx,
                created_at: Instant::now(),
            },
        );

        // 4. Emit the prompt frame. If the writer is gone, drop our entry
        //    so we don't leave a phantom oneshot dangling on the session.
        let msg = ServerMsg::Transaction {
            transaction: tx_base64,
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

        // 5. Await the client callback. Three resolutions:
        //    - `Ok(Ok(result))` — handler delivered a verdict; forward it.
        //    - `Ok(Err(_))`     — responder dropped without sending; this
        //      happens when `reject_all_pending` runs on disconnect.
        //    - `Err(_)`         — `tokio::time::timeout` elapsed. Pop the
        //      entry so a late `tx_result` falls into the SPEC §6.6
        //      "late-ok" branch instead of completing this call.
        match tokio::time::timeout(timeout, responder_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(TxSendError::Disconnected),
            Err(_) => {
                session.pending_txs.lock().remove(&correlation_id);
                Err(TxSendError::Timeout)
            }
        }
    }
}

#[async_trait]
impl TxSender for PlexChatTxSender {
    async fn send_and_await(
        &self,
        tx_base64: String,
        opts: TxSendOpts,
    ) -> Result<Signature, TxSendError> {
        self.send_and_await_with_timeout(tx_base64, opts, TX_AWAIT_TIMEOUT)
            .await
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for `PlexChatTxSender`.
    //!
    //! These tests deliberately do NOT exercise the WebSocket — they wire a
    //! mpsc channel and an `Arc<PlexChatSession>` directly so we can drive
    //! the correlation map by hand and assert the failure paths in
    //! deterministic time.
    //!
    //! Why not `tokio::time::pause`? It requires the `test-util` feature on
    //! the tokio dev-dep, which would also affect any test that imports a
    //! tokio timer. Passing the timeout as a `Duration` to
    //! `send_and_await_with_timeout` is just as effective and keeps the
    //! production path on the SPEC-defined 5-minute window.
    use super::*;
    use crate::ws_plexchat::session::{PlexChatSession, TX_AWAIT_TIMEOUT};
    use std::sync::Arc;
    use std::time::Duration;
    use zeroclaw_solana::context::AgentMode;

    /// Helper: build a `PlexChatTxSender` plus its mpsc receiver and a strong
    /// `Arc<PlexChatSession>` (the sender holds a `Weak`).
    fn rig() -> (
        Arc<PlexChatSession>,
        PlexChatTxSender,
        mpsc::Receiver<ServerMsg>,
    ) {
        let session = PlexChatSession::new(AgentMode::Public);
        let (tx, rx) = mpsc::channel::<ServerMsg>(8);
        let sender = PlexChatTxSender::new(Arc::downgrade(&session), tx);
        (session, sender, rx)
    }

    /// Success path: emit the `Transaction` frame, fulfil the pending
    /// oneshot from the receiving side, and assert the trait method
    /// resolves to the expected `Signature`.
    #[tokio::test]
    async fn send_and_await_success_resolves_with_signature() {
        let (session, sender, mut rx) = rig();

        // Spawn the awaiting call. The trait method goes through the full
        // 5-minute timeout — that's fine because we resolve it immediately
        // below and never let the timer elapse.
        let opts = TxSendOpts {
            message: Some("Buyback".into()),
            index: Some(1),
            total: Some(3),
            fee_sol: Some(0.001),
        };
        let join = tokio::spawn({
            let sender = Arc::new(sender);
            async move { sender.send_and_await("b64-tx".into(), opts).await }
        });

        // Receive the Transaction frame and pull the correlation id.
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("transaction frame timed out")
            .expect("outbound channel closed unexpectedly");
        let (correlation_id, transaction, message, index, total, fee_sol) = match frame {
            ServerMsg::Transaction {
                transaction,
                correlation_id,
                message,
                index,
                total,
                fee_sol,
            } => (correlation_id, transaction, message, index, total, fee_sol),
            other => panic!("expected Transaction frame, got {other:?}"),
        };
        assert_eq!(transaction, "b64-tx");
        assert_eq!(message.as_deref(), Some("Buyback"));
        assert_eq!(index, Some(1));
        assert_eq!(total, Some(3));
        assert_eq!(fee_sol, Some(0.001));
        assert!(!correlation_id.is_empty());

        // Resolve the pending oneshot (mirrors what `handle_tx_result` does
        // when the matching client callback arrives).
        let pending = session
            .pending_txs
            .lock()
            .remove(&correlation_id)
            .expect("pending entry should be registered");
        pending
            .responder
            .send(Ok(Signature::default()))
            .expect("responder send should succeed");

        let resolved = tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("awaiter did not resolve in time")
            .expect("task panicked")
            .expect("send_and_await returned an error");
        assert_eq!(resolved, Signature::default());
        assert!(session.pending_txs.lock().is_empty());
    }

    /// Timeout path via the `pub(crate)` helper. Using a small duration keeps
    /// the test well under a second and avoids depending on `tokio::time`
    /// feature flags.
    #[tokio::test]
    async fn send_and_await_with_timeout_yields_timeout_and_clears_pending() {
        let (session, sender, mut rx) = rig();

        // Drain the outgoing Transaction frame in a background task so the
        // `mpsc::send` inside `send_and_await_with_timeout` doesn't block on
        // a full channel — but we deliberately never resolve the oneshot.
        let drain = tokio::spawn(async move {
            // The helper sends exactly one frame; await it here and assert
            // it really is a Transaction so the test fails loudly if the
            // implementation regresses.
            let frame = rx.recv().await.expect("outbound channel closed");
            assert!(matches!(frame, ServerMsg::Transaction { .. }));
        });

        let result = sender
            .send_and_await_with_timeout(
                "b64-tx".into(),
                TxSendOpts::default(),
                Duration::from_millis(50),
            )
            .await;
        assert!(matches!(result, Err(TxSendError::Timeout)));

        // The pending entry must be reclaimed so a late tx_result falls into
        // the SPEC §6.6 "late-ok" branch rather than completing a stale call.
        assert!(session.pending_txs.lock().is_empty());

        drain.await.unwrap();
    }

    /// Disconnected on send: drop the receiver before the call so the very
    /// first `outbound.send` fails. The implementation must surface
    /// `Disconnected` and clean the pending entry.
    #[tokio::test]
    async fn send_and_await_with_dropped_receiver_returns_disconnected() {
        let (session, sender, rx) = rig();
        drop(rx);

        let result = sender
            .send_and_await_with_timeout(
                "b64-tx".into(),
                TxSendOpts::default(),
                Duration::from_millis(50),
            )
            .await;
        assert!(matches!(result, Err(TxSendError::Disconnected)));
        assert!(
            session.pending_txs.lock().is_empty(),
            "pending entry must be cleared after a failed send"
        );
    }

    /// Disconnected via dropped session: the `Weak<PlexChatSession>` cannot
    /// upgrade, so we never even try to push a frame. This is the path
    /// hit when a tool call lingers past WebSocket teardown.
    #[tokio::test]
    async fn send_and_await_with_dropped_session_returns_disconnected() {
        let (session, sender, _rx) = rig();
        drop(session);

        let result = sender
            .send_and_await_with_timeout(
                "b64-tx".into(),
                TxSendOpts::default(),
                Duration::from_millis(50),
            )
            .await;
        assert!(matches!(result, Err(TxSendError::Disconnected)));
    }

    /// Sanity check: the trait impl really does delegate to the helper with
    /// the SPEC-mandated 5-minute window. We can't observe the duration
    /// directly without pausing time, but we can prove the trait method's
    /// happy path matches the helper's by resolving the pending tx
    /// immediately.
    #[tokio::test]
    async fn trait_send_and_await_uses_default_timeout_constant() {
        // This is a compile-time assertion as much as a runtime one — if the
        // constant ever moves we want this test to drag attention to it.
        const _: () = {
            // 5 minutes at the time of writing; bump this assert if SPEC
            // §6.6 changes.
            assert!(TX_AWAIT_TIMEOUT.as_secs() == 300);
        };

        let (session, sender, mut rx) = rig();
        let sender = Arc::new(sender);
        let join = {
            let sender = sender.clone();
            tokio::spawn(async move {
                sender
                    .send_and_await("b64-tx".into(), TxSendOpts::default())
                    .await
            })
        };

        let frame = rx.recv().await.expect("transaction frame missing");
        let correlation_id = match frame {
            ServerMsg::Transaction { correlation_id, .. } => correlation_id,
            other => panic!("expected Transaction frame, got {other:?}"),
        };
        let pending = session
            .pending_txs
            .lock()
            .remove(&correlation_id)
            .expect("pending entry");
        pending.responder.send(Ok(Signature::default())).unwrap();

        let resolved = join.await.unwrap().unwrap();
        assert_eq!(resolved, Signature::default());
    }
}
