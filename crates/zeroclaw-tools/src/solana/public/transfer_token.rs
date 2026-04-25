//! `transfer-token` — public-mode tool that prompts the connected wallet to
//! transfer SPL tokens to a destination wallet. SPEC §5.1.
//!
//! Public-only: the source ATA is owned by `wallet_address`, so this tool
//! has nothing to send in autonomous mode.
//!
//! Routing details:
//! - Source ATA = `get_associated_token_address(user, mint)`.
//! - Destination ATA = `get_associated_token_address(dest, mint)`.
//! - If the destination ATA does not exist on-chain, prepend a
//!   `create_associated_token_account_idempotent` instruction so the
//!   transfer cannot fail with `AccountNotInitialized`.
//! - Use `transfer_checked` (not `transfer`) so the on-chain program
//!   verifies the decimals against the mint — this guards against
//!   silently wrong-amount transfers caused by a bad off-chain
//!   `decimals` cache.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::instruction::Instruction;
use solana_sdk::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::submit_or_send::{SubmitOpts, submit_or_send};
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

#[derive(Debug, Deserialize)]
struct Args {
    destination: String,
    mint: String,
    amount: u64,
    #[serde(default)]
    decimals: Option<u8>,
}

pub struct TransferToken;

#[async_trait]
impl Tool for TransferToken {
    fn name(&self) -> &str {
        "transfer-token"
    }

    fn description(&self) -> &str {
        "Prompt the connected wallet to transfer SPL tokens to a destination \
         Solana wallet. Creates the destination's associated token account \
         idempotently if it doesn't exist. Public mode only. Pass `decimals` \
         to skip a mint lookup; otherwise it's fetched from the chain."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "destination": {
                    "type": "string",
                    "description": "Base58-encoded Solana wallet address to receive the tokens."
                },
                "mint": {
                    "type": "string",
                    "description": "Base58-encoded SPL token mint."
                },
                "amount": {
                    "type": "integer",
                    "description": "Raw amount of tokens to send (atoms — multiply UI amount by 10^decimals).",
                    "minimum": 1
                },
                "decimals": {
                    "type": "integer",
                    "description": "Optional. Decimals for the mint. Fetched from the chain if omitted.",
                    "minimum": 0,
                    "maximum": 18
                }
            },
            "required": ["destination", "mint", "amount"]
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

        if parsed.amount == 0 {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "amount must be greater than zero",
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
        let mint = match Pubkey::from_str(parsed.mint.trim()) {
            Ok(p) => p,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("mint is not a valid base58 pubkey: {e}"),
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

        let Some(user) = ctx.wallet_address else {
            let r: ToolResult<()> = err(
                ToolErrorCode::Unauthorized,
                "transfer-token requires a connected wallet (public mode only)",
            );
            return Ok(r.into());
        };

        let token_program = spl_token::ID;
        let source_ata = get_associated_token_address(&user, &mint);
        let dest_ata = get_associated_token_address(&dest, &mint);

        // Resolve decimals. Prefer the caller-supplied value (cheap), fall
        // back to fetching the mint account.
        let decimals = match parsed.decimals {
            Some(d) => d,
            None => match fetch_mint_decimals(&ctx, &mint).await {
                Ok(d) => d,
                Err(e) => {
                    let r: ToolResult<()> = err(e.code, e.message);
                    return Ok(r.into());
                }
            },
        };

        // Optionally prepend ATA creation. We probe the destination ATA;
        // an error here is treated as "missing" and we always prepend the
        // idempotent create — it's a no-op if the account already exists.
        let mut ixs: Vec<Instruction> = Vec::with_capacity(2);
        let dest_exists = ctx
            .solana
            .rpc
            .get_account(&dest_ata)
            .await
            .map(|_| true)
            .unwrap_or(false);
        if !dest_exists {
            ixs.push(create_associated_token_account_idempotent(
                &user,
                &dest,
                &mint,
                &token_program,
            ));
        }

        let transfer_ix = match spl_token::instruction::transfer_checked(
            &token_program,
            &source_ata,
            &mint,
            &dest_ata,
            &user,
            &[],
            parsed.amount,
            decimals,
        ) {
            Ok(ix) => ix,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::Generic,
                    format!("failed to build transfer_checked instruction: {e}"),
                );
                return Ok(r.into());
            }
        };
        ixs.push(transfer_ix);

        let opts = SubmitOpts {
            message: Some(format!(
                "Transfer {} tokens of {}",
                parsed.amount, parsed.mint
            )),
            ..Default::default()
        };
        match submit_or_send(&ctx, ixs, opts, &[]).await {
            Ok(sig) => Ok(ok(json!({
                "signature": sig.to_string(),
                "destination": parsed.destination,
                "mint": parsed.mint,
                "amount": parsed.amount,
                "decimals": decimals,
                "created_destination_ata": !dest_exists,
            }))
            .into()),
            Err(e) => {
                let r: ToolResult<()> = err(e.code, e.message);
                Ok(r.into())
            }
        }
    }
}

/// Fetch a mint account and return its `decimals` byte.
async fn fetch_mint_decimals(
    ctx: &zeroclaw_solana::AgentContext,
    mint: &Pubkey,
) -> Result<u8, zeroclaw_solana::ToolError> {
    let acct = ctx.solana.rpc.get_account(mint).await.map_err(|e| {
        let raw = e.to_string();
        let lc = raw.to_ascii_lowercase();
        let code = if lc.contains("not found") || lc.contains("could not find") {
            ToolErrorCode::NotFound
        } else {
            ToolErrorCode::RpcFailure
        };
        zeroclaw_solana::ToolError {
            code,
            message: format!("could not load mint {mint}: {raw}"),
        }
    })?;
    let mint_state =
        spl_token::state::Mint::unpack(&acct.data).map_err(|e| zeroclaw_solana::ToolError {
            code: ToolErrorCode::Integrity,
            message: format!("invalid mint data for {mint}: {e}"),
        })?;
    Ok(mint_state.decimals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_args() {
        let r = TransferToken.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_zero_amount() {
        let r = TransferToken
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "mint": "So11111111111111111111111111111111111111112",
                "amount": 0,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_destination() {
        let r = TransferToken
            .execute(json!({
                "destination": "not-a-pubkey",
                "mint": "So11111111111111111111111111111111111111112",
                "amount": 100,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_mint() {
        let r = TransferToken
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "mint": "not-a-mint",
                "amount": 100,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        let r = TransferToken
            .execute(json!({
                "destination": "11111111111111111111111111111111",
                "mint": "So11111111111111111111111111111111111111112",
                "amount": 100,
                "decimals": 9,
            }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
