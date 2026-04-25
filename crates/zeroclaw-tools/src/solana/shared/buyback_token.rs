//! `buyback-token` — agent-signed Jupiter buy of the agent's own token
//! (or `TOKEN_OVERRIDE`). SPEC §5.1 / §10.4.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

use super::jupiter_swap::{
    SwapPlan, execute_swap, jupiter_client_from_env, max_price_impact_pct_from_env,
    max_slippage_bps_from_env,
};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Debug, Deserialize)]
struct Args {
    amount_sol: f64,
    #[serde(default)]
    slippage_bps: Option<u16>,
}

pub struct BuybackToken;

#[async_trait]
impl Tool for BuybackToken {
    fn name(&self) -> &str {
        "buyback-token"
    }

    fn description(&self) -> &str {
        "Buy the agent's own token (or TOKEN_OVERRIDE) with SOL via Jupiter. \
         The output mint is resolved from `token_override` (env) or the \
         agent's launched token mint. Agent-signed (SPEC §10.4)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "amount_sol": {
                    "type": "number",
                    "description": "Amount of SOL to spend on the buy. Must be greater than zero."
                },
                "slippage_bps": {
                    "type": "integer",
                    "description": "Slippage tolerance in basis points (50 = 0.5%). Defaults to 50; capped at MAX_SLIPPAGE_BPS.",
                    "minimum": 0,
                    "maximum": 10000
                }
            },
            "required": ["amount_sol"]
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

        let amount_ok = matches!(
            parsed.amount_sol.partial_cmp(&0.0),
            Some(std::cmp::Ordering::Greater)
        );
        if !amount_ok {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "amount_sol must be a positive number",
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

        let target = ctx.token_override.or(ctx.agent_token_mint);
        let Some(target_mint) = target else {
            let r: ToolResult<()> = err(
                ToolErrorCode::NoToken,
                "no buyback target: set TOKEN_OVERRIDE or launch the agent token first",
            );
            return Ok(r.into());
        };

        // SOL → atoms (lamports).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let lamports = (parsed.amount_sol * LAMPORTS_PER_SOL).round() as u64;
        if lamports == 0 {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "amount_sol rounds to zero lamports",
            );
            return Ok(r.into());
        }

        let client = jupiter_client_from_env();
        let plan = SwapPlan {
            input_mint: spl_token::native_mint::id().to_string(),
            output_mint: target_mint.to_string(),
            amount: lamports,
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
        let r = BuybackToken.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_zero_amount() {
        let r = BuybackToken
            .execute(json!({ "amount_sol": 0.0 }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_negative_amount() {
        let r = BuybackToken
            .execute(json!({ "amount_sol": -1.0 }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_slippage_over_cap() {
        let r = BuybackToken
            .execute(json!({
                "amount_sol": 0.5,
                "slippage_bps": 600,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("SLIPPAGE_TOO_HIGH"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        let r = BuybackToken
            .execute(json!({ "amount_sol": 0.5 }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
