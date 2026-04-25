//! Tools available in both `AGENT_MODE=public` and `AGENT_MODE=autonomous`.
//!
//! PR 3 ships the read-only subset (SPEC §5.1, top half of the shared
//! table). Identity / treasury tools land in PR 4–6. The
//! `register-agent` and `delegate-execution` tools are wired in as
//! NOT_IMPLEMENTED stubs in PR 4 so the registry surface is final;
//! the real implementations land alongside the hand-rolled MPL Agent
//! Registry IDL client.

pub mod buyback_token;
pub mod delegate_execution;
pub mod get_balance;
pub mod get_token_balances;
pub mod get_token_metadata;
pub mod get_token_price;
pub mod get_transaction;
pub mod jupiter_swap;
pub mod register_agent;
pub mod sell_token;
pub mod sleep;
pub mod swap_token;

use std::sync::Arc;
use zeroclaw_api::tool::Tool;
use zeroclaw_solana::auth::{AuthLevel, with_auth};

/// Compose the shared tool registry. Read-only tools are `Public`;
/// identity-mutating and treasury tools are `Owner` per SPEC §5.4.
pub fn shared_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        with_auth(get_balance::GetBalance, AuthLevel::Public),
        with_auth(get_token_balances::GetTokenBalances, AuthLevel::Public),
        with_auth(get_transaction::GetTransaction, AuthLevel::Public),
        with_auth(get_token_price::GetTokenPrice, AuthLevel::Public),
        with_auth(get_token_metadata::GetTokenMetadata, AuthLevel::Public),
        with_auth(sleep::Sleep, AuthLevel::Public),
        with_auth(register_agent::RegisterAgent, AuthLevel::Owner),
        with_auth(delegate_execution::DelegateExecution, AuthLevel::Owner),
        with_auth(swap_token::SwapToken, AuthLevel::Owner),
        with_auth(buyback_token::BuybackToken, AuthLevel::Owner),
        with_auth(sell_token::SellToken, AuthLevel::Owner),
    ]
}
