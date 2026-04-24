//! Thin async wrapper around `solana_client::nonblocking::rpc_client::RpcClient`.
//!
//! All RPC calls tools make go through this client so tests can swap in a
//! mock by constructing `SolanaClient::from_rpc(Arc<dyn RpcLike>)` (added
//! when first tool needs it — PR 3).

use anyhow::{Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use std::sync::Arc;
use std::time::Duration;

pub struct SolanaClient {
    pub rpc: Arc<RpcClient>,
    pub rpc_url: String,
}

impl SolanaClient {
    /// Construct from a raw RPC URL. Uses `CommitmentConfig::confirmed()` by default.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        let url = rpc_url.into();
        let rpc = Arc::new(RpcClient::new_with_timeout_and_commitment(
            url.clone(),
            Duration::from_secs(30),
            CommitmentConfig::confirmed(),
        ));
        Self { rpc, rpc_url: url }
    }

    /// Preflight check: prove the RPC endpoint is reachable (SPEC §6.8).
    pub async fn preflight(&self) -> Result<u64> {
        self.rpc
            .get_slot()
            .await
            .context("RPC preflight get_slot failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_url_verbatim() {
        let c = SolanaClient::new("http://example.invalid:8899");
        assert_eq!(c.rpc_url, "http://example.invalid:8899");
    }

    #[tokio::test]
    async fn preflight_against_unreachable_rpc_errors() {
        // Use a guaranteed-unreachable port. No network call expected to succeed.
        let c = SolanaClient::new("http://127.0.0.1:1");
        let err = c.preflight().await.unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("preflight") || msg.contains("get_slot") || msg.contains("connection"),
            "unexpected error shape: {msg}"
        );
    }
}
