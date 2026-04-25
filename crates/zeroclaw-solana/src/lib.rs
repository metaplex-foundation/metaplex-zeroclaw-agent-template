//! Solana + Metaplex foundation for ZeroClaw-hosted agents.
//!
//! Port of the Metaplex Agent Template's `@metaplex-agent/shared` package.
//! Data, traits, and thin RPC wrapping only — no LLM, no transport.
//!
//! ## Module map (SPEC §1)
//! - [`error_codes`] — classifier taxonomy (SPEC §5.3)
//! - [`tool_result`] — discriminated tool-result + api::ToolResult bridge (SPEC §5.2)
//! - [`keypair`] — AGENT_KEYPAIR decoder (SPEC §8.1)
//! - [`state`] — atomic agent-state.json (SPEC §4.3)
//! - [`context`] — AgentContext + TxSender (SPEC Appendix B)
//! - [`client`] — thin async RPC wrapper + preflight (SPEC §6.8)
//! - [`owner`] — on-chain owner resolver + TTL cache (SPEC §3.4)
//! - [`jupiter`] — Jupiter quote + swap HTTP client + simulate-and-verify
//!   integrity check (SPEC §10.4)

pub mod auth;
pub mod client;
pub mod context;
pub mod error_codes;
pub mod funding;
pub mod jupiter;
pub mod keypair;
pub mod owner;
pub mod state;
pub mod submit_or_send;
pub mod tool_result;

pub use auth::{AuthLevel, default_auth_policy, with_auth};
pub use context::{
    AGENT_CTX, AgentContext, AgentMode, TxSendError, TxSendOpts, TxSender, current_context,
};
pub use error_codes::{ToolError, ToolErrorCode, to_tool_error};
pub use funding::{AGENT_FUNDING_SOL, AGENT_FUNDING_THRESHOLD_SOL, Funded, ensure_agent_funded};
pub use jupiter::{
    JupiterApi, JupiterClient, Quote, QuoteParams, SwapParams, SwapResponse,
    simulate_and_verify_swap, verify_swap_deltas,
};
pub use submit_or_send::{SubmitOpts, submit_or_send};
pub use tool_result::{ToolResult, err, info, ok};
