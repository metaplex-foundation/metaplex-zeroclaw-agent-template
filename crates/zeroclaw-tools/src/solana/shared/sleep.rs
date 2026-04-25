//! `sleep` — pause for 1..=300 seconds. SPEC §5.1.
//!
//! Used by the agent for polling / monitoring loops without spinning.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult as ApiToolResult};
use zeroclaw_solana::error_codes::ToolErrorCode;
use zeroclaw_solana::tool_result::{ToolResult, err, ok};

const MIN_SECONDS: u64 = 1;
const MAX_SECONDS: u64 = 300;

#[derive(Debug, Deserialize)]
struct Args {
    seconds: f64,
}

pub struct Sleep;

#[async_trait]
impl Tool for Sleep {
    fn name(&self) -> &str {
        "sleep"
    }

    fn description(&self) -> &str {
        "Pause execution for between 1 and 300 seconds. Useful for polling \
         loops (price watch, transaction confirmation, status checks). Inputs \
         outside the range are clamped."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 300,
                    "description": "Duration to sleep, in seconds. Clamped to [1, 300]."
                }
            },
            "required": ["seconds"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ApiToolResult> {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                let r: ToolResult<()> = err(
                    ToolErrorCode::InvalidInput,
                    format!("invalid sleep arguments: {e}"),
                );
                return Ok(r.into());
            }
        };
        if !parsed.seconds.is_finite() || parsed.seconds < 0.0 {
            let r: ToolResult<()> = err(
                ToolErrorCode::InvalidInput,
                "seconds must be a finite, non-negative number",
            );
            return Ok(r.into());
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let raw = parsed.seconds.round() as u64;
        let clamped = raw.clamp(MIN_SECONDS, MAX_SECONDS);
        tokio::time::sleep(Duration::from_secs(clamped)).await;
        Ok(ok(json!({ "slept": clamped })).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn sleeps_for_clamped_seconds() {
        let r = Sleep.execute(json!({ "seconds": 2 })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("\"slept\":2"));
    }

    #[tokio::test(start_paused = true)]
    async fn clamps_above_max() {
        let r = Sleep.execute(json!({ "seconds": 9999 })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("\"slept\":300"));
    }

    #[tokio::test(start_paused = true)]
    async fn clamps_below_min() {
        let r = Sleep.execute(json!({ "seconds": 0 })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("\"slept\":1"));
    }

    #[tokio::test]
    async fn rejects_non_finite() {
        let r = Sleep
            .execute(json!({ "seconds": f64::INFINITY }))
            .await
            .unwrap();
        // serde_json refuses to encode infinity, so it never reaches Args
        // — but a finite negative still routes through the validate path.
        assert!(!r.success);
    }

    #[tokio::test]
    async fn rejects_missing_arg() {
        let r = Sleep.execute(json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }

    #[tokio::test]
    async fn rejects_negative() {
        let r = Sleep.execute(json!({ "seconds": -5 })).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("INVALID_INPUT"));
    }
}
