//! `get-balance` — return the SOL balance for any wallet address. SPEC §5.1.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Debug, Deserialize)]
struct Args {
    address: String,
}

pub struct GetBalance;

#[async_trait]
impl Tool for GetBalance {
    fn name(&self) -> &str {
        "get-balance"
    }

    fn description(&self) -> &str {
        "Look up the SOL balance for any Solana wallet address. Returns SOL \
         (human-friendly float) and the raw lamport count."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "string",
                    "description": "Base58-encoded Solana wallet address."
                }
            },
            "required": ["address"]
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
        let pubkey = match Pubkey::from_str(parsed.address.trim()) {
            Ok(p) => p,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("address is not a valid base58 pubkey: {e}"),
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
        match ctx.solana.rpc.get_balance(&pubkey).await {
            Ok(lamports) => {
                #[allow(clippy::cast_precision_loss)]
                let sol = lamports as f64 / LAMPORTS_PER_SOL;
                Ok(ok(json!({
                    "sol": sol,
                    "lamports": lamports,
                    "address": parsed.address,
                }))
                .into())
            }
            Err(e) => {
                tracing::error!(target: "zeroclaw_tools::solana::get_balance", error=%e, "RPC get_balance failed");
                let r: ToolResult<()> = err(
                    ToolErrorCode::RpcFailure,
                    format!("get_balance RPC failed: {e}"),
                );
                Ok(r.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_address() {
        let r = GetBalance.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_address() {
        let r = GetBalance
            .execute(json!({ "address": "notapubkey!!" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        // Valid address — passes parsing — then current_context() fails.
        let valid = "11111111111111111111111111111111";
        let r = GetBalance
            .execute(json!({ "address": valid }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
        assert!(r.output.contains("AGENT_CTX") || r.output.contains("agent context"));
    }
}
