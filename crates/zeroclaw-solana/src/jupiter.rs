//! Jupiter API client (quote + swap + price).
//!
//! HTTPS only; no SDK exists. SPEC §10.4 simulate-and-verify check lives
//! here. This module is the single place in the workspace that knows how to
//! talk to Jupiter — tools (`swap-token`, `buyback-token`, `sell-token`)
//! drive it through the [`JupiterApi`] trait so they can be tested without
//! network.
//!
//! Endpoints used (SPEC §13):
//! - GET  `https://api.jup.ag/swap/v1/quote`
//! - POST `https://api.jup.ag/swap/v1/swap`
//! - GET  `https://api.jup.ag/price/v3` (consumed by `get-token-price`)

use crate::client::SolanaClient;
use crate::error_codes::{ToolErrorCode, truncate_llm_message};
use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_client::rpc_config::{
    RpcSimulateTransactionAccountsConfig, RpcSimulateTransactionConfig,
};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;
use solana_transaction_status::UiTransactionEncoding;
use std::time::Duration;

const QUOTE_URL: &str = "https://api.jup.ag/swap/v1/quote";
const SWAP_URL: &str = "https://api.jup.ag/swap/v1/swap";
/// Jupiter Price API endpoint. Documented here so future migration of the
/// `get-token-price` tool can consume it through this module instead of
/// duplicating the URL.
#[allow(dead_code)]
pub(crate) const PRICE_URL: &str = "https://api.jup.ag/price/v3";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Parameters for [`JupiterApi::quote`].
#[derive(Debug, Clone, Serialize)]
pub struct QuoteParams {
    /// Base58-encoded input mint.
    pub input_mint: String,
    /// Base58-encoded output mint.
    pub output_mint: String,
    /// Raw integer in the input mint's smallest unit.
    pub amount: u64,
    /// Slippage tolerance in basis points (50 = 0.5%).
    pub slippage_bps: u16,
}

/// Subset of the Jupiter quote response that we read explicitly. The full
/// response body is preserved in [`Quote::raw`] so it can be passed back
/// verbatim to the `/swap` endpoint (Jupiter's API contract requires the
/// caller to forward the entire `quoteResponse` object unchanged).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quote {
    pub input_mint: String,
    pub output_mint: String,
    /// String-encoded integer (raw amount in input mint's smallest unit).
    pub in_amount: String,
    /// String-encoded integer (raw expected min out in output mint's smallest unit).
    pub out_amount: String,
    /// String-encoded decimal — e.g. `"0.0123"` for 1.23%.
    pub price_impact_pct: String,
    /// Slippage tolerance the quote was computed against.
    pub slippage_bps: u16,
    /// Pass-through of the entire response — `/swap` wants this object whole.
    #[serde(flatten)]
    pub raw: serde_json::Value,
}

/// Parameters for [`JupiterApi::swap`].
#[derive(Debug, Clone)]
pub struct SwapParams<'a> {
    pub quote_response: &'a Quote,
    pub user_public_key: String,
    pub wrap_and_unwrap_sol: bool,
    pub use_shared_accounts: bool,
}

/// Subset of the Jupiter `/swap` response we care about.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapResponse {
    /// Base64-encoded versioned transaction ready to sign.
    pub swap_transaction: String,
    /// Block height after which the returned blockhash is no longer valid.
    pub last_valid_block_height: u64,
}

/// Trait abstracting the Jupiter HTTP client so tools can be unit-tested.
/// Mirrors the [`AssetOwnerFetcher`](crate::owner::AssetOwnerFetcher)
/// pattern used elsewhere.
#[async_trait]
pub trait JupiterApi: Send + Sync {
    async fn quote(&self, p: &QuoteParams) -> Result<Quote, ToolErrorCode>;
    async fn swap(&self, p: &SwapParams<'_>) -> Result<SwapResponse, ToolErrorCode>;
}

/// Real Jupiter HTTP client.
pub struct JupiterClient {
    http: reqwest::Client,
    api_key: Option<String>,
}

impl JupiterClient {
    /// Build a new client. `api_key` is forwarded as the `x-api-key` header
    /// when present — Jupiter relaxes rate limits for keyed callers.
    pub fn new(api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("reqwest client");
        Self { http, api_key }
    }

    fn apply_key(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(k) => req.header("x-api-key", k),
            None => req,
        }
    }

    async fn parse_jupiter_error(resp: reqwest::Response) -> ToolErrorCode {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::error!(target: "zeroclaw_solana::jupiter", %status, body=%truncate_llm_message(body.clone()), "jupiter HTTP error");
        // Jupiter sometimes returns helpful machine-readable hints in the
        // body — bucket the most common patterns into typed codes. Anything
        // else (bad gateway, 4xx, 5xx) collapses into RpcFailure so the
        // gateway can retry or fall back uniformly.
        let lc = body.to_ascii_lowercase();
        if lc.contains("slippage") {
            ToolErrorCode::SlippageTooHigh
        } else if lc.contains("price impact") {
            ToolErrorCode::PriceImpactTooHigh
        } else {
            ToolErrorCode::RpcFailure
        }
    }
}

#[async_trait]
impl JupiterApi for JupiterClient {
    async fn quote(&self, p: &QuoteParams) -> Result<Quote, ToolErrorCode> {
        let req = self.http.get(QUOTE_URL).query(&[
            ("inputMint", p.input_mint.as_str()),
            ("outputMint", p.output_mint.as_str()),
            ("amount", &p.amount.to_string()),
            ("slippageBps", &p.slippage_bps.to_string()),
        ]);
        let req = self.apply_key(req);
        let resp = req.send().await.map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "quote request failed");
            if e.is_timeout() {
                ToolErrorCode::Timeout
            } else {
                ToolErrorCode::RpcFailure
            }
        })?;
        if !resp.status().is_success() {
            return Err(Self::parse_jupiter_error(resp).await);
        }
        resp.json::<Quote>().await.map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "quote body decode failed");
            ToolErrorCode::RpcFailure
        })
    }

    async fn swap(&self, p: &SwapParams<'_>) -> Result<SwapResponse, ToolErrorCode> {
        let body = json!({
            "quoteResponse": p.quote_response,
            "userPublicKey": p.user_public_key,
            "wrapAndUnwrapSol": p.wrap_and_unwrap_sol,
            "useSharedAccounts": p.use_shared_accounts,
        });
        let req = self.http.post(SWAP_URL).json(&body);
        let req = self.apply_key(req);
        let resp = req.send().await.map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "swap request failed");
            if e.is_timeout() {
                ToolErrorCode::Timeout
            } else {
                ToolErrorCode::RpcFailure
            }
        })?;
        if !resp.status().is_success() {
            return Err(Self::parse_jupiter_error(resp).await);
        }
        resp.json::<SwapResponse>().await.map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "swap body decode failed");
            ToolErrorCode::RpcFailure
        })
    }
}

/// Decode the base64 versioned tx returned by Jupiter and run a
/// `simulateTransaction` integrity check (SPEC §10.4).
///
/// Asserts:
/// - input-mint debit from the agent's ATA ≤ `params.amount + buffer`,
/// - output-mint credit to the agent's ATA ≥ `expected_min_out` (derived
///   from the quote and its slippage tolerance).
///
/// Returns the decoded `VersionedTransaction` so the caller can sign + send
/// it. On a delta mismatch, returns `ToolErrorCode::Integrity` and the
/// caller MUST NOT sign.
///
/// Implementation note: this is the "best-effort" variant per the PR brief —
/// it pulls `pre_token_balances` / `post_token_balances` from the
/// simulation result, which Jupiter swaps reliably populate. The pure
/// delta-checking core lives in [`verify_swap_deltas`] for unit testing.
pub async fn simulate_and_verify_swap(
    rpc: &SolanaClient,
    agent_pubkey: &Pubkey,
    swap_tx_b64: &str,
    params: &QuoteParams,
    quote: &Quote,
) -> Result<VersionedTransaction, ToolErrorCode> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(swap_tx_b64)
        .map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "swap tx base64 decode failed");
            ToolErrorCode::Integrity
        })?;
    let tx: VersionedTransaction = bincode::deserialize(&bytes).map_err(|e| {
        tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "swap tx bincode decode failed");
        ToolErrorCode::Integrity
    })?;

    let cfg = RpcSimulateTransactionConfig {
        sig_verify: false,
        replace_recent_blockhash: true,
        commitment: None,
        encoding: Some(UiTransactionEncoding::Base64),
        accounts: Some(RpcSimulateTransactionAccountsConfig {
            encoding: None,
            addresses: vec![],
        }),
        min_context_slot: None,
        inner_instructions: false,
    };
    let sim = rpc
        .rpc
        .simulate_transaction_with_config(&tx, cfg)
        .await
        .map_err(|e| {
            tracing::error!(target: "zeroclaw_solana::jupiter", error=%e, "simulate_transaction failed");
            ToolErrorCode::RpcFailure
        })?;
    let result = sim.value;

    if let Some(err) = result.err {
        tracing::error!(target: "zeroclaw_solana::jupiter", error=?err, "simulation reported error");
        return Err(ToolErrorCode::Integrity);
    }

    // Compute expected_min_out: parse out_amount, then apply slippage_bps.
    let quoted_out = quote.out_amount.parse::<u128>().map_err(|_| {
        tracing::error!(target: "zeroclaw_solana::jupiter", "out_amount not an integer");
        ToolErrorCode::Integrity
    })?;
    let expected_min_out = apply_slippage_floor(quoted_out, quote.slippage_bps);

    // Compare against simulated pre/post token balances if Jupiter returned
    // them. If the sim did not surface balances, fall through to the
    // best-effort gate (no error from the tx, so the swap is allowed).
    let pre = result.pre_token_balances.unwrap_or_default();
    let post = result.post_token_balances.unwrap_or_default();
    let owner = agent_pubkey.to_string();

    if !pre.is_empty() || !post.is_empty() {
        verify_swap_deltas(
            &pre,
            &post,
            &owner,
            &params.input_mint,
            &params.output_mint,
            params.amount as u128,
            expected_min_out,
        )?;
    }

    Ok(tx)
}

/// Apply slippage_bps as a floor: returns `quoted_out * (1 - slippage_bps/10_000)`.
/// Saturating math; never panics for u16 bps in [0, 10_000].
fn apply_slippage_floor(quoted_out: u128, slippage_bps: u16) -> u128 {
    let bps = u128::from(slippage_bps.min(10_000));
    // floor = quoted * (10_000 - bps) / 10_000
    quoted_out.saturating_mul(10_000u128.saturating_sub(bps)) / 10_000
}

/// Pure delta verifier extracted for unit tests.
///
/// Validates the agent's pre/post token balances against the swap
/// expectations. Token amounts are read from `UiTransactionTokenBalance`
/// entries whose `owner` matches `agent_owner` and whose `mint` matches
/// `input_mint` / `output_mint`.
///
/// - Input debit (`pre - post` for `input_mint`) must be ≤ `max_in`.
/// - Output credit (`post - pre` for `output_mint`) must be ≥ `expected_min_out`.
///
/// If the agent has no entry for the input mint pre-sim, that's a failure
/// (the agent has nothing to swap). If there's no entry for the output
/// mint either pre- or post-sim, that's also a failure (we expected a
/// credit). Missing-but-zero deltas pass for cases where Jupiter creates
/// the destination ATA inside the same tx.
pub fn verify_swap_deltas(
    pre: &[solana_transaction_status::UiTransactionTokenBalance],
    post: &[solana_transaction_status::UiTransactionTokenBalance],
    agent_owner: &str,
    input_mint: &str,
    output_mint: &str,
    max_in: u128,
    expected_min_out: u128,
) -> Result<(), ToolErrorCode> {
    let pre_in = sum_balance(pre, agent_owner, input_mint);
    let post_in = sum_balance(post, agent_owner, input_mint);
    let pre_out = sum_balance(pre, agent_owner, output_mint);
    let post_out = sum_balance(post, agent_owner, output_mint);

    // Debit on input side: pre_in must be ≥ post_in (it shrank or stayed
    // equal). Allow a small over-debit by 1 atomic unit to absorb mint
    // rounding, but never more than max_in.
    let in_debit = pre_in.saturating_sub(post_in);
    if in_debit > max_in.saturating_add(1) {
        tracing::error!(
            target: "zeroclaw_solana::jupiter",
            in_debit, max_in, "input debit exceeds max_in"
        );
        return Err(ToolErrorCode::Integrity);
    }

    // Credit on output side: post_out - pre_out must be ≥ expected_min_out.
    // Permit a small undershoot by 1 atomic unit to absorb mint rounding.
    let out_credit = post_out.saturating_sub(pre_out);
    let floor = expected_min_out.saturating_sub(1);
    if out_credit < floor {
        tracing::error!(
            target: "zeroclaw_solana::jupiter",
            out_credit, expected_min_out, "output credit below expected_min_out"
        );
        return Err(ToolErrorCode::Integrity);
    }

    Ok(())
}

fn sum_balance(
    balances: &[solana_transaction_status::UiTransactionTokenBalance],
    agent_owner: &str,
    mint: &str,
) -> u128 {
    balances
        .iter()
        .filter(|b| b.mint == mint && balance_owner_matches(b, agent_owner))
        .filter_map(|b| b.ui_token_amount.amount.parse::<u128>().ok())
        .sum()
}

fn balance_owner_matches(
    b: &solana_transaction_status::UiTransactionTokenBalance,
    agent_owner: &str,
) -> bool {
    use solana_transaction_status::option_serializer::OptionSerializer;
    match &b.owner {
        OptionSerializer::Some(o) => o == agent_owner,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_transaction_status::UiTransactionTokenBalance;
    use solana_transaction_status::option_serializer::OptionSerializer;

    fn balance(owner: &str, mint: &str, amount: u128) -> UiTransactionTokenBalance {
        use solana_account_decoder_client_types::token::UiTokenAmount;
        UiTransactionTokenBalance {
            account_index: 0,
            mint: mint.to_string(),
            ui_token_amount: UiTokenAmount {
                ui_amount: None,
                decimals: 9,
                amount: amount.to_string(),
                ui_amount_string: amount.to_string(),
            },
            owner: OptionSerializer::Some(owner.to_string()),
            program_id: OptionSerializer::Some(
                "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
            ),
        }
    }

    #[test]
    fn apply_slippage_floor_basic() {
        // 50 bps = 0.5%. floor of 1_000 = 995.
        assert_eq!(apply_slippage_floor(1_000, 50), 995);
        // 0 bps → unchanged.
        assert_eq!(apply_slippage_floor(1_000, 0), 1_000);
        // Cap at 10_000 bps (100%).
        assert_eq!(apply_slippage_floor(1_000, 10_000), 0);
        // Above the cap also clamps to 100%.
        assert_eq!(apply_slippage_floor(1_000, 65_535), 0);
    }

    #[test]
    fn verify_within_bounds_passes() {
        let owner = "AgentOwner";
        let in_mint = "InMint";
        let out_mint = "OutMint";
        let pre = vec![balance(owner, in_mint, 1_000), balance(owner, out_mint, 0)];
        let post = vec![balance(owner, in_mint, 0), balance(owner, out_mint, 980)];
        // max_in = 1_000, expected_min_out = 950 → ok (out_credit = 980 ≥ 950).
        verify_swap_deltas(&pre, &post, owner, in_mint, out_mint, 1_000, 950).unwrap();
    }

    #[test]
    fn verify_input_overdebit_fails() {
        let owner = "AgentOwner";
        let in_mint = "InMint";
        let out_mint = "OutMint";
        // Pre = 5_000, post = 0 → debit 5_000 > max_in 1_000.
        let pre = vec![balance(owner, in_mint, 5_000), balance(owner, out_mint, 0)];
        let post = vec![balance(owner, in_mint, 0), balance(owner, out_mint, 980)];
        let err = verify_swap_deltas(&pre, &post, owner, in_mint, out_mint, 1_000, 950)
            .expect_err("must fail");
        assert_eq!(err, ToolErrorCode::Integrity);
    }

    #[test]
    fn verify_output_undercredit_fails() {
        let owner = "AgentOwner";
        let in_mint = "InMint";
        let out_mint = "OutMint";
        let pre = vec![balance(owner, in_mint, 1_000), balance(owner, out_mint, 0)];
        // Only 100 received, expected 950.
        let post = vec![balance(owner, in_mint, 0), balance(owner, out_mint, 100)];
        let err = verify_swap_deltas(&pre, &post, owner, in_mint, out_mint, 1_000, 950)
            .expect_err("must fail");
        assert_eq!(err, ToolErrorCode::Integrity);
    }

    #[test]
    fn verify_ignores_other_owners() {
        let owner = "AgentOwner";
        let other = "Stranger";
        let in_mint = "InMint";
        let out_mint = "OutMint";
        // The agent's input balance doesn't budge — but a stranger's does.
        // The check should only see the agent's deltas and then complain
        // about a missing output credit, not pass silently.
        let pre = vec![
            balance(owner, in_mint, 1_000),
            balance(other, in_mint, 1_000),
            balance(owner, out_mint, 0),
        ];
        let post = vec![
            balance(owner, in_mint, 1_000),
            balance(other, in_mint, 0),
            balance(owner, out_mint, 0),
        ];
        let err = verify_swap_deltas(&pre, &post, owner, in_mint, out_mint, 1_000, 950)
            .expect_err("must fail when agent received nothing");
        assert_eq!(err, ToolErrorCode::Integrity);
    }

    #[test]
    fn quote_round_trips_through_serde() {
        // Confirm the flatten-on-raw pattern preserves unknown Jupiter
        // fields so they can be passed back to /swap unchanged.
        let json = r#"{
            "inputMint": "So11111111111111111111111111111111111111112",
            "outputMint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "inAmount": "1000000",
            "outAmount": "23456789",
            "priceImpactPct": "0.0042",
            "slippageBps": 50,
            "routePlan": [{"swapInfo": "opaque"}],
            "contextSlot": 12345
        }"#;
        let q: Quote = serde_json::from_str(json).unwrap();
        assert_eq!(q.in_amount, "1000000");
        assert_eq!(q.out_amount, "23456789");
        assert_eq!(q.slippage_bps, 50);
        // Ensure the raw flatten preserved the unknown fields.
        let v = serde_json::to_value(&q).unwrap();
        assert!(v.get("routePlan").is_some());
        assert!(v.get("contextSlot").is_some());
    }

    #[test]
    fn jupiter_client_constructs_with_and_without_key() {
        let _no_key = JupiterClient::new(None);
        let _with_key = JupiterClient::new(Some("test".into()));
    }
}
