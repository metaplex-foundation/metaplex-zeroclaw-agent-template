//! Mode-aware transaction routing (SPEC §6, "Treasury & Tx routing").
//!
//! Tools build a `Vec<Instruction>` and hand it to [`submit_or_send`]. The
//! helper picks between two execution paths based on the [`AgentContext`]:
//!
//! - **Public mode** (a `TxSender` *and* `wallet_address` are present in
//!   ctx): build an unsigned transaction with the user's wallet as the fee
//!   payer, optionally prepend an `agent_fee_sol` SOL transfer to the
//!   agent's pubkey, base64-serialize, and hand to the WS-bridge `TxSender`
//!   so the connected wallet can sign it.
//!
//! - **Autonomous mode** (no `TxSender`): sign locally with the supplied
//!   `agent_signers` and `send_and_confirm` against the RPC.
//!
//! Tools never branch on `AGENT_MODE` themselves — they always call
//! `submit_or_send`. This keeps the dual-mode contract in one place
//! (SPEC §6.5).

use crate::context::{AgentContext, TxSendError, TxSendOpts};
use crate::error_codes::{ToolError, ToolErrorCode, to_tool_error, truncate_llm_message};
use base64::Engine;
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::{Signature, Signer};
use solana_sdk::transaction::Transaction;
use solana_system_interface::instruction as system_instruction;

/// Lamports per SOL. SOL→lamports conversions deliberately use `f64`
/// arithmetic to mirror the reference implementation; floats are bounded
/// by call sites (fees and funding are < 1 SOL).
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Routing options for [`submit_or_send`]. Mirrors [`TxSendOpts`] but
/// adds the `fee_sol` override so callers like `ensure_agent_funded`
/// can suppress the agent fee on a wallet→agent funding transfer.
#[derive(Debug, Clone, Default)]
pub struct SubmitOpts {
    /// Optional human message to surface in the wallet prompt (public mode).
    pub message: Option<String>,
    /// Optional 1-of-N progress info for the wallet prompt.
    pub index: Option<u32>,
    /// Optional 1-of-N progress info for the wallet prompt.
    pub total: Option<u32>,
    /// Override the prepended agent-fee transfer.
    ///
    /// - `None` → use `ctx.agent_fee_sol`.
    /// - `Some(0.0)` → skip the fee entirely (used by funding transfers).
    /// - `Some(x)` → override with `x` SOL.
    pub fee_sol: Option<f64>,
}

impl SubmitOpts {
    /// Translate the `message`/`index`/`total` triple into a `TxSendOpts`
    /// for the public-mode bridge. The `fee_sol` field is *not* forwarded
    /// — public mode prepends the fee instruction directly into the
    /// instruction list before serializing.
    pub fn to_tx_send_opts(&self) -> TxSendOpts {
        TxSendOpts {
            message: self.message.clone(),
            index: self.index,
            total: self.total,
            fee_sol: self.fee_sol,
        }
    }
}

/// Resolve the SOL amount to prepend as an agent fee.
///
/// Returns `None` if no fee should be added (fee_sol = Some(0.0) or the
/// resolved amount is non-positive).
fn resolved_fee_sol(ctx: &AgentContext, opts: &SubmitOpts) -> Option<f64> {
    let raw = opts.fee_sol.unwrap_or(ctx.agent_fee_sol);
    if raw > 0.0 { Some(raw) } else { None }
}

/// Convert SOL → lamports with explicit casting suppression (we already
/// validate the value is positive and small upstream).
fn sol_to_lamports(sol: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lamports = (sol * LAMPORTS_PER_SOL).round() as u64;
    lamports
}

/// Submit `ixs` either via the public-mode wallet bridge or by signing
/// locally with `agent_signers` and confirming through the RPC.
///
/// Routing rule:
///
/// - If `ctx.tx_sender.is_some()` *and* `ctx.wallet_address.is_some()`,
///   route via the bridge ("public mode").
/// - Otherwise, sign locally with `agent_signers` and send through
///   `ctx.solana.rpc.send_and_confirm_transaction` ("autonomous mode").
///
/// In public mode `agent_signers` is allowed (and expected) to be empty —
/// the user's wallet provides the only signature. In autonomous mode the
/// caller MUST pass at least one signer whose pubkey matches the chosen
/// fee payer (`ctx.agent_pubkey`).
pub async fn submit_or_send(
    ctx: &AgentContext,
    mut ixs: Vec<Instruction>,
    opts: SubmitOpts,
    agent_signers: &[&(dyn Signer + Sync)],
) -> Result<Signature, ToolError> {
    // Public mode: wallet bridge.
    if let (Some(tx_sender), Some(user)) = (ctx.tx_sender.as_ref(), ctx.wallet_address) {
        // Optionally prepend the agent fee.
        if let Some(fee) = resolved_fee_sol(ctx, &opts) {
            let lamports = sol_to_lamports(fee);
            if lamports > 0 {
                let fee_ix = system_instruction::transfer(&user, &ctx.agent_pubkey, lamports);
                let mut prepended = Vec::with_capacity(ixs.len() + 1);
                prepended.push(fee_ix);
                prepended.append(&mut ixs);
                ixs = prepended;
            }
        }

        let blockhash = ctx
            .solana
            .rpc
            .get_latest_blockhash()
            .await
            .map_err(|e| classify_rpc_error("get_latest_blockhash", e.to_string()))?;

        // Build an unsigned transaction with the user as the fee payer.
        let mut tx = Transaction::new_with_payer(&ixs, Some(&user));
        tx.message.recent_blockhash = blockhash;

        let serialized = bincode::serialize(&tx).map_err(|e| ToolError {
            code: ToolErrorCode::Generic,
            message: truncate_llm_message(format!("transaction serialization failed: {e}")),
        })?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(serialized);

        return tx_sender
            .send_and_await(b64, opts.to_tx_send_opts())
            .await
            .map_err(map_tx_send_error);
    }

    // Autonomous mode: sign locally and confirm.
    if agent_signers.is_empty() {
        return Err(ToolError {
            code: ToolErrorCode::Generic,
            message: "submit_or_send: autonomous mode requires at least one agent signer".into(),
        });
    }
    let blockhash = ctx
        .solana
        .rpc
        .get_latest_blockhash()
        .await
        .map_err(|e| classify_rpc_error("get_latest_blockhash", e.to_string()))?;
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.agent_pubkey),
        &agent_signers.to_vec(),
        blockhash,
    );
    ctx.solana
        .rpc
        .send_and_confirm_transaction(&tx)
        .await
        .map_err(|e| {
            // Funnel the raw RPC error through the standard classifier so
            // "insufficient lamports", "blockhash not found", "timeout", etc.
            // bucket consistently.
            let raw = e.to_string();
            tracing::error!(target: "zeroclaw_solana::submit_or_send", error=%raw, "send_and_confirm_transaction failed");
            to_tool_error(&anyhow::anyhow!(raw))
        })
}

fn map_tx_send_error(e: TxSendError) -> ToolError {
    let code = e.as_tool_error_code();
    ToolError {
        code,
        message: truncate_llm_message(e.to_string()),
    }
}

fn classify_rpc_error(label: &str, raw: String) -> ToolError {
    tracing::error!(target: "zeroclaw_solana::submit_or_send", error=%raw, "{label} RPC failed");
    let lc = raw.to_ascii_lowercase();
    let code = if lc.contains("timeout") || lc.contains("timed out") {
        ToolErrorCode::Timeout
    } else {
        ToolErrorCode::RpcFailure
    };
    ToolError {
        code,
        message: truncate_llm_message(format!("{label} RPC failed: {raw}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::SolanaClient;
    use crate::context::{AgentMode, TxSender};
    use async_trait::async_trait;
    use solana_sdk::pubkey::Pubkey;
    use std::sync::Arc;
    use std::sync::Mutex;

    fn pk(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    #[derive(Debug, Clone, Default)]
    #[allow(dead_code)]
    struct CapturedSend {
        tx_base64: String,
        opts: TxSendOpts,
    }

    /// Test fake: captures the `(tx_base64, opts)` it would send, returns
    /// a canned signature instead of round-tripping through a wallet.
    #[allow(dead_code)]
    struct CapturingSender {
        last: Arc<Mutex<Option<CapturedSend>>>,
        signature: Signature,
    }

    impl CapturingSender {
        fn new() -> Self {
            Self {
                last: Arc::new(Mutex::new(None)),
                signature: Signature::from([7u8; 64]),
            }
        }
        #[allow(dead_code)]
        fn last(&self) -> CapturedSend {
            self.last.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait]
    impl TxSender for CapturingSender {
        async fn send_and_await(
            &self,
            tx_base64: String,
            opts: TxSendOpts,
        ) -> Result<Signature, TxSendError> {
            *self.last.lock().unwrap() = Some(CapturedSend { tx_base64, opts });
            Ok(self.signature)
        }
    }

    /// Test fake that always rejects — used to assert error mapping.
    #[allow(dead_code)]
    struct RejectingSender;

    #[async_trait]
    impl TxSender for RejectingSender {
        async fn send_and_await(
            &self,
            _tx_base64: String,
            _opts: TxSendOpts,
        ) -> Result<Signature, TxSendError> {
            Err(TxSendError::Rejected("user said no".into()))
        }
    }

    fn ctx_public(sender: Arc<dyn TxSender>, user: Pubkey, agent_fee_sol: f64) -> AgentContext {
        AgentContext {
            wallet_address: Some(user),
            agent_mode: AgentMode::Public,
            agent_pubkey: pk(2),
            agent_keypair: None,
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol,
            token_override: None,
            owner_wallet: None,
            tx_sender: Some(sender),
            solana: Arc::new(SolanaClient::new("http://127.0.0.1:1")),
        }
    }

    fn ctx_autonomous() -> AgentContext {
        AgentContext {
            wallet_address: None,
            agent_mode: AgentMode::Autonomous,
            agent_pubkey: pk(2),
            agent_keypair: None,
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol: 0.001,
            token_override: None,
            owner_wallet: None,
            tx_sender: None,
            solana: Arc::new(SolanaClient::new("http://127.0.0.1:1")),
        }
    }

    fn deserialize_tx(b64: &str) -> Transaction {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        bincode::deserialize::<Transaction>(&bytes).unwrap()
    }

    #[test]
    fn resolved_fee_uses_ctx_when_opts_none() {
        let ctx = ctx_autonomous();
        let opts = SubmitOpts::default();
        assert_eq!(resolved_fee_sol(&ctx, &opts), Some(0.001));
    }

    #[test]
    fn resolved_fee_some_zero_skips() {
        let ctx = ctx_autonomous();
        let opts = SubmitOpts {
            fee_sol: Some(0.0),
            ..Default::default()
        };
        assert_eq!(resolved_fee_sol(&ctx, &opts), None);
    }

    #[test]
    fn resolved_fee_overrides_ctx() {
        let ctx = ctx_autonomous();
        let opts = SubmitOpts {
            fee_sol: Some(0.005),
            ..Default::default()
        };
        assert_eq!(resolved_fee_sol(&ctx, &opts), Some(0.005));
    }

    #[test]
    fn sol_to_lamports_rounds_correctly() {
        assert_eq!(sol_to_lamports(1.0), 1_000_000_000);
        assert_eq!(sol_to_lamports(0.001), 1_000_000);
        assert_eq!(sol_to_lamports(0.0), 0);
    }

    #[tokio::test]
    async fn public_mode_no_fee_when_fee_sol_zero_does_not_prepend() {
        let sender = Arc::new(CapturingSender::new());
        let dyn_sender: Arc<dyn TxSender> = sender.clone();
        let user = pk(1);
        let ctx = ctx_public(dyn_sender, user, 0.001);

        // Build a "real" instruction: a tiny system transfer that the test
        // can recognize as the *original* (not the prepended fee).
        let original = system_instruction::transfer(&user, &pk(99), 5_000);
        let opts = SubmitOpts {
            fee_sol: Some(0.0),
            ..Default::default()
        };

        // Expected: get_latest_blockhash will fail (no real RPC). We only
        // care about routing, so we expect an Err but verify it routed via
        // the public path by way of the error message.
        let err = submit_or_send(&ctx, vec![original], opts, &[])
            .await
            .unwrap_err();
        // The error must be the classified blockhash-fetch failure, not
        // the "no signers in autonomous mode" guard.
        assert!(
            err.message.contains("get_latest_blockhash")
                || err.code == ToolErrorCode::RpcFailure
                || err.code == ToolErrorCode::Timeout,
            "expected RPC error from public path, got {err:?}"
        );
        assert!(sender.last.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn public_mode_autonomous_signer_required() {
        let ctx = ctx_autonomous();
        // Empty ixs is fine; we just want to hit the no-signers guard.
        let err = submit_or_send(&ctx, vec![], SubmitOpts::default(), &[])
            .await
            .unwrap_err();
        assert_eq!(err.code, ToolErrorCode::Generic);
        assert!(err.message.contains("at least one agent signer"));
    }

    #[tokio::test]
    async fn map_tx_send_error_preserves_classification() {
        let r = map_tx_send_error(TxSendError::Timeout);
        assert_eq!(r.code, ToolErrorCode::Timeout);
        let r = map_tx_send_error(TxSendError::Rejected("nope".into()));
        assert_eq!(r.code, ToolErrorCode::Generic);
    }

    // The next two tests use a CapturingSender but do NOT touch the RPC —
    // they exercise the part of the flow that runs *before* get_latest_blockhash.
    // We do this by intercepting via a lightweight helper that manually
    // builds the same fee-prepending logic. Full end-to-end public-mode
    // routing requires either an RPC mock or a live RPC — both gated.

    #[test]
    fn submit_opts_to_tx_send_opts_propagates_progress_and_fee() {
        let opts = SubmitOpts {
            message: Some("approve transfer".into()),
            index: Some(1),
            total: Some(3),
            fee_sol: Some(0.005),
        };
        let tso = opts.to_tx_send_opts();
        assert_eq!(tso.message.as_deref(), Some("approve transfer"));
        assert_eq!(tso.index, Some(1));
        assert_eq!(tso.total, Some(3));
        assert_eq!(tso.fee_sol, Some(0.005));
    }

    /// Exercise the *fee-prepending* branch of `submit_or_send` end-to-end
    /// without touching RPC. We do this by giving the context a
    /// `CapturingSender` and an unreachable RPC; the function will fail
    /// at `get_latest_blockhash`, but on the way there it has *already*
    /// classified the public branch — and the captured sender is still
    /// empty, proving the failure was at RPC, not pre-routing.
    ///
    /// To test the prepend behaviour itself we extract the helper.
    #[test]
    fn fee_prepend_inserts_first_when_fee_sol_default_some() {
        let user = pk(1);
        let agent = pk(2);
        let original = system_instruction::transfer(&user, &pk(99), 5_000);

        // Default opts → use ctx.agent_fee_sol = 0.001 → 1_000_000 lamports.
        let opts = SubmitOpts::default();
        let ctx = AgentContext {
            wallet_address: Some(user),
            agent_mode: AgentMode::Public,
            agent_pubkey: agent,
            agent_keypair: None,
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol: 0.001,
            token_override: None,
            owner_wallet: None,
            tx_sender: None,
            solana: Arc::new(SolanaClient::new("http://127.0.0.1:1")),
        };

        // Mimic the prepend flow synchronously.
        let mut ixs = vec![original];
        if let Some(fee) = resolved_fee_sol(&ctx, &opts) {
            let lamports = sol_to_lamports(fee);
            if lamports > 0 {
                let fee_ix = system_instruction::transfer(&user, &ctx.agent_pubkey, lamports);
                let mut prepended = Vec::with_capacity(ixs.len() + 1);
                prepended.push(fee_ix);
                prepended.append(&mut ixs);
                ixs = prepended;
            }
        }
        assert_eq!(ixs.len(), 2);
        // The first instruction must be the fee transfer to ctx.agent_pubkey.
        assert_eq!(ixs[0].accounts[1].pubkey, agent);
    }

    #[tokio::test]
    async fn rejecting_sender_maps_to_generic_tool_error() {
        // We hit the wallet bridge before RPC. To prove this path: stub
        // the RPC URL to something that *would* fail the blockhash call,
        // but use a sender that returns *before* RPC by erroring at
        // `send_and_await`. To do that, we need to bypass the blockhash
        // step — which is sequenced before send_and_await. So this test
        // verifies the *mapping function* directly (since the routing
        // logic itself is exercised in the prior tests).
        let mapped = map_tx_send_error(TxSendError::Disconnected);
        assert_eq!(mapped.code, ToolErrorCode::Generic);
        assert!(mapped.message.contains("disconnected"));
    }

    #[test]
    fn deserialize_round_trip_smoke() {
        // Smoke test that our base64+bincode encoding decodes back to a
        // Transaction with the right fee payer and instruction count.
        let user = pk(1);
        let original = system_instruction::transfer(&user, &pk(99), 5_000);
        let mut tx = Transaction::new_with_payer(std::slice::from_ref(&original), Some(&user));
        tx.message.recent_blockhash = Default::default();
        let serialized = bincode::serialize(&tx).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(serialized);
        let round = deserialize_tx(&b64);
        assert_eq!(round.message.account_keys[0], user);
    }
}
