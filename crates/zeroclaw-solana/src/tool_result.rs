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
    ToolResult::Error {
        code,
        message: message.into(),
        _phantom: std::marker::PhantomData,
    }
}

impl<T: Serialize> From<ToolResult<T>> for zeroclaw_api::tool::ToolResult {
    fn from(r: ToolResult<T>) -> Self {
        let (success, error) = match &r {
            ToolResult::Error { message, .. } => (false, Some(message.clone())),
            _ => (true, None),
        };
        let output = serde_json::to_string(&r).unwrap_or_else(|e| {
            format!(
                "{{\"status\":\"error\",\"code\":\"GENERIC\",\"message\":\"serde failure: {e}\"}}"
            )
        });
        zeroclaw_api::tool::ToolResult {
            success,
            output,
            error,
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
}
