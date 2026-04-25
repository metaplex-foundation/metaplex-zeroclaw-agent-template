//! `delegate-execution` — stub. SPEC §4.2 / design doc §5.
//!
//! TODO (PR 5/6): Implement the real flow:
//!
//! 1. Read `agent_asset_address` from ctx (or fail with NotRegistered).
//! 2. Build the MPL Agent Registry "set executive" / "delegate" instruction
//!    via the hand-rolled IDL client noted in `register_agent.rs`.
//! 3. Sign the instruction with the user wallet (public mode) so the
//!    asset owner authorizes the delegation, and route through
//!    `submit_or_send`.
//! 4. Return `{ signature, asset, executive_pubkey }`.
//!
//! Auth: SPEC §5.4 specifies `AuthLevel::Owner` because delegation
//! changes who can sign on behalf of the asset.

use async_trait::async_trait;
use serde_json::{Value, json};
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err};

pub struct DelegateExecution;

#[async_trait]
impl Tool for DelegateExecution {
    fn name(&self) -> &str {
        "delegate-execution"
    }

    fn description(&self) -> &str {
        "Set the agent keypair as the executive signer for the agent asset, \
         enabling autonomous on-chain actions. Currently a stub — full \
         implementation deferred alongside register-agent (mpl-agent-registry \
         crate not on crates.io)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ApiToolResult> {
        let r: ToolResult<()> = err(
            ToolErrorCode::Generic,
            "delegate-execution requires a hand-rolled mpl-agent-registry IDL client; deferred",
        );
        Ok(r.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_not_implemented_stub_error() {
        let r = DelegateExecution.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
        assert!(r.output.contains("deferred"));
    }

    #[test]
    fn name_and_schema_are_well_formed() {
        let t = DelegateExecution;
        assert_eq!(t.name(), "delegate-execution");
        let schema = t.parameters_schema();
        assert_eq!(schema["type"], "object");
    }
}
