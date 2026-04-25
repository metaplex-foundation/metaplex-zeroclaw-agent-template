//! `launch-token` — stub. SPEC §5.1 / §10.5.
//!
//! TODO (PR 7+): Implement the real flow:
//!
//! 1. Verify `confirm_irreversible == true` (already enforced here as a
//!    schema-level guard; tools must not silently default this — SPEC §10.5
//!    treats the launch as a permanent on-chain action).
//! 2. Read `agent_asset_address` from ctx; fail with `NotRegistered` when
//!    `register-agent` has not yet been run for this agent.
//! 3. Call `ensure_agent_funded(ctx)` so the agent wallet has the SOL
//!    needed to pay the genesis rent + initial liquidity.
//! 4. Build the MPL Genesis "create token" instruction(s) using a hand-rolled
//!    IDL client. As of this writing the `mpl-genesis` crate is **not** on
//!    crates.io (confirmed via `cargo search`); the reference TypeScript
//!    implementation lives at
//!    `metaplex-foundation/agent-template/server/src/lib/tools/launch-token.ts`.
//! 5. Route the resulting transaction through `submit_or_send`.
//! 6. On success, persist the new token mint to `agent-state.json` via
//!    `state::save_at` and return `{ mint, signature, name, symbol }`.
//!
//! Auth: SPEC §5.4 specifies `AuthLevel::Owner` because launching the token
//! is an owner-only, irreversible identity-altering action.
//!
//! ## Why a stub?
//!
//! The Metaplex Genesis program is reachable today via the
//! `@metaplex-foundation/mpl-genesis` TypeScript SDK; the Rust crate has not
//! been published. PR 6 ships the surface (kebab-case name, owner-level
//! registration, `confirm_irreversible` gate) so the spec-aware system
//! prompt and tool-list snapshot are stable; the implementation lands when
//! the IDL client is hand-rolled, alongside `register-agent` and
//! `delegate-execution`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err};

#[derive(Debug, Deserialize)]
struct Args {
    /// Required acknowledgement that launching a token is irreversible.
    /// SPEC §10.5: any tool surface for launch-token MUST require an
    /// explicit `true` from the LLM (or the human delegating through it)
    /// before the on-chain action is queued.
    confirm_irreversible: bool,
    #[serde(default)]
    #[allow(dead_code)] // accepted for forward-compat; full impl lands in PR 7+
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    symbol: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    metadata_uri: Option<String>,
}

pub struct LaunchToken;

#[async_trait]
impl Tool for LaunchToken {
    fn name(&self) -> &str {
        "launch-token"
    }

    fn description(&self) -> &str {
        "Launch the agent's own token via MPL Genesis. IRREVERSIBLE: minting \
         the token is a permanent on-chain action; the caller MUST set \
         `confirm_irreversible = true` to acknowledge. Currently a stub — full \
         implementation deferred pending a hand-rolled mpl-genesis IDL client \
         (the crate is not on crates.io)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "confirm_irreversible": {
                    "type": "boolean",
                    "description": "Must be true. Acknowledges that launching the token is a permanent on-chain action (SPEC §10.5)."
                },
                "name": {
                    "type": "string",
                    "description": "Token display name. Optional — agent may default from configured assistant_name."
                },
                "symbol": {
                    "type": "string",
                    "description": "Ticker symbol (typically 3–6 uppercase ASCII)."
                },
                "metadata_uri": {
                    "type": "string",
                    "description": "Off-chain metadata JSON URI. Optional."
                }
            },
            "required": ["confirm_irreversible"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ApiToolResult> {
        // Parse args first so a malformed request fails as `INVALID_INPUT`,
        // matching every other tool's error shape.
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("invalid launch-token arguments: {e}"),
                );
                return Ok(r.into());
            }
        };

        // SPEC §10.5: refuse to even consider the call without explicit
        // acknowledgement — keeps an LLM that omits the field from
        // accidentally minting an asset on a misread of the schema.
        if !parsed.confirm_irreversible {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "launch-token requires confirm_irreversible = true (SPEC §10.5: launching the token is irreversible)",
            );
            return Ok(r.into());
        }

        // Implementation deferred: the `mpl-genesis` Rust crate is not
        // published. See module-level docs for the hand-rolled IDL client
        // plan.
        let r: ToolResult<()> = err(
            ToolErrorCode::Generic,
            "launch-token requires a hand-rolled mpl-genesis IDL client; deferred",
        );
        Ok(r.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_confirmation() {
        // Empty args → serde fails to deserialize the required field.
        let r = LaunchToken.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_explicit_false_confirmation() {
        // Schema accepts the field but the gate refuses the call.
        let r = LaunchToken
            .execute(json!({ "confirm_irreversible": false }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
        assert!(r.output.contains("irreversible"));
    }

    #[tokio::test]
    async fn returns_not_implemented_when_confirmed() {
        // Even with a correct confirmation the stub surfaces a deferred
        // error so the LLM cannot mistake silence for success.
        let r = LaunchToken
            .execute(json!({ "confirm_irreversible": true }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
        assert!(r.output.contains("deferred"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_for_confirmation() {
        // The boolean field MUST be a real bool — refuse a string "true"
        // so an LLM can't trick the gate by stringifying.
        let r = LaunchToken
            .execute(json!({ "confirm_irreversible": "true" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[test]
    fn name_and_schema_are_well_formed() {
        let t = LaunchToken;
        assert_eq!(t.name(), "launch-token");
        let schema = t.parameters_schema();
        assert_eq!(schema["type"], "object");
        // The required array MUST contain `confirm_irreversible` so a
        // schema-aware client surfaces it in the function-call shape.
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required
                .iter()
                .any(|v| v.as_str() == Some("confirm_irreversible"))
        );
    }
}
