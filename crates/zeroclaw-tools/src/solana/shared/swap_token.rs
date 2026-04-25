//! `swap-token` — agent-signed Jupiter swap. SPEC §5.1, §10.4.
//!
//! Authorisation: `AuthLevel::Owner`.
//! Signing: agent keypair (NOT `submit_or_send`) per SPEC §10.4 — Jupiter
//! returns opaque versioned transactions that cannot be re-routed through
//! the public-mode bridge.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

use super::jupiter_swap::{
    SwapPlan, execute_swap, jupiter_client_from_env, max_price_impact_pct_from_env,
    max_slippage_bps_from_env,
};

#[derive(Debug, Deserialize)]
struct Args {
    input_mint: String,
    output_mint: String,
    amount: u64,
    #[serde(default)]
    slippage_bps: Option<u16>,
}

pub struct SwapToken;

#[async_trait]
impl Tool for SwapToken {
    fn name(&self) -> &str {
        "swap-token"
    }

    fn description(&self) -> &str {
        "Swap tokens on Solana via Jupiter. The agent signs the returned \
         transaction with its own keypair after a simulate-and-verify integrity \
         check (SPEC §10.4). Slippage and price-impact caps protect against \
         malicious or stale routes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input_mint": {
                    "type": "string",
                    "description": "Base58-encoded SPL mint to swap from."
                },
                "output_mint": {
                    "type": "string",
                    "description": "Base58-encoded SPL mint to swap to."
                },
                "amount": {
                    "type": "integer",
                    "description": "Raw amount of input mint to swap (atoms — multiply UI amount by 10^decimals).",
                    "minimum": 1
                },
                "slippage_bps": {
                    "type": "integer",
                    "description": "Slippage tolerance in basis points (50 = 0.5%). Defaults to 50; capped at MAX_SLIPPAGE_BPS.",
                    "minimum": 0,
                    "maximum": 10000
                }
            },
            "required": ["input_mint", "output_mint", "amount"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ApiToolResult> {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("invalid arguments: {e}"),
                );
                return Ok(r.into());
            }
        };

        if let Err(e) = Pubkey::from_str(parsed.input_mint.trim()) {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                format!("input_mint is not a valid base58 pubkey: {e}"),
            );
            return Ok(r.into());
        }
        if let Err(e) = Pubkey::from_str(parsed.output_mint.trim()) {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                format!("output_mint is not a valid base58 pubkey: {e}"),
            );
            return Ok(r.into());
        }
        if parsed.amount == 0 {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "amount must be greater than zero",
            );
            return Ok(r.into());
        }
        let max_slippage = max_slippage_bps_from_env();
        if let Some(s) = parsed.slippage_bps
            && s > max_slippage
        {
            let r: ToolResult<()> = err(
                ToolErrorCode::SlippageTooHigh,
                format!("requested slippage {s} bps exceeds cap {max_slippage} bps"),
            );
            return Ok(r.into());
        }

        let ctx = match current_context() {
            Ok(c) => c,
            Err(e) => {
                let r: ToolResult<()> = err(e.code, e.message);
                return Ok(r.into());
            }
        };

        let client = jupiter_client_from_env();
        let plan = SwapPlan {
            input_mint: parsed.input_mint.trim().to_string(),
            output_mint: parsed.output_mint.trim().to_string(),
            amount: parsed.amount,
            slippage_bps: parsed.slippage_bps,
        };
        match execute_swap(
            &ctx,
            &client,
            plan,
            max_slippage,
            max_price_impact_pct_from_env(),
        )
        .await
        {
            Ok(value) => Ok(ok(value).into()),
            Err(e) => {
                let r: ToolResult<()> = err(e.code, e.message);
                Ok(r.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_args() {
        let r = SwapToken.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_zero_amount() {
        let r = SwapToken
            .execute(json!({
                "input_mint": "So11111111111111111111111111111111111111112",
                "output_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "amount": 0,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_input_mint() {
        let r = SwapToken
            .execute(json!({
                "input_mint": "garbage!!",
                "output_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "amount": 1000,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_output_mint() {
        let r = SwapToken
            .execute(json!({
                "input_mint": "So11111111111111111111111111111111111111112",
                "output_mint": "nope",
                "amount": 1000,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_slippage_over_cap() {
        // 600 > 500 bps default cap.
        let r = SwapToken
            .execute(json!({
                "input_mint": "So11111111111111111111111111111111111111112",
                "output_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "amount": 1000,
                "slippage_bps": 600,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("SLIPPAGE_TOO_HIGH"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        let r = SwapToken
            .execute(json!({
                "input_mint": "So11111111111111111111111111111111111111112",
                "output_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "amount": 1000,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
