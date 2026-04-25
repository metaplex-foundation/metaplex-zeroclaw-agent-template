//! `get-token-price` — current USD price for a mint via Jupiter Price API v3.
//!
//! Endpoint: `https://api.jup.ag/price/v3?ids={mint}` (SPEC §5.1, §13).
//! Response shape (subset we care about):
//! ```json
//! { "<mint>": { "usdPrice": 1.23, ...} }
//! ```
//!
//! v3 returns a flat map keyed by mint; older v2 nested it under `data`.
//! We tolerate both for robustness.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

const JUPITER_PRICE_V3_ENDPOINT: &str = "https://api.jup.ag/price/v3";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Args {
    mint: String,
}

pub struct GetTokenPrice;

#[async_trait]
impl Tool for GetTokenPrice {
    fn name(&self) -> &str {
        "get-token-price"
    }

    fn description(&self) -> &str {
        "Fetch the current USD price for a Solana token (by mint address) \
         from Jupiter's Price API v3. Returns null price for tokens Jupiter \
         does not index."
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
        let url = format!("{JUPITER_PRICE_V3_ENDPOINT}?ids={mint_str}");
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(target: "zeroclaw_tools::solana::get_token_price", error=%e, %url, "Jupiter price request failed");
                let code = if e.is_timeout() {
                    ToolErrorCode::Timeout
                } else {
                    ToolErrorCode::RpcFailure
                };
                let r: ToolResult<()> = err(code, format!("jupiter price request failed: {e}"));
                return Ok(r.into());
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let r: ToolResult<()> = err(
                ToolErrorCode::RpcFailure,
                format!("jupiter price returned HTTP {status}"),
            );
            return Ok(r.into());
        }
        let json_val: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::RpcFailure,
                    format!("jupiter price body not JSON: {e}"),
                );
                return Ok(r.into());
            }
        };

        // v3 shape: { "<mint>": { "usdPrice": 1.23, ... } }
        // v2 shape: { "data": { "<mint>": { "price": "1.23", ... } } }
        let usd = extract_usd_price(&json_val, &mint_str);

        Ok(ok(json!({
            "mint": mint_str,
            "usd": usd,
            "source": "jupiter:price/v3",
        }))
        .into())
    }
}

/// Walk the (possibly nested) Jupiter response and return the USD price as
/// an f64, or `None` if absent / malformed.
fn extract_usd_price(json_val: &Value, mint: &str) -> Option<f64> {
    // v3 flat map first.
    if let Some(node) = json_val.get(mint)
        && let Some(p) = price_from_node(node)
    {
        return Some(p);
    }
    // Fallback to v2 / { data: {...} } shape.
    if let Some(node) = json_val.get("data").and_then(|d| d.get(mint))
        && let Some(p) = price_from_node(node)
    {
        return Some(p);
    }
    None
}

fn price_from_node(node: &Value) -> Option<f64> {
    // v3: usdPrice (number)
    if let Some(p) = node.get("usdPrice").and_then(Value::as_f64) {
        return Some(p);
    }
    // v2: price (number or string)
    if let Some(p) = node.get("price").and_then(Value::as_f64) {
        return Some(p);
    }
    if let Some(s) = node.get("price").and_then(Value::as_str)
        && let Ok(p) = s.parse::<f64>()
    {
        return Some(p);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_mint() {
        let r = GetTokenPrice.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_invalid_mint() {
        let r = GetTokenPrice
            .execute(json!({ "mint": "garbage!!" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[test]
    fn extract_v3_shape() {
        let mint = "So11111111111111111111111111111111111111112";
        let body: Value = serde_json::json!({ mint: { "usdPrice": 188.4 } });
        assert_eq!(extract_usd_price(&body, mint), Some(188.4));
    }

    #[test]
    fn extract_v2_fallback_numeric() {
        let mint = "So11111111111111111111111111111111111111112";
        let body: Value = serde_json::json!({ "data": { mint: { "price": 188.4 } } });
        assert_eq!(extract_usd_price(&body, mint), Some(188.4));
    }

    #[test]
    fn extract_v2_fallback_string() {
        let mint = "So11111111111111111111111111111111111111112";
        let body: Value = serde_json::json!({ "data": { mint: { "price": "188.4" } } });
        assert_eq!(extract_usd_price(&body, mint), Some(188.4));
    }

    #[test]
    fn extract_missing_returns_none() {
        let body: Value = serde_json::json!({});
        assert_eq!(extract_usd_price(&body, "MintXYZ"), None);
    }
}
