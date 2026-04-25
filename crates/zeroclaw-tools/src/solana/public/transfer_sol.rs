//! `transfer-sol` — public-mode tool that prompts the connected wallet to
//! transfer SOL to a destination address. SPEC §5.1.
//!
//! Public-only: the source of funds is the user wallet (`wallet_address`),
//! so this tool would have nothing to send in autonomous mode.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::pubkey::Pubkey;
use solana_system_interface::instruction as system_instruction;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::submit_or_send::{SubmitOpts, submit_or_send};
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Debug, Deserialize)]
struct Args {
    destination: String,
    amount_sol: f64,
}

pub struct TransferSol;

#[async_trait]
impl Tool for TransferSol {
    fn name(&self) -> &str {
        "transfer-sol"
    }

    fn description(&self) -> &str {
        "Prompt the connected wallet to transfer SOL to a destination Solana \
         address. The connected wallet pays the network fee plus the agent \
         fee. Public mode only."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "destination": {
                    "type": "string",
                    "description": "Base58-encoded Solana wallet address to receive the SOL."
                },
                "amount_sol": {
                    "type": "number",
                    "description": "Amount of SOL to send. Must be greater than zero."
                }
            },
            "required": ["destination", "amount_sol"]
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

        // Catches NaN, negative, and zero. Using `partial_cmp` per clippy's
        // `neg_cmp_op_on_partial_ord` lint — `!(x > 0.0)` would silently
        // accept NaN.
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

        let dest = match Pubkey::from_str(parsed.destination.trim()) {
            Ok(p) => p,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("destination is not a valid base58 pubkey: {e}"),
                );
                return Ok(r.into());
            }
        };

        let ctx = match current_context() {
            Ok(c) => c,
            Err(e) => {
                let r: ToolResult<()> = err(e.code, e.message);
                return Ok(r.into());
            }
        };

        // Public-only: this tool has no source of funds in autonomous mode.
        let Some(user) = ctx.wallet_address else {
            let r: ToolResult<()> = err(
                ToolErrorCode::Unauthorized,
                "transfer-sol requires a connected wallet (public mode only)",
            );
            return Ok(r.into());
        };

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let lamports = (parsed.amount_sol * LAMPORTS_PER_SOL).round() as u64;
        let ix = system_instruction::transfer(&user, &dest, lamports);

        let opts = SubmitOpts {
            message: Some(format!("Transfer {} SOL", parsed.amount_sol)),
            ..Default::default()
        };
        match submit_or_send(&ctx, vec![ix], opts, &[]).await {
            Ok(sig) => Ok(ok(json!({
                "signature": sig.to_string(),
                "destination": parsed.destination,
                "amount_sol": parsed.amount_sol,
            }))
            .into()),
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
        let r = TransferSol.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_zero_amount() {
        let r = TransferSol
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "amount_sol": 0.0,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_negative_amount() {
        let r = TransferSol
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "amount_sol": -1.0,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_destination() {
        let r = TransferSol
            .execute(json!({
                "destination": "not-a-pubkey",
                "amount_sol": 0.5,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        let r = TransferSol
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "amount_sol": 0.5,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
