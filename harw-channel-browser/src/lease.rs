use std::fmt;
use std::sync::Arc;

/// Immutable fencing capability for one configured browser profile.
#[derive(Debug, PartialEq, Eq)]
pub struct LeaseToken {
    profile_binding: Arc<str>,
    epoch: u64,
    nonce: Arc<str>,
}

impl LeaseToken {
    #[must_use]
    pub fn new(
        profile_binding: impl Into<Arc<str>>,
        epoch: u64,
        nonce: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            profile_binding: profile_binding.into(),
            epoch,
            nonce: nonce.into(),
        }
    }

    #[must_use]
    pub fn profile_binding(&self) -> &str {
        &self.profile_binding
    }

    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// A newer token for the same profile fences this token.
    #[must_use]
    pub fn fences(&self, stale: &Self) -> bool {
        self.profile_binding == stale.profile_binding && self.epoch > stale.epoch
    }
}

/// Current connector ownership for one configured browser identity.
#[derive(Debug, PartialEq, Eq)]
pub struct ConnectorLease {
    profile_binding: Arc<str>,
    current: Option<CurrentLease>,
}

#[derive(Debug, PartialEq, Eq)]
struct CurrentLease {
    epoch: u64,
    nonce: Arc<str>,
}

impl ConnectorLease {
    #[must_use]
    pub fn new(profile_binding: impl Into<Arc<str>>) -> Self {
        Self {
            profile_binding: profile_binding.into(),
            current: None,
        }
    }

    /// Install a new epoch/nonce holder and return its immutable capability.
    /// Any previously issued token becomes invalid immediately.
    #[must_use]
    pub fn issue(&mut self, epoch: u64, nonce: impl Into<Arc<str>>) -> LeaseToken {
        let nonce = nonce.into();
        self.current = Some(CurrentLease {
            epoch,
            nonce: Arc::clone(&nonce),
        });
        LeaseToken {
            profile_binding: Arc::clone(&self.profile_binding),
            epoch,
            nonce,
        }
    }

    pub fn validate(&self, token: &LeaseToken) -> Result<(), LeaseValidationError> {
        if token.profile_binding != self.profile_binding {
            return Err(LeaseValidationError::ProfileBindingMismatch);
        }

        let Some(current) = &self.current else {
            return Err(LeaseValidationError::NotIssued);
        };

        if token.epoch != current.epoch {
            return Err(LeaseValidationError::EpochMismatch {
                current: current.epoch,
                presented: token.epoch,
            });
        }
        if token.nonce != current.nonce {
            return Err(LeaseValidationError::NonceMismatch);
        }

        Ok(())
    }
}

/// Exact reason a token cannot exercise connector ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseValidationError {
    NotIssued,
    ProfileBindingMismatch,
    EpochMismatch { current: u64, presented: u64 },
    NonceMismatch,
}

impl fmt::Display for LeaseValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotIssued => formatter.write_str("connector lease has not been issued"),
            Self::ProfileBindingMismatch => {
                formatter.write_str("lease token belongs to a different browser profile")
            }
            Self::EpochMismatch { current, presented } => write!(
                formatter,
                "lease token epoch {presented} does not match current epoch {current}"
            ),
            Self::NonceMismatch => formatter.write_str("lease token nonce is not current"),
        }
    }
}

impl std::error::Error for LeaseValidationError {}
