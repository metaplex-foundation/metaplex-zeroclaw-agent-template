//! `register-agent` — stub. SPEC §5.1 / design doc §5.
//!
//! TODO (PR 5/6): Implement the real flow:
//!
//! 1. Call `ensure_agent_funded(ctx)` so the agent wallet has the
//!    SOL needed to pay the registry rent.
//! 2. Build the MPL Agent Registry "register" instruction using a
//!    hand-rolled IDL client (the `mpl-agent-registry` crate is **not**
//!    on crates.io as of this writing — confirmed via `cargo search`).
//!    The reference TypeScript implementation is at
//!    `metaplex-foundation/agent-template/server/src/lib/tools/register-agent.ts`.
//! 3. Pass the new asset Pubkey through `submit_or_send`.
//! 4. On success persist the asset address to `agent-state.json`
//!    via `state::save_at` and return `{ asset, signature }`.
//!
//! Auth: SPEC §5.4 specifies `AuthLevel::Owner` so only the verified
//! asset owner can re-register / re-mint identity assets.

use async_trait::async_trait;
use serde_json::{Value, json};
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err};

pub struct RegisterAgent;

#[async_trait]
impl Tool for RegisterAgent {
    fn name(&self) -> &str {
        "register-agent"
    }

    fn description(&self) -> &str {
        "Mint an MPL Core asset on the Metaplex Agent Registry to establish \
         the agent's on-chain identity. Currently a stub — full \
         implementation deferred to a follow-up PR pending a hand-rolled \
         mpl-agent-registry IDL client (the crate is not on crates.io)."
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
            "register-agent requires a hand-rolled mpl-agent-registry IDL client; deferred",
        );
        Ok(r.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_not_implemented_stub_error() {
        let r = RegisterAgent.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("GENERIC"));
        assert!(r.output.contains("deferred"));
    }

    #[test]
    fn name_and_schema_are_well_formed() {
        let t = RegisterAgent;
        assert_eq!(t.name(), "register-agent");
        let schema = t.parameters_schema();
        assert_eq!(schema["type"], "object");
    }
}
