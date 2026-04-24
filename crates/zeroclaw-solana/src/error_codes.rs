use serde::{Deserialize, Serialize};

/// Taxonomy of tool-result error codes. Matches SPEC §5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ToolErrorCode {
    InvalidInput,
    InsufficientFunds,
    Timeout,
    NotFound,
    SlippageTooHigh,
    PriceImpactTooHigh,
    Unauthorized,
    RpcFailure,
    Integrity,
    NotRegistered,
    NoToken,
    Generic,
}

/// A classified tool error ready to return to the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
}

/// Maximum byte length of the LLM-safe message. Classifiers truncate at the
/// nearest char boundary at or below this value.
const MAX_MESSAGE_BYTES: usize = 200;

/// Truncate an error/diagnostic string to `MAX_MESSAGE_BYTES` while
/// respecting UTF-8 character boundaries. Used by both `to_tool_error`
/// and `tool_result::err` to keep LLM-visible messages bounded
/// (SPEC §5.3).
pub fn truncate_llm_message(mut s: String) -> String {
    if s.len() <= MAX_MESSAGE_BYTES {
        return s;
    }
    let mut cut = MAX_MESSAGE_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s
}

/// Classify an `anyhow::Error` (or `&dyn std::error::Error`) into a
/// `ToolError`. The raw error should be logged separately — only the
/// classified code + LLM-safe message should reach the model.
///
/// Keywords are checked in a fixed order; the first match wins. For example,
/// `"insufficient funds, account not found"` classifies as
/// `InsufficientFunds`, not `NotFound`, because `insufficient` is checked
/// before `not found`.
pub fn to_tool_error(err: &anyhow::Error) -> ToolError {
    let raw = err.to_string();
    let lc = raw.to_ascii_lowercase();
    let code = if lc.contains("insufficient") || lc.contains("not enough") {
        ToolErrorCode::InsufficientFunds
    } else if lc.contains("timeout") || lc.contains("timed out") {
        ToolErrorCode::Timeout
    } else if lc.contains("not found") || lc.contains("does not exist") {
        ToolErrorCode::NotFound
    } else if lc.contains("slippage") {
        ToolErrorCode::SlippageTooHigh
    } else if lc.contains("price impact") {
        ToolErrorCode::PriceImpactTooHigh
    } else if lc.contains("unauthorized") || lc.contains("not the owner") {
        ToolErrorCode::Unauthorized
    } else if lc.contains("rpc") || lc.contains("fetch") || lc.contains("network") {
        ToolErrorCode::RpcFailure
    } else {
        ToolErrorCode::Generic
    };
    let message = truncate_llm_message(raw);
    ToolError { code, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_insufficient_funds() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("insufficient lamports")).code,
            ToolErrorCode::InsufficientFunds
        );
    }

    #[test]
    fn classifies_timeout() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("RPC request timed out")).code,
            ToolErrorCode::Timeout
        );
    }

    #[test]
    fn classifies_not_found() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("account does not exist")).code,
            ToolErrorCode::NotFound
        );
    }

    #[test]
    fn classifies_slippage() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("slippage tolerance exceeded")).code,
            ToolErrorCode::SlippageTooHigh
        );
    }

    #[test]
    fn classifies_price_impact() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("price impact 5.2% too large")).code,
            ToolErrorCode::PriceImpactTooHigh
        );
    }

    #[test]
    fn classifies_unauthorized() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("caller is not the owner")).code,
            ToolErrorCode::Unauthorized
        );
    }

    #[test]
    fn classifies_rpc_failure() {
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("network unreachable")).code,
            ToolErrorCode::RpcFailure
        );
    }

    #[test]
    fn falls_back_to_generic_with_truncation() {
        let long = "x".repeat(500);
        let err = to_tool_error(&anyhow::anyhow!("{long}"));
        assert_eq!(err.code, ToolErrorCode::Generic);
        assert!(err.message.len() <= 200);
    }

    #[test]
    fn overlap_first_match_wins() {
        // "insufficient" beats "not found"
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("insufficient funds; account not found")).code,
            ToolErrorCode::InsufficientFunds
        );
        // "timed out" beats "rpc"
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("rpc request timed out")).code,
            ToolErrorCode::Timeout
        );
        // "slippage" beats "price impact" (both possible in one message)
        assert_eq!(
            to_tool_error(&anyhow::anyhow!("slippage from price impact too high")).code,
            ToolErrorCode::SlippageTooHigh
        );
    }

    #[test]
    fn truncates_multibyte_input_without_panic() {
        // "😀" is 4 bytes. 100 copies = 400 bytes; any naive truncate at 200
        // would land mid-codepoint and panic.
        let long = "😀".repeat(100);
        let err = to_tool_error(&anyhow::anyhow!("{long}"));
        assert_eq!(err.code, ToolErrorCode::Generic);
        assert!(err.message.len() <= 200);
        // The truncated message must still be valid UTF-8 made of whole
        // emoji (each 4 bytes), so its length is a multiple of 4.
        assert_eq!(err.message.len() % 4, 0);
    }
}
