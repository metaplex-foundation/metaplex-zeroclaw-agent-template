//! Tools available in both `AGENT_MODE=public` and `AGENT_MODE=autonomous`.
//!
//! PR 3 ships the read-only subset (SPEC §5.1, top half of the shared
//! table). Identity / treasury tools land in PR 4–6.

pub mod get_balance;
pub mod get_token_balances;
pub mod get_token_metadata;
pub mod get_token_price;
pub mod get_transaction;
pub mod sleep;

use std::sync::Arc;
use zeroclaw_api::tool::Tool;
use zeroclaw_solana::auth::{AuthLevel, with_auth};

/// Compose the shared read-only tool registry. All read-only tools are
/// `AuthLevel::Public` per SPEC §5.1.
pub fn shared_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        with_auth(get_balance::GetBalance, AuthLevel::Public),
        with_auth(get_token_balances::GetTokenBalances, AuthLevel::Public),
        with_auth(get_transaction::GetTransaction, AuthLevel::Public),
        with_auth(get_token_price::GetTokenPrice, AuthLevel::Public),
        with_auth(get_token_metadata::GetTokenMetadata, AuthLevel::Public),
        with_auth(sleep::Sleep, AuthLevel::Public),
    ]
}
