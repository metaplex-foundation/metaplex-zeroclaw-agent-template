//! Public-mode-only Solana tools (SPEC §5.1).
//!
//! Tools whose source-of-funds is the connected user wallet
//! (`wallet_address`). They have no equivalent in autonomous mode and so
//! never appear in the autonomous tool registry.

pub mod transfer_sol;
pub mod transfer_token;

use std::sync::Arc;
use zeroclaw_api::tool::Tool;
use zeroclaw_solana::auth::{AuthLevel, with_auth};

pub fn public_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        with_auth(transfer_sol::TransferSol, AuthLevel::Public),
        with_auth(transfer_token::TransferToken, AuthLevel::Public),
    ]
}
