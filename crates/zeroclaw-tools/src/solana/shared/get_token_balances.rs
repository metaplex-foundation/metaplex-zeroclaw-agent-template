//! `get-token-balances` — list SPL token holdings for any wallet. SPEC §5.1.
//!
//! Calls `getTokenAccountsByOwner` with `JsonParsed` encoding so the parsed
//! `info.mint` / `info.tokenAmount` fields are available without needing to
//! deserialize raw token account state. Token-2022 balances are not yet
//! returned — the spec only requires SPL Token (the canonical program) for
//! PR 3; Token-2022 will be added in PR 4 when transfer tools land.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_client::rpc_request::TokenAccountsFilter;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

/// SPL Token program ID. Hardcoded so we don't drag in the heavy `spl-token`
/// crate just for one constant.
const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

#[derive(Debug, Deserialize)]
struct Args {
    address: String,
}

pub struct GetTokenBalances;

#[async_trait]
impl Tool for GetTokenBalances {
    fn name(&self) -> &str {
        "get-token-balances"
    }

    fn description(&self) -> &str {
        "List the SPL token holdings (mint, raw amount, decimals, ui amount) \
         for any Solana wallet address. Token-2022 holdings are not included \
         in this PR; planned for the write-tool PR."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "string",
                    "description": "Base58-encoded Solana wallet address whose SPL token holdings to enumerate."
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
        let owner = match Pubkey::from_str(parsed.address.trim()) {
            Ok(p) => p,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("address is not a valid base58 pubkey: {e}"),
                );
                return Ok(r.into());
            }
        };
        let token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM_ID)
            .expect("hardcoded SPL token program ID is valid");

        let ctx = match current_context() {
            Ok(c) => c,
            Err(e) => {
                let r: ToolResult<()> = err(e.code, e.message);
                return Ok(r.into());
            }
        };

        let accounts = match ctx
            .solana
            .rpc
            .get_token_accounts_by_owner(&owner, TokenAccountsFilter::ProgramId(token_program))
            .await
        {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(target: "zeroclaw_tools::solana::get_token_balances", error=%e, "RPC get_token_accounts_by_owner failed");
                let r: ToolResult<()> = err(
                    ToolErrorCode::RpcFailure,
                    format!("get_token_accounts_by_owner RPC failed: {e}"),
                );
                return Ok(r.into());
            }
        };

        let mut tokens: Vec<Value> = Vec::with_capacity(accounts.len());
        for keyed in accounts {
            // JsonParsed encoding: account.data serializes (untagged) to
            // { program: "spl-token", parsed: { type: "account", info: { mint, owner, tokenAmount: {...} } }, space }
            // Re-roundtrip through serde_json so we don't have to depend on
            // solana_account_decoder_client_types directly — the inner enum
            // isn't re-exported by solana_client.
            let data_json = match serde_json::to_value(&keyed.account.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let parsed = data_json.get("parsed").cloned().unwrap_or(Value::Null);
            let info = parsed.get("info").cloned().unwrap_or(Value::Null);
            let mint = info.get("mint").and_then(Value::as_str).map(str::to_owned);
            let amount = info
                .get("tokenAmount")
                .and_then(|t| t.get("amount"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let decimals = info
                .get("tokenAmount")
                .and_then(|t| t.get("decimals"))
                .and_then(Value::as_u64);
            let ui_amount = info
                .get("tokenAmount")
                .and_then(|t| t.get("uiAmount"))
                .and_then(Value::as_f64);
            let Some(mint) = mint else { continue };
            tokens.push(json!({
                "mint": mint,
                "amount": amount,
                "decimals": decimals,
                "ui_amount": ui_amount,
                "account": keyed.pubkey,
            }));
        }

        Ok(ok(json!({
            "address": parsed.address,
            "tokens": tokens,
            "count": tokens.len(),
        }))
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_address() {
        let r = GetTokenBalances.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_address() {
        let r = GetTokenBalances
            .execute(json!({ "address": "bad!!" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        let valid = "11111111111111111111111111111111";
        let r = GetTokenBalances
            .execute(json!({ "address": valid }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
