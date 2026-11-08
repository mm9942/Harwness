//! Ziel-Identität einer Warden-Aktion: [`CgroupKernelId`], [`CgroupTarget`]
//! (PL-100 WP 3.1, Bedrohung B21).
//!
//! # Zweck
//! Der Kernel vergibt cgroup-IDs neu, sobald eine cgroup verschwindet. Eine
//! Autorisierung, die nur einen Pfad oder nur eine Kernel-ID nennt, könnte
//! deshalb eine andere cgroup treffen als die, die der Operator freigegeben
//! hat. Proof v2 bindet daher das Tripel aus Kernel-ID, relativem Pfad und
//! `first_seen` (wann der Sensor diese cgroup zuerst beobachtet hat) in den MAC
//! ([`crate::signed::SignedAuthorization`]). Der Warden beobachtet das Ziel
//! unmittelbar vor dem Eingriff erneut und vergleicht über
//! [`crate::signed::VerifiedAuthorization::confirm_target`].
//!
//! # Wichtige Typen
//! - [`CgroupKernelId`]: Kernel-ID einer cgroup, nie `0` (die Wurzel-cgroup
//!   hat bereits ID `1`); `0` wird beim Konstruieren und beim Dekodieren
//!   abgelehnt.
//! - [`CgroupTarget`]: das gebundene Tripel.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! [`ProofError::InvalidKernelId`] für eine Kernel-ID `0`.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
//! use harw_types::CgroupId;
//!
//! let Ok(path) = CgroupId::try_from_str("harw.slice/job-1") else { return };
//! let Ok(kernel_id) = CgroupKernelId::new(4242) else { return };
//! let target = CgroupTarget::new(path, kernel_id, jiff::Timestamp::UNIX_EPOCH);
//! assert_eq!(target.kernel_id.get(), 4242);
//! assert!(CgroupKernelId::new(0).is_err());
//! ```

use std::fmt;
use std::num::NonZeroU64;

use harw_types::CgroupId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::canonical::{put_field, put_nested, put_timestamp, put_u64};
use crate::error::ProofError;

/// Kernel-ID einer cgroup (Inode-Nummer des cgroup-Verzeichnisses in cgroup v2).
///
/// # Description
/// Nie `0`. Auf der Leitung eine JSON-Zahl; eine `0` scheitert schon beim
/// Dekodieren.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::CgroupKernelId;
///
/// assert_eq!(CgroupKernelId::new(7).map(CgroupKernelId::get).ok(), Some(7));
/// assert!(serde_json::from_str::<CgroupKernelId>("0").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct CgroupKernelId(NonZeroU64);

impl CgroupKernelId {
    /// Validates and wraps a raw kernel cgroup id.
    ///
    /// # Arguments
    /// - `raw` (`u64`): die Kernel-ID.
    ///
    /// # Errors
    /// - [`ProofError::InvalidKernelId`]: `raw == 0`.
    pub fn new(raw: u64) -> Result<Self, ProofError> {
        NonZeroU64::new(raw)
            .map(Self)
            .ok_or(ProofError::InvalidKernelId)
    }

    /// Returns the raw kernel id (never `0`).
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for CgroupKernelId {
    type Error = ProofError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CgroupKernelId> for u64 {
    fn from(value: CgroupKernelId) -> Self {
        value.get()
    }
}

impl fmt::Display for CgroupKernelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Das in Proof v2 gebundene Ziel: relativer cgroup-Pfad, Kernel-ID und
/// erste Beobachtung.
///
/// # Description
/// Gleichheit heißt: alle drei Teile stimmen überein. Eine wiederverwendete
/// Kernel-ID unter demselben Pfad unterscheidet sich im `first_seen`, ein
/// umbenannter Pfad im `path`.
///
/// # Wire-Format
/// JSON-Objekt, `deny_unknown_fields`; `first_seen` RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupTarget {
    /// Relativer cgroup-Pfad unterhalb der cgroup-v2-Wurzel (z. B. `harw.slice/job-1`).
    pub path: CgroupId,
    /// Kernel-ID der cgroup.
    pub kernel_id: CgroupKernelId,
    /// Zeitpunkt, zu dem der Sensor diese cgroup zuerst beobachtet hat.
    pub first_seen: Timestamp,
}

impl CgroupTarget {
    /// Builds a target identity from its three parts.
    #[must_use]
    pub fn new(path: CgroupId, kernel_id: CgroupKernelId, first_seen: Timestamp) -> Self {
        Self {
            path,
            kernel_id,
            first_seen,
        }
    }

    // Kanonische Form als ein zusammengesetztes Feld: Pfad, Kernel-ID, first_seen.
    pub(crate) fn put_canonical(&self, buf: &mut Vec<u8>) {
        put_nested(buf, |inner| {
            put_field(inner, self.path.as_str().as_bytes());
            put_u64(inner, self.kernel_id.get());
            put_timestamp(inner, self.first_seen);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{CgroupKernelId, CgroupTarget};
    use crate::error::ProofError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::CgroupId;
    use jiff::Timestamp;

    fn target(path: &str, kid: u64, first_seen: i64) -> TestResult<CgroupTarget> {
        Ok(CgroupTarget::new(
            CgroupId::try_from_str(path).map_err(ctx("cgroup id"))?,
            CgroupKernelId::new(kid).map_err(ctx("kernel id"))?,
            Timestamp::from_second(first_seen).map_err(ctx("timestamp"))?,
        ))
    }

    fn canonical(target: &CgroupTarget) -> Vec<u8> {
        let mut buf = Vec::new();
        target.put_canonical(&mut buf);
        buf
    }

    #[test]
    fn test_kernel_id_rejects_zero() {
        assert!(matches!(
            CgroupKernelId::new(0),
            Err(ProofError::InvalidKernelId)
        ));
        assert!(matches!(
            CgroupKernelId::try_from(0u64),
            Err(ProofError::InvalidKernelId)
        ));
    }

    #[test]
    fn test_kernel_id_serde_is_a_plain_number_and_rejects_zero() -> TestResult {
        let kid = CgroupKernelId::new(4242).map_err(ctx("kernel id"))?;
        assert_eq!(
            serde_json::to_string(&kid).map_err(ctx("serializes"))?,
            "4242"
        );
        let back: CgroupKernelId = serde_json::from_str("4242").map_err(ctx("deserializes"))?;
        assert_eq!(back, kid);
        assert!(serde_json::from_str::<CgroupKernelId>("0").is_err());
        assert!(serde_json::from_str::<CgroupKernelId>("-1").is_err());
        assert_eq!(kid.to_string(), "4242");
        Ok(())
    }

    #[test]
    fn test_target_serde_roundtrip_and_unknown_field_rejected() -> TestResult {
        let original = target("harw.slice/job-1", 4242, 1_800_000_000)?;
        let mut value = serde_json::to_value(&original).map_err(ctx("to_value"))?;
        let back: CgroupTarget =
            serde_json::from_value(value.clone()).map_err(ctx("deserializes"))?;
        assert_eq!(back, original);
        value
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<CgroupTarget>(value).is_err());
        Ok(())
    }

    #[test]
    fn test_target_equality_requires_all_three_parts() -> TestResult {
        let base = target("harw.slice/job-1", 4242, 1_800_000_000)?;
        assert_eq!(base, target("harw.slice/job-1", 4242, 1_800_000_000)?);
        assert_ne!(base, target("harw.slice/job-2", 4242, 1_800_000_000)?);
        assert_ne!(base, target("harw.slice/job-1", 4243, 1_800_000_000)?);
        assert_ne!(base, target("harw.slice/job-1", 4242, 1_800_000_001)?);
        Ok(())
    }

    #[test]
    fn test_canonical_form_changes_with_every_part() -> TestResult {
        let base = canonical(&target("harw.slice/job-1", 4242, 1_800_000_000)?);
        assert_eq!(
            base,
            canonical(&target("harw.slice/job-1", 4242, 1_800_000_000)?)
        );
        assert_ne!(
            base,
            canonical(&target("harw.slice/job-2", 4242, 1_800_000_000)?)
        );
        assert_ne!(
            base,
            canonical(&target("harw.slice/job-1", 4243, 1_800_000_000)?)
        );
        assert_ne!(
            base,
            canonical(&target("harw.slice/job-1", 4242, 1_800_000_001)?)
        );
        Ok(())
    }
}
