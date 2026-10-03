//! Ownership labels set at create time.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Label key: the runner that owns the instance.
pub const LABEL_OWNER: &str = "harw.owner";
/// Label key: the job.
pub const LABEL_WORK_ID: &str = "harw.work_id";
/// Label key: the attempt.
pub const LABEL_ATTEMPT: &str = "harw.attempt";
/// Label key: the lease epoch the attempt was started under.
pub const LABEL_EPOCH: &str = "harw.lease_epoch";
/// Label key: the tenant.
pub const LABEL_TENANT: &str = "harw.tenant";
/// Label key: the sandbox/run profile.
pub const LABEL_PROFILE: &str = "harw.profile";

/// Longest accepted label value in bytes.
const MAX_VALUE: usize = 128;

/// Why labels were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelError {
    /// A value is empty, too long or has characters outside `[A-Za-z0-9._:-]`.
    InvalidValue(&'static str),
    /// A required key is missing.
    Missing(&'static str),
    /// A numeric label is not a number.
    NotANumber(&'static str),
}

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(key) => write!(f, "label `{key}` has an invalid value"),
            Self::Missing(key) => write!(f, "label `{key}` is missing"),
            Self::NotANumber(key) => write!(f, "label `{key}` is not a number"),
        }
    }
}

impl std::error::Error for LabelError {}

fn valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_VALUE
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Who an instance belongs to; compared on recovery and by garbage
/// collection. Never holds secrets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerLabels {
    /// The runner id.
    pub owner: String,
    /// The job id.
    pub work_id: String,
    /// The attempt number.
    pub attempt: u64,
    /// The lease epoch of the claim.
    pub lease_epoch: u64,
    /// The tenant.
    pub tenant: String,
    /// The run/sandbox profile.
    pub profile: String,
}

impl OwnerLabels {
    /// Builds validated labels.
    ///
    /// # Errors
    /// [`LabelError::InvalidValue`] for a text part that is not label-safe.
    pub fn new(
        owner: &str,
        work_id: &str,
        attempt: u64,
        lease_epoch: u64,
        tenant: &str,
        profile: &str,
    ) -> Result<Self, LabelError> {
        for (key, value) in [
            (LABEL_OWNER, owner),
            (LABEL_WORK_ID, work_id),
            (LABEL_TENANT, tenant),
            (LABEL_PROFILE, profile),
        ] {
            if !valid(value) {
                return Err(LabelError::InvalidValue(key));
            }
        }
        Ok(Self {
            owner: owner.to_owned(),
            work_id: work_id.to_owned(),
            attempt,
            lease_epoch,
            tenant: tenant.to_owned(),
            profile: profile.to_owned(),
        })
    }

    /// The engine label map.
    #[must_use]
    pub fn to_map(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (LABEL_OWNER.to_owned(), self.owner.clone()),
            (LABEL_WORK_ID.to_owned(), self.work_id.clone()),
            (LABEL_ATTEMPT.to_owned(), self.attempt.to_string()),
            (LABEL_EPOCH.to_owned(), self.lease_epoch.to_string()),
            (LABEL_TENANT.to_owned(), self.tenant.clone()),
            (LABEL_PROFILE.to_owned(), self.profile.clone()),
        ])
    }

    /// Reads labels back from an engine's label map (extra keys are ignored).
    ///
    /// # Errors
    /// A missing key, a bad value or a non-numeric number.
    pub fn from_map(labels: &BTreeMap<String, String>) -> Result<Self, LabelError> {
        let text = |key: &'static str| {
            labels
                .get(key)
                .map(String::as_str)
                .ok_or(LabelError::Missing(key))
        };
        let number = |key: &'static str| {
            text(key)?
                .parse::<u64>()
                .map_err(|_| LabelError::NotANumber(key))
        };
        Self::new(
            text(LABEL_OWNER)?,
            text(LABEL_WORK_ID)?,
            number(LABEL_ATTEMPT)?,
            number(LABEL_EPOCH)?,
            text(LABEL_TENANT)?,
            text(LABEL_PROFILE)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> OwnerLabels {
        OwnerLabels::new("runner-1", "work-9", 2, 7, "tenant-a", "hermetic")
            .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn labels_round_trip_through_the_engine_map() {
        let map = labels().to_map();
        assert_eq!(map.get(LABEL_OWNER).map(String::as_str), Some("runner-1"));
        assert_eq!(OwnerLabels::from_map(&map), Ok(labels()));
    }

    #[test]
    fn extra_keys_are_ignored_but_missing_or_bad_ones_are_not() {
        let mut map = labels().to_map();
        map.insert("com.example/x".to_owned(), "y".to_owned());
        assert_eq!(OwnerLabels::from_map(&map), Ok(labels()));
        map.remove(LABEL_TENANT);
        assert_eq!(
            OwnerLabels::from_map(&map),
            Err(LabelError::Missing(LABEL_TENANT))
        );
        let mut bad = labels().to_map();
        bad.insert(LABEL_EPOCH.to_owned(), "seven".to_owned());
        assert_eq!(
            OwnerLabels::from_map(&bad),
            Err(LabelError::NotANumber(LABEL_EPOCH))
        );
    }

    #[test]
    fn values_that_could_carry_secrets_or_injection_are_refused() {
        for bad in ["", "has space", "a=b", "line\nbreak", &"x".repeat(129)] {
            assert!(
                OwnerLabels::new(bad, "w", 1, 1, "t", "p").is_err(),
                "{bad:?}"
            );
        }
    }
}
