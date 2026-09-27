//! Sandbox enforcement reporting — the "no `bool sandboxed`" rule.
//!
//! A sandbox is never reported as a single yes/no. Each dimension carries its
//! own [`EnforcementState`], filled in by the platform backend (Linux,
//! Darwin) after it applied the policy. Consumers decide against a
//! [`SandboxRequirement`] whether the achieved enforcement suffices.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::SandboxRequirement;

/// How far one sandbox dimension was actually enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementState {
    /// Fully enforced as requested.
    Enforced,
    /// Enforced, but weaker than requested (e.g. an older kernel ABI).
    Partial,
    /// Supported by the platform, but not applied.
    NotEnforced,
    /// The platform cannot enforce this dimension at all.
    Unsupported,
}

impl EnforcementState {
    /// Whether the dimension is fully enforced.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        matches!(self, Self::Enforced)
    }
}

impl fmt::Display for EnforcementState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enforced => "enforced",
            Self::Partial => "partial",
            Self::NotEnforced => "not_enforced",
            Self::Unsupported => "unsupported",
        })
    }
}

/// Per-dimension enforcement report of one attempt's sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxReport {
    /// Filesystem access restriction.
    pub filesystem: EnforcementState,
    /// Network access restriction.
    pub network: EnforcementState,
    /// `no_new_privs` (or the platform equivalent).
    pub no_new_privs: EnforcementState,
    /// Capability / privilege dropping.
    pub capabilities: EnforcementState,
    /// Resource limits from the `ResourceRequest`.
    pub resource_limits: EnforcementState,
}

impl SandboxReport {
    /// A report with every dimension in `state`.
    #[must_use]
    pub const fn uniform(state: EnforcementState) -> Self {
        Self {
            filesystem: state,
            network: state,
            no_new_privs: state,
            capabilities: state,
            resource_limits: state,
        }
    }

    /// The dimensions in a fixed order.
    #[must_use]
    pub const fn dimensions(&self) -> [(&'static str, EnforcementState); 5] {
        [
            ("filesystem", self.filesystem),
            ("network", self.network),
            ("no_new_privs", self.no_new_privs),
            ("capabilities", self.capabilities),
            ("resource_limits", self.resource_limits),
        ]
    }

    /// Summary across all dimensions:
    /// - `Enforced` if every dimension is enforced,
    /// - `Unsupported` if every dimension is unsupported,
    /// - `NotEnforced` if no dimension is enforced or partial,
    /// - `Partial` otherwise.
    #[must_use]
    pub fn overall(&self) -> EnforcementState {
        let states = self.dimensions().map(|(_, state)| state);
        if states
            .iter()
            .all(|state| *state == EnforcementState::Enforced)
        {
            EnforcementState::Enforced
        } else if states
            .iter()
            .all(|state| *state == EnforcementState::Unsupported)
        {
            EnforcementState::Unsupported
        } else if states.iter().all(|state| {
            matches!(
                state,
                EnforcementState::NotEnforced | EnforcementState::Unsupported
            )
        }) {
            EnforcementState::NotEnforced
        } else {
            EnforcementState::Partial
        }
    }

    /// Names of the dimensions that are not fully enforced.
    #[must_use]
    pub fn shortfalls(&self) -> Vec<&'static str> {
        self.dimensions()
            .into_iter()
            .filter(|(_, state)| !state.is_enforced())
            .map(|(name, _)| name)
            .collect()
    }

    /// Whether this report satisfies `requirement`: [`SandboxRequirement::Required`]
    /// demands every dimension enforced; the other requirements accept any
    /// report (the report still records what was achieved).
    #[must_use]
    pub fn satisfies(&self, requirement: SandboxRequirement) -> bool {
        match requirement {
            SandboxRequirement::Required => self.overall() == EnforcementState::Enforced,
            SandboxRequirement::BestEffort | SandboxRequirement::None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EnforcementState as S, SandboxReport};
    use crate::spec::SandboxRequirement;
    use crate::test_support::{TestResult, ctx};

    const ALL: [S; 4] = [S::Enforced, S::Partial, S::NotEnforced, S::Unsupported];

    #[test]
    fn uniform_reports_summarize_to_their_state() {
        for state in ALL {
            assert_eq!(SandboxReport::uniform(state).overall(), state, "{state}");
        }
    }

    #[test]
    fn mixed_reports_summarize_to_the_weakest_meaningful_state() {
        let mut report = SandboxReport::uniform(S::Enforced);
        report.network = S::Unsupported;
        assert_eq!(report.overall(), S::Partial);
        assert_eq!(report.shortfalls(), vec!["network"]);

        let mut report = SandboxReport::uniform(S::NotEnforced);
        report.capabilities = S::Unsupported;
        assert_eq!(report.overall(), S::NotEnforced);

        let mut report = SandboxReport::uniform(S::Unsupported);
        report.filesystem = S::Partial;
        assert_eq!(report.overall(), S::Partial);
    }

    #[test]
    fn required_needs_every_dimension_enforced() {
        assert!(SandboxReport::uniform(S::Enforced).satisfies(SandboxRequirement::Required));
        for weak in [S::Partial, S::NotEnforced, S::Unsupported] {
            for index in 0..5 {
                let mut report = SandboxReport::uniform(S::Enforced);
                match index {
                    0 => report.filesystem = weak,
                    1 => report.network = weak,
                    2 => report.no_new_privs = weak,
                    3 => report.capabilities = weak,
                    _ => report.resource_limits = weak,
                }
                assert!(
                    !report.satisfies(SandboxRequirement::Required),
                    "{weak} in dimension {index}"
                );
                assert!(report.satisfies(SandboxRequirement::BestEffort));
                assert!(report.satisfies(SandboxRequirement::None));
                assert_eq!(report.shortfalls().len(), 1);
            }
        }
    }

    #[test]
    fn report_round_trips_through_serde() -> TestResult {
        let mut report = SandboxReport::uniform(S::Enforced);
        report.network = S::NotEnforced;
        let json = serde_json::to_string(&report).map_err(ctx("serialize"))?;
        assert!(json.contains(r#""network":"not_enforced""#), "{json}");
        let back: SandboxReport = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, report);

        // A legacy-style boolean flag is not a valid report.
        assert!(serde_json::from_str::<SandboxReport>(r#"{"sandboxed":true}"#).is_err());
        Ok(())
    }
}
