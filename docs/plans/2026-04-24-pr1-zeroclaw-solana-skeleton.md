# PR 1 — `zeroclaw-solana` Crate Skeleton Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a new `zeroclaw-solana` crate to the workspace containing the pure-data foundation modules (`error_codes`, `tool_result`, `keypair`, `state`, `context`, `client`, `owner`) with unit tests, plus setup commit (SPEC.md + design doc). No tools, no WebSocket, no on-chain writes.

**Architecture:** `zeroclaw-solana` sits next to `zeroclaw-api` at the foundation of the dep graph. It depends on `solana-sdk`, `solana-client`, `mpl-core`, plus `serde`/`anyhow`/`tokio` from the workspace. Downstream crates (`zeroclaw-tools`, `zeroclaw-gateway`) will depend on it in later PRs. Modules are data + trait-shaped; IO (RPC calls) is isolated to `client` + `owner` to keep units testable.

**Tech Stack:** Rust 2024 edition, `solana-sdk` 2.x, `solana-client` 2.x (nonblocking), `mpl-core` 0.x, `bs58` 0.5, `anyhow`, `serde`, `serde_json`, `tokio`, `parking_lot`, `async-trait`. Test deps: `tempfile`, `proptest` (optional).

**Risk tier:** High (per `AGENTS.md` — new crate that later handles keypairs and tx signing). Every task ends in `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo test -p zeroclaw-solana` passing.

---

## Pre-work (completed before plan execution)

- [x] Git worktree created at `.worktrees/feat-zeroclaw-solana-skeleton/` on branch `feat/zeroclaw-solana-skeleton`.
- [x] Baseline `cargo check --workspace` green after `rustup default stable` (1.95).
- [x] `docs/SPEC.md` + `docs/plans/2026-04-24-metaplex-agent-port-design.md` copied into worktree.

## Pre-flight verification at task start

Before Task 1, verify `mpl-agent-registry`, `mpl-genesis` Rust crate availability by running:

```bash
cargo search mpl-agent-registry mpl-genesis mpl-core 2>&1 | head -20
```

Record presence/absence in `docs/plans/2026-04-24-pr1-zeroclaw-solana-skeleton.md` append-only notes at bottom. Absence of `mpl-agent-registry` / `mpl-genesis` does not block PR 1 — those are PR 4 / PR 6 concerns — but `mpl-core` must be present (PR 1 uses it for `owner.rs`). If `mpl-core` is missing, stop and escalate.

---

## Task 1: Scaffold crate + setup commit

**Files:**
- Modify: `Cargo.toml` (workspace members + workspace.dependencies)
- Create: `crates/zeroclaw-solana/Cargo.toml`
- Create: `crates/zeroclaw-solana/src/lib.rs`

**Step 1: Add to workspace members**

Edit `Cargo.toml` line 2, insert `"crates/zeroclaw-solana"` into the `members` array alphabetically between `"crates/zeroclaw-runtime"` and `"crates/zeroclaw-tool-call-parser"`. Final line shape (single line, matches existing style):

```toml
members = [".", "crates/zeroclaw-api", "crates/zeroclaw-infra", "crates/zeroclaw-config", "crates/zeroclaw-providers", "crates/zeroclaw-memory", "crates/zeroclaw-channels", "crates/zeroclaw-tools", "crates/zeroclaw-runtime", "crates/zeroclaw-solana", "crates/zeroclaw-tui", "crates/zeroclaw-plugins", "crates/zeroclaw-gateway", "crates/zeroclaw-hardware", "crates/zeroclaw-tool-call-parser", "crates/robot-kit", "crates/aardvark-sys", "crates/zeroclaw-macros", "apps/tauri"]
```

**Step 2: Add workspace dep entry**

In `Cargo.toml`, add inside `[workspace.dependencies]` (alphabetical, just after the `zeroclaw-runtime` line):

```toml
zeroclaw-solana = { path = "crates/zeroclaw-solana", version = "0.7.3" }
```

Also add external Solana deps in the same table (or `[workspace.dependencies]` — ZeroClaw uses one unified table; append after existing entries):

```toml
solana-sdk = { version = "2.1", default-features = false }
solana-client = { version = "2.1", default-features = false }
solana-transaction-status = { version = "2.1", default-features = false }
spl-token = { version = "7", default-features = false, features = ["no-entrypoint"] }
spl-associated-token-account = { version = "6", default-features = false, features = ["no-entrypoint"] }
mpl-core = { version = "0.8", default-features = false }
bs58 = { version = "0.5", default-features = false, features = ["alloc"] }
```

Version numbers above are best-guess as of 2026-Q2; adjust to latest published after `cargo search` in pre-flight. Record any adjustments in plan notes.

**Step 3: Create `crates/zeroclaw-solana/Cargo.toml`**

```toml
[package]
name = "zeroclaw-solana"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Solana + Metaplex foundation: RPC client, keypair, state, context, owner resolution, error taxonomy."
publish = false

[dependencies]
zeroclaw-api.workspace = true
anyhow = "1.0"
async-trait = "0.1"
bs58.workspace = true
mpl-core.workspace = true
parking_lot = "0.12"
serde = { version = "1.0", default-features = false, features = ["derive"] }
serde_json = { version = "1.0", default-features = false, features = ["std"] }
solana-client.workspace = true
solana-sdk.workspace = true
thiserror = "2.0"
tokio = { version = "1.50", default-features = false, features = ["sync", "time", "rt", "fs"] }
tracing = { version = "0.1", default-features = false }

[dev-dependencies]
tempfile = "3.26"
tokio = { version = "1.50", features = ["rt-multi-thread", "macros", "test-util"] }
```

**Step 4: Create `crates/zeroclaw-solana/src/lib.rs`**

```rust
//! Solana + Metaplex foundation for ZeroClaw-hosted agents.
//!
//! This crate is the port of the Metaplex Agent Template's `@metaplex-agent/shared`
//! package to ZeroClaw. It contains the data structures, error taxonomy, RPC
//! client wrapper, keypair decoder, state file, agent context, and owner resolver.
//! It intentionally has **no LLM or transport concerns** — those live in
//! `zeroclaw-tools` and `zeroclaw-gateway`.
//!
//! See `docs/plans/2026-04-24-metaplex-agent-port-design.md` §1 for the full
//! crate map.
```

**Step 5: Verify workspace still builds**

Run:
```bash
cargo check -p zeroclaw-solana
```
Expected: compiles cleanly; outputs an empty crate.

**Step 6: Commit setup + scaffold**

```bash
git add docs/SPEC.md docs/plans/2026-04-24-metaplex-agent-port-design.md \
        docs/plans/2026-04-24-pr1-zeroclaw-solana-skeleton.md \
        Cargo.toml Cargo.lock crates/zeroclaw-solana/
git commit -m "feat(solana): scaffold zeroclaw-solana crate

Adds an empty zeroclaw-solana crate to the workspace plus the
SPEC.md contract and the multi-PR port design. Follow-up PRs add
the actual modules (keypair, state, context, owner, …).

Refs: docs/plans/2026-04-24-metaplex-agent-port-design.md"
```

---

## Task 2: `error_codes` module (TDD)

**Files:**
- Create: `crates/zeroclaw-solana/src/error_codes.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod error_codes;`)

**Step 1: Write failing test**

Append to `error_codes.rs`:

```rust
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
}
```

**Step 2: Run — expect FAIL**

```bash
cargo test -p zeroclaw-solana error_codes
```
Expected: compile error (`ToolErrorCode`, `to_tool_error` not defined).

**Step 3: Implement**

Write to top of `error_codes.rs`:

```rust
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
    if message.len() > 200 {
        message.truncate(200);
    }
    ToolError { code, message }
}
```

Add `pub mod error_codes;` to `lib.rs`.

**Step 4: Run — expect PASS**

```bash
cargo test -p zeroclaw-solana error_codes
```
Expected: 8 passed.

**Step 5: Commit**

```bash
git add crates/zeroclaw-solana/src/{error_codes.rs,lib.rs}
git commit -m "feat(solana): add ToolErrorCode taxonomy + to_tool_error classifier

Mirrors SPEC §5.3 classifier codes. Raw errors should be logged
server-side; only the taxonomy code + short message returns to the LLM."
```

---

## Task 3: `tool_result` module (TDD)

**Files:**
- Create: `crates/zeroclaw-solana/src/tool_result.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod tool_result;`)

**Step 1: Write failing test**

```rust
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
```

**Step 2: Run — expect FAIL**

```bash
cargo test -p zeroclaw-solana tool_result
```

**Step 3: Implement**

```rust
use crate::error_codes::ToolErrorCode;
use serde::Serialize;

/// Discriminated tool result. Serializes with a `status` field so the LLM
/// can branch on success/info/error uniformly. Matches SPEC §5.2.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolResult<T: Serialize> {
    Success { data: T },
    Info { data: T },
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
    ToolResult::Error { code, message: message.into(), _phantom: std::marker::PhantomData }
}

impl<T: Serialize> From<ToolResult<T>> for zeroclaw_api::tool::ToolResult {
    fn from(r: ToolResult<T>) -> Self {
        let (success, error) = match &r {
            ToolResult::Error { message, .. } => (false, Some(message.clone())),
            _ => (true, None),
        };
        let output = serde_json::to_string(&r)
            .unwrap_or_else(|e| format!("{{\"status\":\"error\",\"code\":\"GENERIC\",\"message\":\"serde failure: {e}\"}}"));
        zeroclaw_api::tool::ToolResult { success, output, error }
    }
}
```

Add `pub mod tool_result;` to `lib.rs`.

**Step 4: Run — expect PASS**

```bash
cargo test -p zeroclaw-solana tool_result
```
Expected: 5 passed.

**Step 5: Commit**

```bash
git add crates/zeroclaw-solana/src/{tool_result.rs,lib.rs}
git commit -m "feat(solana): add discriminated ToolResult + api::ToolResult bridge

Tools internally return ToolResult<T> with a status discriminant
(success/info/error). Into<api::ToolResult> serializes to JSON so
the LLM sees the status field inside the existing output/error shape."
```

---

## Task 4: `keypair` module (TDD)

**Files:**
- Create: `crates/zeroclaw-solana/src/keypair.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod keypair;`)

**Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Well-known devnet keypair (NOT A SECRET — standard test vector).
    const SAMPLE_SECRET_BS58: &str = "4NMwxzmYj2uvHuq8xoqhY8RXg63KSVJM1DXkpbmkUY7YQWuoyQgFnnzn6yo3CMnqZasnNPNuAT2TLwQsCaKkUddp";

    #[test]
    fn decodes_base58_secret() {
        let kp = decode(SAMPLE_SECRET_BS58).expect("decodes");
        assert_eq!(kp.pubkey().to_string().len(), 44); // base58 pubkey
    }

    #[test]
    fn decodes_json_byte_array() {
        let bytes = bs58::decode(SAMPLE_SECRET_BS58).into_vec().unwrap();
        let json_array = serde_json::to_string(&bytes).unwrap();
        let kp = decode(&json_array).expect("decodes");
        assert!(!kp.pubkey().to_string().is_empty());
    }

    #[test]
    fn rejects_malformed() {
        let err = decode("not-valid-anything").unwrap_err();
        assert!(err.to_string().to_lowercase().contains("decode") || err.to_string().to_lowercase().contains("parse"));
    }

    #[test]
    fn rejects_wrong_length_json_array() {
        let bad = serde_json::to_string(&vec![1u8, 2, 3]).unwrap();
        let err = decode(&bad).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("length") || err.to_string().to_lowercase().contains("expected"));
    }
}
```

**Step 2: Run — expect FAIL**

```bash
cargo test -p zeroclaw-solana keypair
```

**Step 3: Implement**

```rust
use anyhow::{anyhow, Context, Result};
use solana_sdk::signature::{Keypair, Signer};

/// Decode an `AGENT_KEYPAIR` env value. Accepts two formats (SPEC §8.1):
///
/// 1. Base58-encoded 64-byte secret key.
/// 2. JSON byte array `[1, 2, ...]` of length 64.
///
/// On success, returns a loaded `solana_sdk::signature::Keypair`.
pub fn decode(raw: &str) -> Result<Keypair> {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(trimmed)
            .context("AGENT_KEYPAIR: expected JSON byte array")?;
        if bytes.len() != 64 {
            return Err(anyhow!(
                "AGENT_KEYPAIR: JSON byte array has length {}, expected 64",
                bytes.len()
            ));
        }
        Keypair::from_bytes(&bytes).map_err(|e| anyhow!("AGENT_KEYPAIR: {e}"))
    } else {
        let bytes = bs58::decode(trimmed)
            .into_vec()
            .context("AGENT_KEYPAIR: base58 decode failed")?;
        if bytes.len() != 64 {
            return Err(anyhow!(
                "AGENT_KEYPAIR: base58 secret has length {}, expected 64",
                bytes.len()
            ));
        }
        Keypair::from_bytes(&bytes).map_err(|e| anyhow!("AGENT_KEYPAIR: {e}"))
    }
}

/// Convenience: wrap `decode(std::env::var("AGENT_KEYPAIR")?)`.
pub fn from_env() -> Result<Keypair> {
    let raw = std::env::var("AGENT_KEYPAIR").context("AGENT_KEYPAIR is not set")?;
    decode(&raw)
}

// Re-export for downstream crates that want Signer / Keypair without adding
// solana-sdk to their direct deps.
pub use solana_sdk::signature::{Keypair as SolanaKeypair, Signer as SolanaSigner};
```

Add `pub mod keypair;` to `lib.rs`.

**Step 4: Run — expect PASS**

```bash
cargo test -p zeroclaw-solana keypair
```

Note: the SAMPLE_SECRET_BS58 in the test must be a *real* base58-encoded 64-byte secret. If the one provided above doesn't satisfy that (common false-start), regenerate inside the test itself via `Keypair::new()` and serialize both formats:

```rust
let kp = Keypair::new();
let bs58 = bs58::encode(&kp.to_bytes()).into_string();
let json = serde_json::to_string(&kp.to_bytes().to_vec()).unwrap();
// ...assert both decode back to kp.pubkey()
```

**Step 5: Commit**

```bash
git add crates/zeroclaw-solana/src/{keypair.rs,lib.rs}
git commit -m "feat(solana): add AGENT_KEYPAIR decoder (base58 + JSON byte array)

Matches SPEC §8.1. Accepts both Solana CLI byte-array JSON format
and Phantom-style base58 secret. Rejects malformed/wrong-length input."
```

---

## Task 5: `state` module (TDD)

**Files:**
- Create: `crates/zeroclaw-solana/src/state.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod state;`)

**Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trips_empty_state() {
        let d = tempdir().unwrap();
        let path = d.path().join("agent-state.json");
        let loaded = load_at(&path).unwrap();
        assert!(loaded.agent_asset_address.is_none());
        assert!(loaded.agent_token_mint.is_none());
    }

    #[test]
    fn writes_and_reads_back() {
        let d = tempdir().unwrap();
        let path = d.path().join("agent-state.json");
        let mut s = AgentState::default();
        s.agent_asset_address = Some("ARsZN4kZUWNX8Ek5ZkVUpRitSFjym6p9jLvUxrg9kPWU".into());
        save_at(&path, &s).unwrap();
        let back = load_at(&path).unwrap();
        assert_eq!(back.agent_asset_address.as_deref(), Some("ARsZN4kZUWNX8Ek5ZkVUpRitSFjym6p9jLvUxrg9kPWU"));
    }

    #[test]
    fn write_is_atomic_tempfile_cleanup() {
        let d = tempdir().unwrap();
        let path = d.path().join("agent-state.json");
        let s = AgentState { agent_asset_address: Some("X".into()), agent_token_mint: None };
        save_at(&path, &s).unwrap();
        // .tmp should not exist after rename
        assert!(!d.path().join("agent-state.json.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writes_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir().unwrap();
        let path = d.path().join("agent-state.json");
        save_at(&path, &AgentState::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
```

**Step 2: Run — expect FAIL**

```bash
cargo test -p zeroclaw-solana state
```

**Step 3: Implement**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Agent identity that persists across restarts (SPEC §4.3).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_asset_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_token_mint: Option<String>,
}

/// Load `agent-state.json` from a specific path. Missing file → `AgentState::default()`.
pub fn load_at(path: &Path) -> Result<AgentState> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AgentState::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Atomic write: tmp + fsync + rename. Sets mode 0600 on unix.
pub fn save_at(path: &Path, state: &AgentState) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(state)?;
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .create(true).truncate(true).write(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("open {}", tmp.display()))?;
            std::io::Write::write_all(&mut f, &json)?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp, &json)
                .with_context(|| format!("write {}", tmp.display()))?;
        }
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Discover the workspace root by walking up from cwd looking for `Cargo.toml`
/// containing a `[workspace]` table. Used to anchor `agent-state.json` per SPEC §4.3.
pub fn workspace_state_path() -> Result<PathBuf> {
    let mut cur = std::env::current_dir().context("cwd")?;
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate).unwrap_or_default();
            if content.contains("[workspace]") {
                return Ok(cur.join("agent-state.json"));
            }
        }
        if !cur.pop() {
            return Err(anyhow::anyhow!("workspace root not found walking up from cwd"));
        }
    }
}
```

Add `pub mod state;` to `lib.rs`.

**Step 4: Run — expect PASS**

```bash
cargo test -p zeroclaw-solana state
```

**Step 5: Commit**

```bash
git add crates/zeroclaw-solana/src/{state.rs,lib.rs}
git commit -m "feat(solana): add atomic agent-state.json read/write

Matches SPEC §4.3. Atomic write via tmp + fsync + rename, mode 0600
on unix, workspace-root anchoring via walk-up search."
```

---

## Task 6: `context` module (types only — no test)

**Files:**
- Create: `crates/zeroclaw-solana/src/context.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod context;`)

**Step 1: Write module**

```rust
//! `AgentContext` and supporting types — injected into tools via `tokio::task_local!`
//! (plumbing set up in PR 3).

use crate::error_codes::ToolErrorCode;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode { Public, Autonomous }

/// Options for a transaction prompted to a connected user wallet.
#[derive(Debug, Clone, Default)]
pub struct TxSendOpts {
    pub message: Option<String>,
    pub index: Option<u32>,
    pub total: Option<u32>,
    pub fee_sol: Option<f64>,
}

/// Public-mode per-session transaction sender. Implementations emit
/// a `transaction` WS message and await the client's `tx_result` /
/// `tx_error` callback.
#[async_trait]
pub trait TxSender: Send + Sync {
    async fn send_and_await(
        &self,
        tx_base64: String,
        opts: TxSendOpts,
    ) -> Result<Signature, TxSendError>;
}

/// Errors from `TxSender::send_and_await` — already sanitized for the LLM.
#[derive(Debug, thiserror::Error)]
pub enum TxSendError {
    #[error("user rejected or wallet error: {0}")]
    Rejected(String),
    #[error("transaction prompt timed out")]
    Timeout,
    #[error("client disconnected")]
    Disconnected,
    #[error("unknown correlation id")]
    UnknownCorrelation,
    #[error("invalid mode for transaction prompt")]
    InvalidMode,
}

impl TxSendError {
    pub fn as_tool_error_code(&self) -> ToolErrorCode {
        match self {
            TxSendError::Timeout => ToolErrorCode::Timeout,
            TxSendError::Rejected(_) | TxSendError::Disconnected
            | TxSendError::UnknownCorrelation | TxSendError::InvalidMode => ToolErrorCode::Generic,
        }
    }
}

/// Everything a tool may need to know about the calling session + server config.
/// Matches SPEC Appendix B (AgentContext).
#[derive(Clone)]
pub struct AgentContext {
    pub wallet_address: Option<Pubkey>,
    pub agent_mode: AgentMode,
    pub agent_asset_address: Option<Pubkey>,
    pub agent_token_mint: Option<Pubkey>,
    pub agent_fee_sol: f64,
    pub token_override: Option<Pubkey>,
    pub owner_wallet: Option<Pubkey>,
    pub tx_sender: Option<Arc<dyn TxSender>>,
    pub solana: Arc<crate::client::SolanaClient>,
}

impl std::fmt::Debug for AgentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentContext")
            .field("wallet_address", &self.wallet_address)
            .field("agent_mode", &self.agent_mode)
            .field("agent_asset_address", &self.agent_asset_address)
            .field("agent_token_mint", &self.agent_token_mint)
            .field("agent_fee_sol", &self.agent_fee_sol)
            .field("token_override", &self.token_override)
            .field("owner_wallet", &self.owner_wallet)
            .field("tx_sender", &self.tx_sender.as_ref().map(|_| "<dyn TxSender>"))
            .finish()
    }
}
```

**Step 2: Verify compiles**

```bash
cargo check -p zeroclaw-solana
```
Expected: compiles. Will reference `crate::client::SolanaClient` which doesn't exist yet — that forward reference is fine because we add `client` in Task 7; for now stub `SolanaClient` temporarily in `lib.rs`:

```rust
// lib.rs — temporary forward stub, replaced in Task 7.
pub mod client { pub struct SolanaClient; }
```

Alternative: reorder — do Task 7 before Task 6. Either works. Plan executor should pick whichever yields the smaller diff.

**Step 3: Commit**

```bash
git add crates/zeroclaw-solana/src/{context.rs,lib.rs}
git commit -m "feat(solana): add AgentContext, TxSender trait, AgentMode

Matches SPEC Appendix B. AgentContext carries everything a tool
needs per-call; TxSender is the public-mode async seam tools call
to prompt a user-signed transaction over PlexChat (wired in PR 3)."
```

---

## Task 7: `client` module (thin RPC wrapper)

**Files:**
- Create: `crates/zeroclaw-solana/src/client.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (replace stub `mod client` with real module)

**Step 1: Implement**

```rust
//! Thin async wrapper around `solana_client::nonblocking::rpc_client::RpcClient`.
//!
//! All RPC calls tools make go through this client so tests can swap in a
//! mock by constructing `SolanaClient::from_rpc(Arc<dyn RpcLike>)` (added
//! when first tool needs it — PR 3).

use anyhow::{Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use std::sync::Arc;
use std::time::Duration;

pub struct SolanaClient {
    pub rpc: Arc<RpcClient>,
    pub rpc_url: String,
}

impl SolanaClient {
    /// Construct from a raw RPC URL. Uses `CommitmentConfig::confirmed()` by default.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        let url = rpc_url.into();
        let rpc = Arc::new(RpcClient::new_with_timeout_and_commitment(
            url.clone(),
            Duration::from_secs(30),
            CommitmentConfig::confirmed(),
        ));
        Self { rpc, rpc_url: url }
    }

    /// Preflight check: prove the RPC endpoint is reachable (SPEC §6.8).
    pub async fn preflight(&self) -> Result<u64> {
        self.rpc.get_slot().await.context("RPC preflight get_slot failed")
    }
}
```

**Step 2: Verify**

```bash
cargo check -p zeroclaw-solana
cargo clippy -p zeroclaw-solana --all-targets -- -D warnings
```

**Step 3: Commit**

```bash
git add crates/zeroclaw-solana/src/{client.rs,lib.rs}
git commit -m "feat(solana): add SolanaClient RPC wrapper with preflight

Thin async wrapper around solana_client::nonblocking::RpcClient,
confirmed commitment, 30s timeout. preflight() implements SPEC §6.8
step 2 (get_slot reachability check)."
```

---

## Task 8: `owner` module (TDD with a trait abstraction for RPC)

**Files:**
- Create: `crates/zeroclaw-solana/src/owner.rs`
- Modify: `crates/zeroclaw-solana/src/lib.rs` (add `pub mod owner;`)

**Step 1: Write failing test**

The goal is an `OwnerCache` that, given `(asset_address, mpl_core_fetch_fn)`, caches the result with a TTL. Isolate RPC from the test with a small trait:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use solana_sdk::pubkey::Pubkey;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct MockFetcher {
        calls: AtomicU32,
        owner: Pubkey,
    }
    #[async_trait]
    impl AssetOwnerFetcher for MockFetcher {
        async fn fetch_owner(&self, _asset: &Pubkey) -> anyhow::Result<Option<Pubkey>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.owner))
        }
    }

    fn pk(seed: u8) -> Pubkey { Pubkey::new_from_array([seed; 32]) }

    #[tokio::test]
    async fn returns_bootstrap_when_asset_none() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher { calls: AtomicU32::new(0), owner: pk(1) };
        let got = resolve_owner(&f, &cache, None, Some(pk(9))).await.unwrap();
        assert_eq!(got, Some(pk(9)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fetches_and_caches_on_chain_owner() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher { calls: AtomicU32::new(0), owner: pk(3) };
        let asset = pk(2);
        let got1 = resolve_owner(&f, &cache, Some(asset), Some(pk(9))).await.unwrap();
        let got2 = resolve_owner(&f, &cache, Some(asset), Some(pk(9))).await.unwrap();
        assert_eq!(got1, Some(pk(3)));
        assert_eq!(got2, Some(pk(3)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "cached on second call");
    }

    #[tokio::test]
    async fn refetches_after_ttl_elapses() {
        let cache = OwnerCache::new(Duration::from_millis(10));
        let f = MockFetcher { calls: AtomicU32::new(0), owner: pk(5) };
        let asset = pk(4);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalidate_forces_refetch() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher { calls: AtomicU32::new(0), owner: pk(7) };
        let asset = pk(6);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        cache.invalidate(&asset);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }
}
```

**Step 2: Run — expect FAIL**

```bash
cargo test -p zeroclaw-solana owner
```

**Step 3: Implement**

```rust
use async_trait::async_trait;
use parking_lot::Mutex;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Trait abstracting the MPL Core asset fetch so unit tests can mock it.
/// The real impl (`MplCoreFetcher`) is added when first used (PR 3).
#[async_trait]
pub trait AssetOwnerFetcher: Send + Sync {
    async fn fetch_owner(&self, asset: &Pubkey) -> anyhow::Result<Option<Pubkey>>;
}

struct CachedOwner {
    owner: Option<Pubkey>,
    expires_at: Instant,
}

pub struct OwnerCache {
    inner: Mutex<HashMap<Pubkey, CachedOwner>>,
    ttl: Duration,
}

impl OwnerCache {
    pub fn new(ttl: Duration) -> Self {
        Self { inner: Mutex::new(HashMap::new()), ttl }
    }

    fn get(&self, asset: &Pubkey) -> Option<Option<Pubkey>> {
        let g = self.inner.lock();
        g.get(asset).and_then(|c| {
            if c.expires_at > Instant::now() { Some(c.owner) } else { None }
        })
    }

    fn put(&self, asset: Pubkey, owner: Option<Pubkey>) {
        let mut g = self.inner.lock();
        g.insert(asset, CachedOwner { owner, expires_at: Instant::now() + self.ttl });
    }

    pub fn invalidate(&self, asset: &Pubkey) {
        self.inner.lock().remove(asset);
    }
}

/// Resolve the agent owner per SPEC §3.4:
/// 1. If `agent_asset_address` is set → fetch on-chain owner (cached).
/// 2. Else if `bootstrap_wallet` is set → return it.
/// 3. Else → `None` (autonomous mode should refuse to serve).
pub async fn resolve_owner(
    fetcher: &dyn AssetOwnerFetcher,
    cache: &OwnerCache,
    agent_asset_address: Option<Pubkey>,
    bootstrap_wallet: Option<Pubkey>,
) -> anyhow::Result<Option<Pubkey>> {
    if let Some(asset) = agent_asset_address {
        if let Some(cached) = cache.get(&asset) { return Ok(cached); }
        let owner = fetcher.fetch_owner(&asset).await?;
        cache.put(asset, owner);
        return Ok(owner);
    }
    Ok(bootstrap_wallet)
}
```

Add `pub mod owner;` to `lib.rs`.

**Step 4: Run — expect PASS**

```bash
cargo test -p zeroclaw-solana owner
```
Expected: 4 passed.

**Step 5: Commit**

```bash
git add crates/zeroclaw-solana/src/{owner.rs,lib.rs}
git commit -m "feat(solana): add OwnerCache + resolve_owner

Process-global TTL cache for on-chain owner lookups, trait-abstracted
fetcher for unit tests. Matches SPEC §3.4 resolution order:
on-chain asset > BOOTSTRAP_WALLET > None."
```

---

## Task 9: Finalize `lib.rs` exports + top-level doc

**Files:**
- Modify: `crates/zeroclaw-solana/src/lib.rs`

**Step 1: Write**

```rust
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

pub mod client;
pub mod context;
pub mod error_codes;
pub mod keypair;
pub mod owner;
pub mod state;
pub mod tool_result;

pub use context::{AgentContext, AgentMode, TxSender, TxSendError, TxSendOpts};
pub use error_codes::{ToolError, ToolErrorCode, to_tool_error};
pub use tool_result::{err, info, ok, ToolResult};
```

**Step 2: Verify**

```bash
cargo fmt --all -- --check
cargo clippy -p zeroclaw-solana --all-targets -- -D warnings
cargo test -p zeroclaw-solana
```

**Step 3: Commit**

```bash
git add crates/zeroclaw-solana/src/lib.rs
git commit -m "docs(solana): crate-level module map + re-exports"
```

---

## Task 10: Final CI gate + PR prep

**Step 1: Full workspace validation**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If `./dev/ci.sh all` is available and quick enough, run it:

```bash
./dev/ci.sh all
```

**Step 2: Verify `git log` shape**

```bash
git log --oneline master..HEAD
```

Expected ~8–9 commits, all conventional, none touching unrelated crates.

**Step 3: Push branch + open PR**

```bash
git push -u origin feat/zeroclaw-solana-skeleton
```

Then (via `gh`) open a PR with body following `.github/pull_request_template.md`:

- Title: `feat(solana): add zeroclaw-solana crate skeleton`
- Size: S
- Risk: High (new foundation crate)
- Depends on: (none — first PR of the chain)
- Blocks: PR 2 (PlexChat WS route)
- Link the design doc + SPEC.md as rationale.

Do not push + PR without user confirmation, since push is a shared-state action per `CLAUDE.md` tone rules.

---

## Append-only notes

(Filled during execution — crate versions observed from `cargo search`, any IDL decisions, surprises, etc.)

### Task 1 (2026-04-24) — version adjustments

The Solana ecosystem has moved well past 2.x since the plan was written. `cargo search` + crates.io API showed:

| Crate | Plan proposal | Latest stable observed | Used |
| --- | --- | --- | --- |
| `solana-sdk` | `2.1` | `4.0.1` (and `3.0.0` last pre-4) | `3` |
| `solana-client` | `2.1` | `3.1.14` (4.x line still in `beta.7`) | `3` |
| `solana-transaction-status` | `2.1` | `3.1.14` (4.x line still `rc.0`) | `3` |
| `spl-token` | `7` | `9.0.0` | `9` |
| `spl-associated-token-account` | `6` | `8.0.0` | `8` |
| `mpl-core` | `0.8` | `0.12.0` | `0.12` |
| `bs58` | `0.5` | `0.5.1` | `0.5` |

Chose the `3.x` line for the three Anza/Solana crates (`solana-sdk`, `solana-client`, `solana-transaction-status`) because `solana-client` + `solana-transaction-status` have no stable `4.x` yet (still `4.0.0-beta.7` / `4.0.0-rc.0`). Mixing `solana-sdk = 4` with `solana-client = 3` risks ABI drift, so I pinned all three to `3`. When the 4.x line ships stable we can bump them together.

**`mpl-core` / `borsh-v1` rationale.** `mpl-core 0.12` pulls `kaigan 0.5` which depends on both `borsh ^0.10.3` and `borsh ^1.5` unconditionally. The `kaigan/borsh-v1` feature (enabled transitively by `mpl-core/borsh-v1`) is defined as `[]` — it toggles kaigan's *internal `cfg` code paths* onto the `borsh 1.x` trait impls, which is what `mpl-core`'s generated `asset_signer.rs` expects. Without it, kaigan compiles the `borsh 0.10` paths and `RemainderVec<u8>: BorshSerialize` fails to resolve against mpl-core's `borsh 1.x` expectations. Both borsh major versions remain in the build graph; that is intrinsic to `kaigan 0.5` and not something the feature flag removes. Verify with `cargo tree -p zeroclaw-solana -i borsh@0.10` — both `0.10.x` and `1.x` should appear. Note that `mpl-core 0.12`'s `default = ["borsh-v1"]`, so being explicit here is intentional guard against upstream default drift rather than a strict necessity.

`spl-token`, `spl-associated-token-account`, and `solana-transaction-status` are declared in `[workspace.dependencies]` for future consumers but are **not** pulled by `zeroclaw-solana` itself yet (per the Task 1 crate manifest). They'll be activated by later PRs when those modules land. Their versions above are the best-guess latest majors; revisit on first use.

`cargo check -p zeroclaw-solana` and `cargo check --workspace` both pass on `rustc 1.95` with these pins.

### Task 4 (2026-04-24) — solana-sdk 3.x API drift

The Task 4 plan snippet called `Keypair::from_bytes(&bytes)`, which does not exist in `solana-sdk 3.x`. In the 3.x line, `solana_keypair::Keypair` (re-exported as `solana_sdk::signature::Keypair` behind the `full` feature) now implements `TryFrom<&[u8]>` instead, returning `Result<Keypair, SignatureError>`. Adjusted the decoder and tests to use `Keypair::try_from(&bytes[..])`. Length validation stays explicit (`bytes.len() != 64`) so we return the plan's intended "length N, expected 64" message rather than the library's generic "candidate keypair byte array is the wrong length".

Had to add `features = ["full"]` to the `solana-sdk` dep in `crates/zeroclaw-solana/Cargo.toml`. The workspace declares `solana-sdk = { version = "3", default-features = false }`, and in 3.x the entire `signature` / `signer` modules (including `Keypair`, `Signer`, `SignerError`) are gated on `#[cfg(feature = "full")]`. Without `full`, `solana_sdk::signature::Keypair` doesn't resolve. Enabling `full` only on this crate keeps the workspace default minimal for other future consumers while giving `zeroclaw-solana` the signer surface SPEC §8.1 requires.

Used `Keypair::new()` in the tests to generate fresh bytes at test time (the deviation from the plan's SAMPLE_SECRET_BS58 approach, explicitly allowed by Task 4 self-review). Avoids any hardcoded-secret lint traps and keeps the test self-contained.

`cargo test -p zeroclaw-solana` → 25 passed (18 existing + 7 new). `cargo clippy -p zeroclaw-solana --all-targets -- -D warnings` and `cargo fmt --all -- --check` both exit 0. The `pub use solana_sdk::signature::{Keypair as SolanaKeypair, Signer as SolanaSigner};` re-export compiled without stutter warnings under `-D warnings`.

### Task 7 (2026-04-24) — executed before Task 6; solana-commitment-config split-out crate

Executed Task 7 ahead of Task 6 per the Task 6 Step 2 "alternative: reorder" note, so the `context` module can reference `SolanaClient` without a stub.

The Task 7 plan snippet imported `solana_sdk::commitment_config::CommitmentConfig`. This path does not exist in `solana-sdk 3.x`. Unlike `Keypair` (which 3.x keeps re-exported under `solana_sdk::signature::Keypair` behind `full`), `CommitmentConfig` was extracted to its own `solana-commitment-config` crate and is NOT re-exported by `solana-sdk 3.0.0` at all — no feature flag brings it back. `solana-client 3.1.14` also does not re-export it (verified by inspecting `src/lib.rs` — only `rpc_client_api::*` modules are re-exported under named submodules).

Options considered: (1) rely on the transitive `solana_commitment_config` crate without declaring it — rejected as fragile (breaks if upstream dep graph shifts); (2) add `solana-commitment-config` as a direct `zeroclaw-solana` dep — chosen. Pinned to `version = "3", default-features = false` matching the Anza-ecosystem 3.x line already locked by Task 1. Kept it crate-local rather than promoting to `[workspace.dependencies]` because no other crate needs it yet; promote on first second-consumer per the same rule Task 1 used for `spl-token`.

`solana-client 3.1.14` resolved `nonblocking::rpc_client::RpcClient` without any feature flags — the "if you need features on solana-client" branch of Task 7 did not trigger.

`cargo test -p zeroclaw-solana` → 37 passed (35 existing + 2 new). `cargo clippy -p zeroclaw-solana --all-targets -- -D warnings` and `cargo fmt --all -- --check` both exit 0. The unreachable-RPC test hits `http://127.0.0.1:1` and asserts the error message contains `preflight`, `get_slot`, or `connection`; observed error matches (anyhow context wrapping preserves "RPC preflight get_slot failed" prefix).
