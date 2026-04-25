//! Shared Jupiter swap pipeline used by `swap-token`, `buyback-token`, and
//! `sell-token`. Centralises:
//!
//! - the slippage / price-impact caps (SPEC §5.1, §10.4),
//! - the quote → swap → simulate-and-verify → sign-and-send sequence,
//! - the env-var seam for `JUPITER_API_KEY` / `MAX_SLIPPAGE_BPS` /
//!   `MAX_PRICE_IMPACT_PCT`.
//!
//! Tools convert their own input (mints, amounts) into a [`SwapPlan`] and
//! call [`execute_swap`]. The result is a JSON `value` ready to drop into
//! `tool_result::ok(...)`.

use serde_json::{Value, json};
use solana_sdk::signature::Signer;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use zeroclaw_solana::context::AgentContext;
use zeroclaw_solana::error_codes::{ToolError, ToolErrorCode};
use zeroclaw_solana::jupiter::{
    JupiterApi, JupiterClient, QuoteParams, SwapParams, simulate_and_verify_swap,
};

/// Default slippage when the caller didn't specify one (50 bps = 0.5%).
pub const DEFAULT_SLIPPAGE_BPS: u16 = 50;

/// Hard upper bound on accepted slippage (500 bps = 5%). SPEC §5.1.
pub const MAX_SLIPPAGE_BPS_DEFAULT: u16 = 500;

/// Hard upper bound on accepted Jupiter price impact (2.0%). SPEC §5.1.
pub const MAX_PRICE_IMPACT_PCT_DEFAULT: f64 = 2.0;

/// Environment variable that overrides [`MAX_SLIPPAGE_BPS_DEFAULT`].
pub const ENV_MAX_SLIPPAGE_BPS: &str = "MAX_SLIPPAGE_BPS";
/// Environment variable that overrides [`MAX_PRICE_IMPACT_PCT_DEFAULT`].
pub const ENV_MAX_PRICE_IMPACT_PCT: &str = "MAX_PRICE_IMPACT_PCT";
/// Environment variable feeding the Jupiter API key.
pub const ENV_JUPITER_API_KEY: &str = "JUPITER_API_KEY";

/// Caller-provided plan for a single swap.
pub struct SwapPlan {
    pub input_mint: String,
    pub output_mint: String,
    pub amount: u64,
    pub slippage_bps: Option<u16>,
}

/// Read `MAX_SLIPPAGE_BPS` from env, falling back to the default. Invalid
/// values fall through to the default rather than failing closed — the
/// default is the safer cap.
pub fn max_slippage_bps_from_env() -> u16 {
    std::env::var(ENV_MAX_SLIPPAGE_BPS)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(MAX_SLIPPAGE_BPS_DEFAULT)
}

/// Read `MAX_PRICE_IMPACT_PCT` from env, falling back to the default.
pub fn max_price_impact_pct_from_env() -> f64 {
    std::env::var(ENV_MAX_PRICE_IMPACT_PCT)
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(MAX_PRICE_IMPACT_PCT_DEFAULT)
}

/// Build a `JupiterClient` populated with `JUPITER_API_KEY` if set.
pub fn jupiter_client_from_env() -> JupiterClient {
    let key = std::env::var(ENV_JUPITER_API_KEY)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    JupiterClient::new(key)
}

/// Resolve and validate the slippage requested by the caller against the
/// configured upper bound. Returns the bps value to pass to Jupiter, or a
/// classified `ToolError` to propagate.
pub fn resolve_slippage(requested: Option<u16>, max_slippage_bps: u16) -> Result<u16, ToolError> {
    let bps = requested.unwrap_or(DEFAULT_SLIPPAGE_BPS);
    if bps > max_slippage_bps {
        return Err(ToolError {
            code: ToolErrorCode::SlippageTooHigh,
            message: format!("requested slippage {bps} bps exceeds cap {max_slippage_bps} bps",),
        });
    }
    Ok(bps)
}

/// Reject the quote if Jupiter's reported `priceImpactPct` exceeds the cap.
pub fn enforce_price_impact(
    price_impact_pct_str: &str,
    max_price_impact_pct: f64,
) -> Result<f64, ToolError> {
    // Jupiter returns the impact as a fractional decimal (e.g. "0.0123"
    // for 1.23%). Convert to percentage points before comparing.
    let frac = price_impact_pct_str.parse::<f64>().map_err(|_| ToolError {
        code: ToolErrorCode::Integrity,
        message: format!("price_impact_pct not a number: {price_impact_pct_str}"),
    })?;
    let pct = frac * 100.0;
    if !pct.is_finite() {
        return Err(ToolError {
            code: ToolErrorCode::Integrity,
            message: "price_impact_pct is not finite".into(),
        });
    }
    if pct > max_price_impact_pct {
        return Err(ToolError {
            code: ToolErrorCode::PriceImpactTooHigh,
            message: format!("price impact {pct:.4}% exceeds cap {max_price_impact_pct:.4}%",),
        });
    }
    Ok(pct)
}

/// Run the full Jupiter swap pipeline using `client` to talk to Jupiter and
/// `ctx` for both RPC + the agent keypair. Returns a JSON value suitable for
/// `tool_result::ok(...)`. All errors are already classified.
pub async fn execute_swap(
    ctx: &AgentContext,
    client: &dyn JupiterApi,
    plan: SwapPlan,
    max_slippage_bps: u16,
    max_price_impact_pct: f64,
) -> Result<Value, ToolError> {
    if plan.amount == 0 {
        return Err(ToolError {
            code: ToolErrorCode::InvalidInput,
            message: "amount must be greater than zero".into(),
        });
    }

    let slippage_bps = resolve_slippage(plan.slippage_bps, max_slippage_bps)?;

    let agent_keypair: Arc<solana_sdk::signature::Keypair> = ctx
        .agent_keypair
        .as_ref()
        .cloned()
        .ok_or_else(|| ToolError {
            code: ToolErrorCode::Unauthorized,
            message: "agent keypair not configured for this session".into(),
        })?;

    let qp = QuoteParams {
        input_mint: plan.input_mint.clone(),
        output_mint: plan.output_mint.clone(),
        amount: plan.amount,
        slippage_bps,
    };
    let quote = client.quote(&qp).await.map_err(|code| ToolError {
        code,
        message: "jupiter quote request failed".into(),
    })?;

    let pct = enforce_price_impact(&quote.price_impact_pct, max_price_impact_pct)?;

    let user_pk_str = ctx.agent_pubkey.to_string();
    let sp = SwapParams {
        quote_response: &quote,
        user_public_key: user_pk_str.clone(),
        wrap_and_unwrap_sol: true,
        use_shared_accounts: true,
    };
    let swap = client.swap(&sp).await.map_err(|code| ToolError {
        code,
        message: "jupiter swap request failed".into(),
    })?;

    // Decode + integrity check.
    let mut tx: VersionedTransaction = simulate_and_verify_swap(
        &ctx.solana,
        &ctx.agent_keypair
            .as_ref()
            .map(|kp| kp.pubkey())
            .unwrap_or(ctx.agent_pubkey),
        &swap.swap_transaction,
        &qp,
        &quote,
    )
    .await
    .map_err(|code| ToolError {
        code,
        message: "swap integrity check failed".into(),
    })?;

    // Re-sign with the agent keypair against a fresh blockhash. Jupiter's
    // returned tx already has the agent's slot wired up; we just have to
    // produce the signature.
    let blockhash = ctx
        .solana
        .rpc
        .get_latest_blockhash()
        .await
        .map_err(|e| ToolError {
            code: ToolErrorCode::RpcFailure,
            message: format!("get_latest_blockhash failed: {e}"),
        })?;
    tx.message.set_recent_blockhash(blockhash);
    // Build the signed tx in a tight scope so the `&dyn Signer` borrow does
    // not survive across the `send_and_confirm_transaction` await (the trait
    // object is not `Sync`, which would otherwise poison the future).
    let signed = {
        let signers: [&dyn Signer; 1] = [agent_keypair.as_ref()];
        VersionedTransaction::try_new(tx.message, &signers).map_err(|e| ToolError {
            code: ToolErrorCode::Generic,
            message: format!("failed to sign jupiter tx: {e}"),
        })?
    };

    let signature = ctx
        .solana
        .rpc
        .send_and_confirm_transaction(&signed)
        .await
        .map_err(|e| ToolError {
            code: ToolErrorCode::RpcFailure,
            message: format!("send_and_confirm_transaction failed: {e}"),
        })?;

    Ok(json!({
        "signature": signature.to_string(),
        "input_mint": plan.input_mint,
        "output_mint": plan.output_mint,
        "amount_in": plan.amount,
        "expected_min_out": quote.out_amount,
        "slippage_bps": slippage_bps,
        "price_impact_pct": pct,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_slippage_uses_default() {
        let bps = resolve_slippage(None, 500).unwrap();
        assert_eq!(bps, DEFAULT_SLIPPAGE_BPS);
    }

    #[test]
    fn resolve_slippage_accepts_at_cap() {
        let bps = resolve_slippage(Some(500), 500).unwrap();
        assert_eq!(bps, 500);
    }

    #[test]
    fn resolve_slippage_rejects_over_cap() {
        let err = resolve_slippage(Some(501), 500).expect_err("must fail");
        assert_eq!(err.code, ToolErrorCode::SlippageTooHigh);
    }

    #[test]
    fn enforce_price_impact_within_cap() {
        // 0.005 = 0.5% ≤ 2.0%
        let pct = enforce_price_impact("0.005", 2.0).unwrap();
        assert!((pct - 0.5).abs() < 1e-9);
    }

    #[test]
    fn enforce_price_impact_at_cap() {
        let pct = enforce_price_impact("0.02", 2.0).unwrap();
        assert!((pct - 2.0).abs() < 1e-9);
    }

    #[test]
    fn enforce_price_impact_over_cap_rejects() {
        let err = enforce_price_impact("0.03", 2.0).expect_err("over cap");
        assert_eq!(err.code, ToolErrorCode::PriceImpactTooHigh);
    }

    #[test]
    fn enforce_price_impact_unparseable_is_integrity() {
        let err = enforce_price_impact("nan-ish", 2.0).expect_err("bad number");
        assert_eq!(err.code, ToolErrorCode::Integrity);
    }
}
