//! Agent-wallet funding seam (SPEC §6, "register-agent funding flow").
//!
//! `ensure_agent_funded` is called at the top of identity-mutating tools
//! (`register-agent`, `delegate-execution`) so the caller can prompt the
//! connected wallet to fund the agent before the actual on-chain action
//! runs.
//!
//! Constants `AGENT_FUNDING_SOL` (the amount to send) and
//! `AGENT_FUNDING_THRESHOLD_SOL` (the balance below which we top up) match
//! the reference implementation's defaults from `server-limits.ts`.

use crate::context::AgentContext;
use crate::error_codes::{ToolError, ToolErrorCode, truncate_llm_message};
use crate::submit_or_send::{SubmitOpts, submit_or_send};
use solana_system_interface::instruction as system_instruction;
use std::time::Duration;

/// Lamports per SOL — kept local to avoid a cross-module constant pull.
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Amount of SOL transferred from the connected wallet to the agent
/// wallet during a funding flow (default 0.02 SOL — SPEC §6).
pub const AGENT_FUNDING_SOL: f64 = 0.02;

/// Balance threshold below which `ensure_agent_funded` triggers a funding
/// transfer (default 0.01 SOL — SPEC §6).
pub const AGENT_FUNDING_THRESHOLD_SOL: f64 = 0.01;

/// Number of `get_balance` polls after a funding transfer before giving
/// up. Matches the reference flow's "10 × 1s" cadence.
const POLL_ATTEMPTS: u32 = 10;
/// Delay between balance polls.
const POLL_DELAY: Duration = Duration::from_secs(1);

/// Outcome of [`ensure_agent_funded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Funded {
    /// The agent wallet was already at or above the threshold; nothing
    /// changed on-chain.
    AlreadyFunded,
    /// A funding transfer succeeded and the post-transfer balance check
    /// observed the new balance at or above the threshold.
    JustFunded,
}

/// Top up the agent wallet from the connected user wallet if its balance
/// is below [`AGENT_FUNDING_THRESHOLD_SOL`].
///
/// In autonomous mode (no `tx_sender`, no `wallet_address`) there is no
/// user wallet to fund from — the function returns
/// `ToolErrorCode::InsufficientFunds` so the caller can surface it to the
/// LLM.
pub async fn ensure_agent_funded(ctx: &AgentContext) -> Result<Funded, ToolError> {
    let bal = ctx
        .solana
        .rpc
        .get_balance(&ctx.agent_pubkey)
        .await
        .map_err(|e| {
            let raw = e.to_string();
            tracing::error!(target: "zeroclaw_solana::funding", error=%raw, "get_balance failed");
            ToolError {
                code: ToolErrorCode::RpcFailure,
                message: truncate_llm_message(format!("get_balance failed: {raw}")),
            }
        })?;
    #[allow(clippy::cast_precision_loss)]
    let bal_sol = bal as f64 / LAMPORTS_PER_SOL;
    if bal_sol >= AGENT_FUNDING_THRESHOLD_SOL {
        return Ok(Funded::AlreadyFunded);
    }

    // Need to fund — public mode only.
    let (Some(_tx_sender), Some(user)) = (ctx.tx_sender.as_ref(), ctx.wallet_address) else {
        return Err(ToolError {
            code: ToolErrorCode::InsufficientFunds,
            message: truncate_llm_message(format!(
                "Agent wallet {addr} needs {sol} SOL",
                addr = ctx.agent_pubkey,
                sol = AGENT_FUNDING_SOL
            )),
        });
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lamports = (AGENT_FUNDING_SOL * LAMPORTS_PER_SOL).round() as u64;
    let fund_ix = system_instruction::transfer(&user, &ctx.agent_pubkey, lamports);

    // Funding tx has `fee_sol = 0.0` so we don't double-charge.
    let opts = SubmitOpts {
        message: Some("Fund agent wallet".into()),
        fee_sol: Some(0.0),
        ..Default::default()
    };
    submit_or_send(ctx, vec![fund_ix], opts, &[]).await?;

    // Poll balance until the funded amount is visible.
    for _ in 0..POLL_ATTEMPTS {
        if let Ok(post_bal) = ctx.solana.rpc.get_balance(&ctx.agent_pubkey).await {
            #[allow(clippy::cast_precision_loss)]
            let post_sol = post_bal as f64 / LAMPORTS_PER_SOL;
            if post_sol >= AGENT_FUNDING_THRESHOLD_SOL {
                return Ok(Funded::JustFunded);
            }
        }
        tokio::time::sleep(POLL_DELAY).await;
    }
    // The transfer was accepted but the new balance never showed up.
    Err(ToolError {
        code: ToolErrorCode::Timeout,
        message: truncate_llm_message(format!(
            "funded {AGENT_FUNDING_SOL} SOL to agent but balance did not update in time"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::SolanaClient;
    use crate::context::{AgentMode, TxSendError, TxSendOpts, TxSender};
    use async_trait::async_trait;
    use solana_sdk::pubkey::Pubkey;
    use solana_sdk::signature::Signature;
    use std::sync::Arc;

    fn pk(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    struct OkSender;
    #[async_trait]
    impl TxSender for OkSender {
        async fn send_and_await(
            &self,
            _tx_base64: String,
            _opts: TxSendOpts,
        ) -> Result<Signature, TxSendError> {
            Ok(Signature::from([1u8; 64]))
        }
    }

    fn ctx_no_wallet() -> AgentContext {
        AgentContext {
            wallet_address: None,
            agent_mode: AgentMode::Autonomous,
            agent_pubkey: pk(2),
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol: 0.001,
            token_override: None,
            owner_wallet: None,
            tx_sender: None,
            solana: Arc::new(SolanaClient::new("http://127.0.0.1:1")),
        }
    }

    #[tokio::test]
    async fn rpc_unreachable_classifies_as_rpc_failure() {
        // SolanaClient targets a guaranteed-unreachable port → get_balance fails.
        let ctx = ctx_no_wallet();
        let err = ensure_agent_funded(&ctx).await.unwrap_err();
        assert_eq!(err.code, ToolErrorCode::RpcFailure);
        assert!(err.message.contains("get_balance"));
    }

    #[test]
    fn defaults_match_spec() {
        // Hard-coded defaults are part of the contract — pin them here so
        // a refactor that bumps them shows up in CI.
        assert_eq!(AGENT_FUNDING_SOL, 0.02);
        assert_eq!(AGENT_FUNDING_THRESHOLD_SOL, 0.01);
    }

    #[test]
    fn ok_sender_implements_tx_sender_trait() {
        // Compile-time assertion that the test fake satisfies TxSender;
        // future tests can wire it through AgentContext without ceremony.
        fn assert_tx_sender<T: TxSender>(_t: &T) {}
        assert_tx_sender(&OkSender);
    }
}
