//! Solana / Metaplex tools (SPEC §5.1).
//!
//! Two sub-modules mirror the spec's tool-list layout:
//!
//! - [`shared`] — tools available in both `AGENT_MODE=public` and
//!   `AGENT_MODE=autonomous`.
//! - [`public`] — tools only available in public mode (`transfer-sol`,
//!   `transfer-token`).
//!
//! Tool registries are composed by [`public_agent_tools`] and
//! [`autonomous_agent_tools`].

pub mod public;
pub mod shared;

use std::sync::Arc;
use zeroclaw_api::tool::Tool;

/// Public-mode tool list: shared tools + public-only transfer tools.
pub fn public_agent_tools() -> Vec<Arc<dyn Tool>> {
    let mut tools = shared::shared_tools();
    tools.extend(public::public_tools());
    tools
}

/// Autonomous-mode tool list: shared tools only.
pub fn autonomous_agent_tools() -> Vec<Arc<dyn Tool>> {
    shared::shared_tools()
}

/// Convenience: return the names of tools enabled in public mode.
pub fn public_tool_names() -> Vec<String> {
    public_agent_tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// Convenience: return the names of tools enabled in autonomous mode.
pub fn autonomous_tool_names() -> Vec<String> {
    autonomous_agent_tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_tools_present_in_both_modes() {
        let shared_count = shared::shared_tools().len();
        assert_eq!(autonomous_agent_tools().len(), shared_count);
        assert!(public_agent_tools().len() >= shared_count);
    }

    #[test]
    fn shared_tool_names_are_kebab_case() {
        for t in shared::shared_tools() {
            let name = t.name();
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "tool name must be kebab-case: {name}"
            );
        }
    }

    #[test]
    fn shared_tool_set_matches_spec_5_1() {
        let names: Vec<String> = shared::shared_tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        // Read-only subset only in PR 3; identity/treasury tools land in PR 4+.
        let expected = [
            "get-balance",
            "get-token-balances",
            "get-transaction",
            "get-token-price",
            "get-token-metadata",
            "sleep",
        ];
        for e in &expected {
            assert!(names.iter().any(|n| n == e), "missing {e}");
        }
    }

    #[test]
    fn public_only_tools_include_transfer_tools() {
        let public_names: Vec<String> = public::public_tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        for e in ["transfer-sol", "transfer-token"] {
            assert!(public_names.iter().any(|n| n == e), "missing public tool {e}");
        }
    }

    #[test]
    fn public_tool_names_are_kebab_case() {
        for t in public::public_tools() {
            let name = t.name();
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "tool name must be kebab-case: {name}"
            );
        }
    }
}
