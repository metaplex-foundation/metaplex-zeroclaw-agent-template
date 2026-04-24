//! `agent-state.json` read/write — auto-persisted agent identity (SPEC §4.3).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Agent identity that persists across restarts (SPEC §4.3).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    /// Base58-encoded Pubkey. Callers parse with `solana_sdk::pubkey::Pubkey::from_str`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_asset_address: Option<String>,
    /// Base58-encoded Pubkey. Callers parse with `solana_sdk::pubkey::Pubkey::from_str`.
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

/// Atomic write: tmp + fsync + rename + best-effort parent-dir fsync. Sets
/// mode 0600 on unix.
///
/// Concurrency: `save_at` is **not** safe for concurrent callers on the same
/// path — two writers racing can both create the `.tmp` sidecar and both
/// rename, yielding last-writer-wins. This is acceptable for agent state,
/// which is only written by single-flight tool handlers.
///
/// Crash safety: the rename is atomic on POSIX filesystems, so an interrupt
/// between `.tmp` creation and `rename` leaves the original `agent-state.json`
/// intact (only a stray `.tmp` sidecar may remain). After `rename` returns we
/// make a best-effort attempt to `fsync` the parent directory so the new
/// directory entry is durable; this is best-effort because some filesystems
/// (notably tmpfs and certain NFS mounts) reject `fsync` on directory
/// handles, in which case an interrupt *after* `rename` returned may still
/// leave the directory entry un-flushed even though the file contents are
/// durable.
///
/// On non-unix targets, `save_at` writes without fsync and without
/// permission restriction; ZeroClaw server deployment is unix-only.
pub fn save_at(path: &Path, state: &AgentState) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(state)?;
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("open {}", tmp.display()))?;
            std::io::Write::write_all(&mut f, &json)?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
        }
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        // Best-effort — tmpfs and some NFS mounts reject fsync on directory handles.
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// Discover the workspace root by walking up from `start` looking for a
/// `Cargo.toml` containing a `[workspace]` table, then anchor
/// `agent-state.json` at that root (SPEC §4.3).
///
/// Split out from [`workspace_state_path`] so tests can inject a synthetic
/// start directory without touching the process-global cwd.
pub fn workspace_state_path_from(start: &Path) -> Result<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.exists() {
            let content = std::fs::read_to_string(&candidate).unwrap_or_default();
            if content
                .lines()
                .any(|l| l.trim_start().starts_with("[workspace]"))
            {
                return Ok(cur.join("agent-state.json"));
            }
        }
        if !cur.pop() {
            return Err(anyhow::anyhow!(
                "workspace root not found walking up from {}",
                start.display()
            ));
        }
    }
}

/// Convenience wrapper around [`workspace_state_path_from`] that starts from
/// the current working directory.
pub fn workspace_state_path() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cwd")?;
    workspace_state_path_from(&cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_returns_default() {
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
        let s = AgentState {
            agent_asset_address: Some("ARsZN4kZUWNX8Ek5ZkVUpRitSFjym6p9jLvUxrg9kPWU".into()),
            ..AgentState::default()
        };
        save_at(&path, &s).unwrap();
        let back = load_at(&path).unwrap();
        assert_eq!(
            back.agent_asset_address.as_deref(),
            Some("ARsZN4kZUWNX8Ek5ZkVUpRitSFjym6p9jLvUxrg9kPWU")
        );
    }

    #[test]
    fn write_is_atomic_tempfile_cleanup() {
        let d = tempdir().unwrap();
        let path = d.path().join("agent-state.json");
        let s = AgentState {
            agent_asset_address: Some("X".into()),
            agent_token_mint: None,
        };
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

    #[test]
    fn workspace_state_path_from_finds_workspace_cargo_toml() {
        // Create fake workspace: <root>/Cargo.toml with [workspace] + nested dir.
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let nested = root.join("crates").join("some-crate").join("src");
        std::fs::create_dir_all(&nested).unwrap();
        // Add a non-workspace Cargo.toml at the crate level to prove we skip it.
        std::fs::write(
            root.join("crates").join("some-crate").join("Cargo.toml"),
            "[package]\nname = \"some-crate\"\n",
        )
        .unwrap();

        let got = workspace_state_path_from(&nested).unwrap();
        // Canonicalize both sides — tempdir on macOS lives under /var -> /private/var symlink.
        let expected = root.join("agent-state.json").canonicalize().ok();
        let got_canon = got.canonicalize().ok();
        if let (Some(e), Some(g)) = (expected, got_canon) {
            assert_eq!(g, e);
        } else {
            // File doesn't exist yet, so canonicalize may fail — compare parents instead.
            assert_eq!(got.file_name().unwrap(), "agent-state.json");
            assert_eq!(
                got.parent().unwrap().canonicalize().unwrap(),
                root.canonicalize().unwrap()
            );
        }
    }

    #[test]
    fn workspace_state_path_from_ignores_comment_false_positives() {
        let d = tempdir().unwrap();
        let fake_root = d.path();
        std::fs::write(
            fake_root.join("Cargo.toml"),
            "# this used to be a [workspace] member\n[package]\nname = \"foo\"\n",
        )
        .unwrap();
        let result = workspace_state_path_from(fake_root);
        // This should NOT resolve anchored at fake_root; it should either
        // walk further up (if there's a real workspace ancestor) or error.
        // Assert that if it returns Ok, the parent is NOT fake_root.
        if let Ok(p) = result {
            assert_ne!(p.parent().unwrap(), fake_root);
        }
    }

    #[test]
    fn workspace_state_path_from_errors_when_no_workspace_found() {
        let d = tempdir().unwrap();
        // No Cargo.toml anywhere on the walk — but /tmp ancestors might have one
        // on some hosts. Guard by checking walk-up manually: if any ancestor has
        // a workspace Cargo.toml we skip the negative assertion.
        let mut walker = d.path().to_path_buf();
        let mut has_workspace_ancestor = false;
        loop {
            let c = walker.join("Cargo.toml");
            if c.exists()
                && std::fs::read_to_string(&c)
                    .unwrap_or_default()
                    .contains("[workspace]")
            {
                has_workspace_ancestor = true;
                break;
            }
            if !walker.pop() {
                break;
            }
        }
        if has_workspace_ancestor {
            // Environment-dependent; skip rather than produce a flaky failure.
            return;
        }
        let err = workspace_state_path_from(d.path()).unwrap_err();
        assert!(
            err.to_string().contains("workspace root not found"),
            "unexpected error: {err}"
        );
    }
}
