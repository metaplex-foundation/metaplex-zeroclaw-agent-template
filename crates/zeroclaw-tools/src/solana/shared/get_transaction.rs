//! `get-transaction` — look up transaction status by signature. SPEC §5.1.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::signature::Signature;
use solana_transaction_status::UiTransactionEncoding;
use std::str::FromStr;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::context::current_context;
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

#[derive(Debug, Deserialize)]
struct Args {
    signature: String,
}

pub struct GetTransaction;

#[async_trait]
impl Tool for GetTransaction {
    fn name(&self) -> &str {
        "get-transaction"
    }

    fn description(&self) -> &str {
        "Look up the status of a Solana transaction by base58 signature. \
         Returns the slot, block time, success flag, and any on-chain error."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "signature": {
                    "type": "string",
                    "description": "Base58-encoded transaction signature."
                }
            },
            "required": ["signature"]
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
        let sig = match Signature::from_str(parsed.signature.trim()) {
            Ok(s) => s,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("signature is not a valid base58 signature: {e}"),
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

        match ctx
            .solana
            .rpc
            .get_transaction(&sig, UiTransactionEncoding::Json)
            .await
        {
            Ok(tx) => {
                let (success, on_chain_err) = match tx.transaction.meta.as_ref() {
                    Some(meta) => {
                        let err_val = meta
                            .err
                            .as_ref()
                            .map(|e| Value::String(format!("{e:?}")))
                            .unwrap_or(Value::Null);
                        (meta.err.is_none(), err_val)
                    }
                    // No meta (extremely rare); treat as unknown — surface what we have.
                    None => (false, Value::String("transaction meta unavailable".into())),
                };
                Ok(ok(json!({
                    "signature": parsed.signature,
                    "slot": tx.slot,
                    "block_time": tx.block_time,
                    "success": success,
                    "error": on_chain_err,
                }))
                .into())
            }
            Err(e) => {
                let raw = e.to_string();
                tracing::error!(target: "zeroclaw_tools::solana::get_transaction", error=%raw, "RPC get_transaction failed");
                // Classify "not found" specifically — common case worth telling the LLM.
                let lc = raw.to_ascii_lowercase();
                let code = if lc.contains("not found") || lc.contains("could not find") {
                    ToolErrorCode::NotFound
                } else {
                    ToolErrorCode::RpcFailure
                };
                let r: ToolResult<()> = err(code, format!("get_transaction RPC failed: {raw}"));
                Ok(r.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_signature() {
        let r = GetTransaction.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_signature() {
        let r = GetTransaction
            .execute(json!({ "signature": "not-a-sig" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn errors_when_no_agent_context() {
        // 64-byte all-zero signature serializes to a 88-char base58 string
        // (Signature::default()) — passes parsing, then trips current_context.
        let valid_sig = Signature::default().to_string();
        let r = GetTransaction
            .execute(json!({ "signature": valid_sig }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
    }
}
