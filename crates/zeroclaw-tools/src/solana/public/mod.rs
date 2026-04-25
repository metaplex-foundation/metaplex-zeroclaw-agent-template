//! Public-mode-only Solana tools (SPEC §5.1).
//!
//! Reserved for `transfer-sol` and `transfer-token`, which require the
//! `submit_or_send` write path landed by PR 4. Empty until then.

use std::sync::Arc;
use zeroclaw_api::tool::Tool;

pub fn public_tools() -> Vec<Arc<dyn Tool>> {
    Vec::new()
}
