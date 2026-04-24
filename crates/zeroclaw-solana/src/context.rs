//! `AgentContext` and supporting types — injected into tools via `tokio::task_local!`
//! (plumbing set up in PR 3).

use crate::error_codes::ToolErrorCode;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    Public,
    Autonomous,
}

/// Options for a transaction prompted to a connected user wallet.
#[derive(Debug, Clone, Default)]
pub struct TxSendOpts {
    pub message: Option<String>,
    pub index: Option<u32>,
    pub total: Option<u32>,
    pub fee_sol: Option<f64>,
}

/// Public-mode per-session transaction sender. Implementations emit
/// a `transaction` WS message and await the client's `tx_result` /
/// `tx_error` callback.
#[async_trait]
pub trait TxSender: Send + Sync {
    async fn send_and_await(
        &self,
        tx_base64: String,
        opts: TxSendOpts,
    ) -> Result<Signature, TxSendError>;
}

/// Errors from `TxSender::send_and_await` — already sanitized for the LLM.
#[derive(Debug, thiserror::Error)]
pub enum TxSendError {
    #[error("user rejected or wallet error: {0}")]
    Rejected(String),
    #[error("transaction prompt timed out")]
    Timeout,
    #[error("client disconnected")]
    Disconnected,
    #[error("unknown correlation id")]
    UnknownCorrelation,
    #[error("invalid mode for transaction prompt")]
    InvalidMode,
}

impl TxSendError {
    pub fn as_tool_error_code(&self) -> ToolErrorCode {
        match self {
            TxSendError::Timeout => ToolErrorCode::Timeout,
            TxSendError::Rejected(_)
            | TxSendError::Disconnected
            | TxSendError::UnknownCorrelation
            | TxSendError::InvalidMode => ToolErrorCode::Generic,
        }
    }
}

/// Everything a tool may need to know about the calling session + server config.
/// Matches SPEC Appendix B (AgentContext).
#[derive(Clone)]
pub struct AgentContext {
    pub wallet_address: Option<Pubkey>,
    pub agent_mode: AgentMode,
    pub agent_asset_address: Option<Pubkey>,
    pub agent_token_mint: Option<Pubkey>,
    pub agent_fee_sol: f64,
    pub token_override: Option<Pubkey>,
    pub owner_wallet: Option<Pubkey>,
    pub tx_sender: Option<Arc<dyn TxSender>>,
    pub solana: Arc<crate::client::SolanaClient>,
}

impl std::fmt::Debug for AgentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentContext")
            .field("wallet_address", &self.wallet_address)
            .field("agent_mode", &self.agent_mode)
            .field("agent_asset_address", &self.agent_asset_address)
            .field("agent_token_mint", &self.agent_token_mint)
            .field("agent_fee_sol", &self.agent_fee_sol)
            .field("token_override", &self.token_override)
            .field("owner_wallet", &self.owner_wallet)
            .field(
                "tx_sender",
                &self.tx_sender.as_ref().map(|_| "<dyn TxSender>"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mode_serialization_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&AgentMode::Public).unwrap(),
            "\"public\""
        );
        assert_eq!(
            serde_json::to_string(&AgentMode::Autonomous).unwrap(),
            "\"autonomous\""
        );
    }

    #[test]
    fn tx_send_error_maps_to_tool_error_code() {
        assert_eq!(
            TxSendError::Timeout.as_tool_error_code(),
            ToolErrorCode::Timeout
        );
        assert_eq!(
            TxSendError::Disconnected.as_tool_error_code(),
            ToolErrorCode::Generic
        );
        assert_eq!(
            TxSendError::UnknownCorrelation.as_tool_error_code(),
            ToolErrorCode::Generic
        );
        assert_eq!(
            TxSendError::InvalidMode.as_tool_error_code(),
            ToolErrorCode::Generic
        );
        assert_eq!(
            TxSendError::Rejected("foo".into()).as_tool_error_code(),
            ToolErrorCode::Generic
        );
    }

    #[test]
    fn agent_context_debug_does_not_expose_tx_sender_internals() {
        let ctx = AgentContext {
            wallet_address: None,
            agent_mode: AgentMode::Public,
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol: 0.001,
            token_override: None,
            owner_wallet: None,
            tx_sender: None,
            solana: Arc::new(crate::client::SolanaClient::new("http://example.invalid")),
        };
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("AgentContext"));
        assert!(dbg.contains("agent_mode"));
        // No network call attempted.
    }
}
