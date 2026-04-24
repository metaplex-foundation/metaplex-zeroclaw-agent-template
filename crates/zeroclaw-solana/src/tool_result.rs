//! Discriminated tool-result type + bridge to `zeroclaw_api::tool::ToolResult`.
//!
//! Tools return `ToolResult<T>` with a `status` discriminant (`success`, `info`,
//! `error`). The `Into<api::ToolResult>` bridge serializes the typed variant to
//! JSON and sets the envelope flags for the gateway. Mirrors SPEC §5.2 / §5.3.

use crate::error_codes::ToolErrorCode;
use serde::Serialize;

/// Discriminated tool result. Serializes with a `status` field so the LLM
/// can branch on success/info/error uniformly. Matches SPEC §5.2.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolResult<T: Serialize> {
    Success {
        data: T,
    },
    Info {
        data: T,
    },
    Error {
        code: ToolErrorCode,
        message: String,
        #[serde(skip)]
        _phantom: std::marker::PhantomData<T>,
    },
}

pub fn ok<T: Serialize>(data: T) -> ToolResult<T> {
    ToolResult::Success { data }
}

pub fn info<T: Serialize>(data: T) -> ToolResult<T> {
    ToolResult::Info { data }
}

pub fn err<T: Serialize>(code: ToolErrorCode, message: impl Into<String>) -> ToolResult<T> {
    let message = crate::error_codes::truncate_llm_message(message.into());
    ToolResult::Error {
        code,
        message,
        _phantom: std::marker::PhantomData,
    }
}

impl<T: Serialize> From<ToolResult<T>> for zeroclaw_api::tool::ToolResult {
    fn from(r: ToolResult<T>) -> Self {
        let serialized = serde_json::to_string(&r);
        match (&r, serialized) {
            (ToolResult::Error { message, .. }, Ok(output)) => zeroclaw_api::tool::ToolResult {
                success: false,
                output,
                error: Some(message.clone()),
            },
            (_, Ok(output)) => zeroclaw_api::tool::ToolResult {
                success: true,
                output,
                error: None,
            },
            (_, Err(e)) => {
                let reason = format!("tool-result serialization failed: {e}");
                zeroclaw_api::tool::ToolResult {
                    success: false,
                    output: "{\"status\":\"error\",\"code\":\"GENERIC\",\"message\":\"serialization failure\"}".to_string(),
                    error: Some(reason),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_codes::ToolErrorCode;
    use serde_json::json;

    #[test]
    fn ok_serializes_with_success_status() {
        let r = ok(json!({ "sol": 1.5 }));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "success");
        assert_eq!(v["data"]["sol"], 1.5);
    }

    #[test]
    fn info_serializes_with_info_status() {
        let r = info(json!({ "note": "already registered" }));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "info");
    }

    #[test]
    fn err_serializes_with_error_status_and_code() {
        let r: ToolResult<()> = err(ToolErrorCode::NotRegistered, "agent not registered");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "error");
        assert_eq!(v["code"], "NOT_REGISTERED");
        assert_eq!(v["message"], "agent not registered");
    }

    #[test]
    fn bridges_to_api_tool_result_on_success() {
        let r = ok(json!({ "ok": true }));
        let api: zeroclaw_api::tool::ToolResult = r.into();
        assert!(api.success);
        assert!(api.error.is_none());
        assert!(api.output.contains("\"status\":\"success\""));
    }

    #[test]
    fn bridges_to_api_tool_result_on_err() {
        let r: ToolResult<()> = err(ToolErrorCode::Integrity, "delta mismatch");
        let api: zeroclaw_api::tool::ToolResult = r.into();
        assert!(!api.success);
        assert_eq!(api.error.as_deref(), Some("delta mismatch"));
    }

    #[test]
    fn bridges_to_api_tool_result_on_info() {
        let r = info(serde_json::json!({ "note": "already registered" }));
        let api: zeroclaw_api::tool::ToolResult = r.into();
        assert!(api.success);
        assert!(api.error.is_none());
        assert!(api.output.contains("\"status\":\"info\""));
    }

    #[test]
    fn bridges_serialization_failure_as_error() {
        use serde::ser::Error;
        use serde::{Serialize, Serializer};

        struct AlwaysFails;
        impl Serialize for AlwaysFails {
            fn serialize<S: Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(S::Error::custom("intentional test failure"))
            }
        }

        let r = ok(AlwaysFails);
        let api: zeroclaw_api::tool::ToolResult = r.into();
        assert!(
            !api.success,
            "failed serialization must not masquerade as success"
        );
        assert!(api.error.as_deref().unwrap_or("").contains("serialization"));
        assert!(api.output.contains("\"status\":\"error\""));
    }

    #[test]
    fn err_truncates_long_message_to_under_200_bytes() {
        let long: String = "x".repeat(500);
        let r: ToolResult<()> = err(ToolErrorCode::Generic, long);
        let v = serde_json::to_value(&r).unwrap();
        let msg = v["message"].as_str().unwrap();
        assert!(msg.len() <= 200);
    }
}
