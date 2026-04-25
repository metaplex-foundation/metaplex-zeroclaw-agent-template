//! Per-WebSocket-connection session state for the PlexChat route.

use parking_lot::Mutex;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
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
        if w.len() >= RATE_LIMIT {
            return false;
        }
        w.push_back(now);
        true
    }

    /// Append a turn, dropping the oldest if at MAX_HISTORY.
    pub fn push_history(&self, role: ChatRole, content: String) {
        let mut h = self.history.lock();
        if h.len() == MAX_HISTORY {
            h.pop_front();
        }
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
        s.pending_txs.lock().insert(
            "a".into(),
            PendingTx {
                responder: tx1,
                created_at: Instant::now(),
            },
        );
        s.pending_txs.lock().insert(
            "b".into(),
            PendingTx {
                responder: tx2,
                created_at: Instant::now(),
            },
        );
        s.reject_all_pending();
        assert!(matches!(rx1.await.unwrap(), Err(TxSendError::Disconnected)));
        assert!(matches!(rx2.await.unwrap(), Err(TxSendError::Disconnected)));
        assert!(s.pending_txs.lock().is_empty());
    }
}
