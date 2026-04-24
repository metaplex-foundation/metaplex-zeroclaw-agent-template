//! On-chain owner resolution + process-global TTL cache (SPEC §3.4).
//!
//! The `AssetOwnerFetcher` trait abstracts the MPL Core asset fetch so unit
//! tests can mock it without network. The real fetcher (`MplCoreFetcher`) is
//! wired in PR 3 alongside the first consumer tool.

use async_trait::async_trait;
use parking_lot::Mutex;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Trait abstracting the MPL Core asset fetch so unit tests can mock it.
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
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    fn get(&self, asset: &Pubkey) -> Option<Option<Pubkey>> {
        let g = self.inner.lock();
        g.get(asset).and_then(|c| {
            if c.expires_at > Instant::now() {
                Some(c.owner)
            } else {
                None
            }
        })
    }

    fn put(&self, asset: Pubkey, owner: Option<Pubkey>) {
        let mut g = self.inner.lock();
        g.insert(
            asset,
            CachedOwner {
                owner,
                expires_at: Instant::now() + self.ttl,
            },
        );
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
        if let Some(cached) = cache.get(&asset) {
            return Ok(cached);
        }
        let owner = fetcher.fetch_owner(&asset).await?;
        cache.put(asset, owner);
        return Ok(owner);
    }
    Ok(bootstrap_wallet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use solana_sdk::pubkey::Pubkey;
    use std::sync::atomic::{AtomicU32, Ordering};
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

    fn pk(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    #[tokio::test]
    async fn returns_bootstrap_when_asset_none() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher {
            calls: AtomicU32::new(0),
            owner: pk(1),
        };
        let got = resolve_owner(&f, &cache, None, Some(pk(9))).await.unwrap();
        assert_eq!(got, Some(pk(9)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fetches_and_caches_on_chain_owner() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher {
            calls: AtomicU32::new(0),
            owner: pk(3),
        };
        let asset = pk(2);
        let got1 = resolve_owner(&f, &cache, Some(asset), Some(pk(9)))
            .await
            .unwrap();
        let got2 = resolve_owner(&f, &cache, Some(asset), Some(pk(9)))
            .await
            .unwrap();
        assert_eq!(got1, Some(pk(3)));
        assert_eq!(got2, Some(pk(3)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "cached on second call");
    }

    #[tokio::test]
    async fn refetches_after_ttl_elapses() {
        let cache = OwnerCache::new(Duration::from_millis(10));
        let f = MockFetcher {
            calls: AtomicU32::new(0),
            owner: pk(5),
        };
        let asset = pk(4);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalidate_forces_refetch() {
        let cache = OwnerCache::new(Duration::from_secs(60));
        let f = MockFetcher {
            calls: AtomicU32::new(0),
            owner: pk(7),
        };
        let asset = pk(6);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        cache.invalidate(&asset);
        let _ = resolve_owner(&f, &cache, Some(asset), None).await.unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }
}
