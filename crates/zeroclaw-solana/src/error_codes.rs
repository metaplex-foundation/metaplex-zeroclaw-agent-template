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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
}

/// Maximum byte length of the LLM-safe message. Classifiers truncate at the
/// nearest char boundary at or below this value.
const MAX_MESSAGE_BYTES: usize = 200;

/// Classify an `anyhow::Error` (or `&dyn std::error::Error`) into a
/// `ToolError`. The raw error should be logged separately — only the
/// classified code + LLM-safe message should reach the model.
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
    let mut message = raw;
    if message.len() > MAX_MESSAGE_BYTES {
        // `String::truncate` panics when the byte index lands inside a
        // multi-byte UTF-8 character. Walk back to the nearest char
        // boundary before truncating.
        let mut cut = MAX_MESSAGE_BYTES;
        while cut > 0 && !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
    }
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
