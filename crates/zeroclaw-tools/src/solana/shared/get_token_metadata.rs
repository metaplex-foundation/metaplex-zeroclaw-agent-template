//! `get-token-metadata` — name / symbol / image for a mint. SPEC §5.1.
//!
//! Uses Jupiter's token list endpoint (`https://tokens.jup.ag/token/{mint}`)
//! which is much simpler than running a Helius DAS request. Falls back to a
//! placeholder shape when the endpoint returns 404 / non-2xx so the LLM
//! always gets a typed response. Helius/DAS upgrade is a TODO for PR 4+ if
//! richer metadata is required.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

const JUPITER_TOKEN_ENDPOINT: &str = "https://tokens.jup.ag/token";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Args {
    mint: String,
}

pub struct GetTokenMetadata;

#[async_trait]
impl Tool for GetTokenMetadata {
    fn name(&self) -> &str {
        "get-token-metadata"
    }

    fn description(&self) -> &str {
        "Look up display metadata (name, symbol, image URL) for a Solana \
         token by mint address. Returns placeholder fields if the token is \
         not in Jupiter's directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mint": {
                    "type": "string",
                    "description": "Base58-encoded SPL token mint address."
                }
            },
            "required": ["mint"]
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
        let mint_str = parsed.mint.trim().to_string();
        if let Err(e) = Pubkey::from_str(&mint_str) {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                format!("mint is not a valid base58 pubkey: {e}"),
            );
            return Ok(r.into());
        }

        let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::Generic,
                    format!("failed to build HTTP client: {e}"),
                );
                return Ok(r.into());
            }
        };
        let url = format!("{JUPITER_TOKEN_ENDPOINT}/{mint_str}");
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "zeroclaw_tools::solana::get_token_metadata", error=%e, %url, "Jupiter token request failed");
                let code = if e.is_timeout() {
                    ToolErrorCode::Timeout
                } else {
                    ToolErrorCode::RpcFailure
                };
                let r: ToolResult<()> = err(code, format!("jupiter token request failed: {e}"));
                return Ok(r.into());
            }
        };

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(ok(unknown_metadata(&mint_str)).into());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let r: ToolResult<()> = err(
                ToolErrorCode::RpcFailure,
                format!("jupiter token returned HTTP {status}"),
            );
            return Ok(r.into());
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::RpcFailure,
                    format!("jupiter token body not JSON: {e}"),
                );
                return Ok(r.into());
            }
        };
        Ok(ok(extract_metadata(&body, &mint_str)).into())
    }
}

fn extract_metadata(body: &Value, mint: &str) -> Value {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "<unknown>".to_string());
    let symbol = body
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "<unknown>".to_string());
    let image = body
        .get("logoURI")
        .or_else(|| body.get("image"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let decimals = body.get("decimals").and_then(Value::as_u64);
    json!({
        "mint": mint,
        "name": name,
        "symbol": symbol,
        "image": image,
        "decimals": decimals,
        "source": "jupiter:tokens",
    })
}

fn unknown_metadata(mint: &str) -> Value {
    json!({
        "mint": mint,
        "name": "<unknown>",
        "symbol": "<unknown>",
        "image": Value::Null,
        "decimals": Value::Null,
        "source": "jupiter:tokens (not found)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_mint() {
        let r = GetTokenMetadata.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_mint() {
        let r = GetTokenMetadata
            .execute(json!({ "mint": "garbage!!" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[test]
    fn extract_full_jupiter_token_payload() {
        let mint = "So11111111111111111111111111111111111111112";
        let body = json!({
            "address": mint,
            "name": "Wrapped SOL",
            "symbol": "SOL",
            "decimals": 9,
            "logoURI": "https://example.invalid/sol.png",
        });
        let v = extract_metadata(&body, mint);
        assert_eq!(v["name"], "Wrapped SOL");
        assert_eq!(v["symbol"], "SOL");
        assert_eq!(v["image"], "https://example.invalid/sol.png");
        assert_eq!(v["decimals"], 9);
    }

    #[test]
    fn unknown_when_404_emits_placeholders() {
        let v = unknown_metadata("Xyz");
        assert_eq!(v["name"], "<unknown>");
        assert_eq!(v["symbol"], "<unknown>");
        assert!(v["image"].is_null());
    }
}
