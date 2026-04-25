//! Auth policy + `with_auth` wrapper. SPEC §5.4.
//!
//! Wraps any [`Tool`] so the auth level is checked against the current
//! [`AgentContext`](crate::context::AgentContext) before `execute` runs.
//! Unknown levels fail closed.

use crate::context::current_context;
use crate::error_codes::ToolErrorCode;
use crate::tool_result::{ToolResult, err};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};

/// Per-tool authorization level. Unknown levels are denied by default
/// (`fail-closed`). New levels can be added to this enum + matched in
/// [`default_auth_policy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthLevel {
    /// Any caller may invoke this tool.
    Public,
    /// Only the verified asset owner may invoke this tool.
    Owner,
}

impl AuthLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthLevel::Public => "public",
            AuthLevel::Owner => "owner",
        }
    }
}

/// SPEC §5.4 default policy:
///
/// - `Public` is always allowed.
/// - `Owner` requires both `owner_wallet` *and* `wallet_address` to be set,
///   *and* equal. The `owner_wallet != null` guard fails closed when owner
///   resolution has not yet succeeded.
pub fn default_auth_policy(level: AuthLevel) -> Result<(), ToolErrorCode> {
    match level {
        AuthLevel::Public => Ok(()),
        AuthLevel::Owner => {
            let ctx = current_context().map_err(|e| e.code)?;
            match (ctx.owner_wallet, ctx.wallet_address) {
                (Some(owner), Some(connected)) if owner == connected => Ok(()),
                _ => Err(ToolErrorCode::Unauthorized),
            }
        }
    }
}

/// `Tool` adapter that runs [`default_auth_policy`] before delegating to the
/// inner tool. Auth failures return a `ToolResult::Error` with code
/// `Unauthorized` (or `Generic` if context is missing).
pub struct AuthGate<T: Tool> {
    inner: T,
    level: AuthLevel,
}

impl<T: Tool> AuthGate<T> {
    pub fn new(inner: T, level: AuthLevel) -> Self {
        Self { inner, level }
    }

    pub fn level(&self) -> AuthLevel {
        self.level
    }
}

#[async_trait]
impl<T: Tool> Tool for AuthGate<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ApiToolResult> {
        if let Err(code) = default_auth_policy(self.level) {
            let message = match code {
                ToolErrorCode::Unauthorized => "This operation requires owner authorization",
                _ => "Authorization check failed (no agent context)",
            };
            let r: ToolResult<()> = err(code, message);
            return Ok(r.into());
        }
        self.inner.execute(args).await
    }
}

/// Convenience constructor returning the gated tool as `Arc<dyn Tool>` so it
/// can be dropped straight into a tool registry.
pub fn with_auth<T: Tool + 'static>(tool: T, level: AuthLevel) -> Arc<dyn Tool> {
    Arc::new(AuthGate::new(tool, level))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::SolanaClient;
    use crate::context::{AGENT_CTX, AgentContext, AgentMode};
    use serde_json::json;
    use solana_sdk::pubkey::Pubkey;

    fn pk(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    fn ctx_with(owner: Option<Pubkey>, connected: Option<Pubkey>) -> AgentContext {
        AgentContext {
            wallet_address: connected,
            agent_mode: AgentMode::Public,
            agent_pubkey: pk(99),
            agent_asset_address: None,
            agent_token_mint: None,
            agent_fee_sol: 0.001,
            token_override: None,
            owner_wallet: owner,
            tx_sender: None,
            solana: Arc::new(SolanaClient::new("http://example.invalid")),
        }
    }

    #[tokio::test]
    async fn public_policy_passes_outside_scope() {
        // Public requires no context — it must Ok even with no AGENT_CTX.
        assert!(default_auth_policy(AuthLevel::Public).is_ok());
    }

    #[tokio::test]
    async fn owner_policy_outside_scope_returns_generic() {
        let err = default_auth_policy(AuthLevel::Owner).expect_err("must error");
        assert_eq!(err, ToolErrorCode::Generic);
    }

    #[tokio::test]
    async fn owner_policy_with_mismatched_wallet_returns_unauthorized() {
        AGENT_CTX
            .scope(ctx_with(Some(pk(1)), Some(pk(2))), async {
                let err = default_auth_policy(AuthLevel::Owner).expect_err("mismatched");
                assert_eq!(err, ToolErrorCode::Unauthorized);
            })
            .await;
    }

    #[tokio::test]
    async fn owner_policy_with_no_connected_wallet_returns_unauthorized() {
        AGENT_CTX
            .scope(ctx_with(Some(pk(1)), None), async {
                let err = default_auth_policy(AuthLevel::Owner).expect_err("no wallet");
                assert_eq!(err, ToolErrorCode::Unauthorized);
            })
            .await;
    }

    #[tokio::test]
    async fn owner_policy_with_no_owner_returns_unauthorized() {
        AGENT_CTX
            .scope(ctx_with(None, Some(pk(2))), async {
                let err = default_auth_policy(AuthLevel::Owner).expect_err("no owner");
                assert_eq!(err, ToolErrorCode::Unauthorized);
            })
            .await;
    }

    #[tokio::test]
    async fn owner_policy_with_matching_wallet_passes() {
        AGENT_CTX
            .scope(ctx_with(Some(pk(7)), Some(pk(7))), async {
                assert!(default_auth_policy(AuthLevel::Owner).is_ok());
            })
            .await;
    }

    // ---- AuthGate behaviour ------------------------------------------------

    struct AlwaysOk;
    #[async_trait]
    impl Tool for AlwaysOk {
        fn name(&self) -> &str {
            "always-ok"
        }
        fn description(&self) -> &str {
            "test tool that always succeeds"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        async fn execute(&self, _args: Value) -> anyhow::Result<ApiToolResult> {
            Ok(ApiToolResult {
                success: true,
                output: "{\"status\":\"success\",\"data\":\"ran\"}".into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn gate_denies_owner_when_unauthorized() {
        let gate = AuthGate::new(AlwaysOk, AuthLevel::Owner);
        AGENT_CTX
            .scope(ctx_with(Some(pk(1)), Some(pk(2))), async {
                let r = gate.execute(json!({})).await.unwrap();
                assert!(!r.success);
                assert!(r.output.contains("\"status\":\"error\""));
                assert!(r.output.contains("\"UNAUTHORIZED\""));
            })
            .await;
    }

    #[tokio::test]
    async fn gate_denies_owner_when_no_context() {
        let gate = AuthGate::new(AlwaysOk, AuthLevel::Owner);
        let r = gate.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("\"GENERIC\""));
    }

    #[tokio::test]
    async fn gate_passes_owner_when_authorized() {
        let gate = AuthGate::new(AlwaysOk, AuthLevel::Owner);
        AGENT_CTX
            .scope(ctx_with(Some(pk(8)), Some(pk(8))), async {
                let r = gate.execute(json!({})).await.unwrap();
                assert!(r.success);
                assert!(r.output.contains("ran"));
            })
            .await;
    }

    #[tokio::test]
    async fn gate_passes_public_regardless_of_context() {
        let gate = AuthGate::new(AlwaysOk, AuthLevel::Public);
        // No scope — public must still execute.
        let r = gate.execute(json!({})).await.unwrap();
        assert!(r.success);
    }

    #[test]
    fn auth_level_strings() {
        assert_eq!(AuthLevel::Public.as_str(), "public");
        assert_eq!(AuthLevel::Owner.as_str(), "owner");
    }

    #[test]
    fn with_auth_returns_arc_dyn_tool() {
        let arc: Arc<dyn Tool> = with_auth(AlwaysOk, AuthLevel::Public);
        assert_eq!(arc.name(), "always-ok");
    }
}
