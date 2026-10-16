//! A warm pool of verified image slots, partitioned by tenant.
//!
//! Containers cannot be pre-created and handed over later: engines fix
//! labels, mounts and resource limits at create time, so a warm container
//! would carry another attempt's identity. The pool therefore holds
//! *verified image slots* (the image was checked present at its pinned
//! digest) and accounts for them; every attempt still creates its own
//! container. Slots are single-use and keyed by tenant, digest and profile;
//! there is no API that looks across keys.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use harw_container_model::ImageDigest;
use harw_job_core::SandboxProfileName;

/// The only way to address a slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    tenant: String,
    digest: ImageDigest,
    profile: SandboxProfileName,
}

impl PoolKey {
    /// Builds a key.
    #[must_use]
    pub fn new(tenant: &str, digest: ImageDigest, profile: SandboxProfileName) -> Self {
        Self {
            tenant: tenant.to_owned(),
            digest,
            profile,
        }
    }
}

/// A verified, single-use slot.
#[derive(Debug, PartialEq, Eq)]
pub struct Slot {
    key: PoolKey,
    verified_unix: i64,
}

impl Slot {
    /// When the image was verified.
    #[must_use]
    pub fn verified_unix(&self) -> i64 {
        self.verified_unix
    }

    /// The key it belongs to.
    #[must_use]
    pub fn key(&self) -> &PoolKey {
        &self.key
    }
}

/// The pool.
#[derive(Debug)]
pub struct WarmPool {
    capacity: usize,
    idle_ttl: i64,
    slots: HashMap<PoolKey, VecDeque<i64>>,
}

impl WarmPool {
    /// A pool holding at most `capacity` slots in total; a slot idle longer
    /// than `idle_ttl` is dropped.
    #[must_use]
    pub fn new(capacity: usize, idle_ttl: Duration) -> Self {
        Self {
            capacity,
            idle_ttl: i64::try_from(idle_ttl.as_secs()).unwrap_or(i64::MAX),
            slots: HashMap::new(),
        }
    }

    /// Total slots currently held (including ones not yet expired).
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.values().map(VecDeque::len).sum()
    }

    /// Whether the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn expire(&mut self, now_unix: i64) {
        let ttl = self.idle_ttl;
        for queue in self.slots.values_mut() {
            queue.retain(|verified| now_unix.saturating_sub(*verified) <= ttl);
        }
        self.slots.retain(|_, queue| !queue.is_empty());
    }

    /// Registers a freshly verified image. `false` when the pool is full.
    pub fn offer(&mut self, key: PoolKey, now_unix: i64) -> bool {
        self.expire(now_unix);
        if self.len() >= self.capacity {
            return false;
        }
        self.slots.entry(key).or_default().push_back(now_unix);
        true
    }

    /// Takes one slot for exactly this key; it is gone afterwards.
    pub fn take(&mut self, key: &PoolKey, now_unix: i64) -> Option<Slot> {
        self.expire(now_unix);
        let queue = self.slots.get_mut(key)?;
        let verified_unix = queue.pop_front()?;
        if queue.is_empty() {
            self.slots.remove(key);
        }
        Some(Slot {
            key: key.clone(),
            verified_unix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{IMAGE_HEX, TestResult, ctx};

    fn digest(hex: &str) -> TestResult<ImageDigest> {
        ImageDigest::parse(&format!("rust@sha256:{hex}")).map_err(ctx("digest"))
    }

    fn key(tenant: &str, hex: &str, profile: SandboxProfileName) -> TestResult<PoolKey> {
        Ok(PoolKey::new(tenant, digest(hex)?, profile))
    }

    #[test]
    fn slots_never_cross_tenant_digest_or_profile() -> TestResult {
        let mut pool = WarmPool::new(8, Duration::from_secs(60));
        let a = key("a", IMAGE_HEX, SandboxProfileName::NoNetwork)?;
        assert!(pool.offer(a.clone(), 100));
        let other_tenant = key("b", IMAGE_HEX, SandboxProfileName::NoNetwork)?;
        let other_digest = key("a", &"f".repeat(64), SandboxProfileName::NoNetwork)?;
        let other_profile = key("a", IMAGE_HEX, SandboxProfileName::WorkspaceBuild)?;
        for k in [&other_tenant, &other_digest, &other_profile] {
            assert!(pool.take(k, 101).is_none());
        }
        assert!(pool.take(&a, 101).is_some());
        Ok(())
    }

    #[test]
    fn a_slot_is_single_use() -> TestResult {
        let mut pool = WarmPool::new(8, Duration::from_secs(60));
        let a = key("a", IMAGE_HEX, SandboxProfileName::NoNetwork)?;
        assert!(pool.offer(a.clone(), 100));
        assert!(pool.take(&a, 101).is_some());
        assert!(pool.take(&a, 102).is_none());
        assert!(pool.is_empty());
        Ok(())
    }

    #[test]
    fn idle_slots_expire_and_capacity_holds() -> TestResult {
        let mut pool = WarmPool::new(2, Duration::from_secs(60));
        let a = key("a", IMAGE_HEX, SandboxProfileName::NoNetwork)?;
        assert!(pool.offer(a.clone(), 100));
        assert!(pool.offer(a.clone(), 100));
        assert!(!pool.offer(a.clone(), 100), "full");
        assert!(pool.take(&a, 161).is_none(), "expired");
        assert!(pool.offer(a, 161), "room again after expiry");
        Ok(())
    }
}
