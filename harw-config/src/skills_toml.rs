//! `[skills]` — triggered skill injection into the per-round model context
//! (PL-95 S3, PL-97 B1).
//!
//! # Responsibility
//! Declarative switches and byte caps only. The injector itself lives in
//! `harw-core` (`skill_injection`), the deterministic selection core in
//! `harw-catalog` (`skill_select`). A `config.toml` without `[skills]` keeps
//! the defaults: injection on for bundled and trusted-layer skills, off for
//! imported skills, 4 KiB per round, 16 KiB per session.
//!
//! ```toml
//! [skills]
//! triggered_injection = true   # deliver matching skill sections per round
//! inject_imported = false      # also deliver imported skills (as Evidence)
//! per_turn_bytes = 4096        # cap per round; the effective value is ~3 % of
//!                              # the model window, at least 1 KiB, at most this
//! per_session_bytes = 16384    # cap until the next compaction; at most four
//!                              # effective per-round budgets
//! ```
//!
//! # Scope
//! Every field is `Profile` scope with `ProfileReplaces`: the home layer and
//! the active profile set it, an untrusted project layer never does (see
//! `crate::merge`, `merge_skills`). Skills never grant rights; the switches
//! only decide which text reaches the model.

use crate::serde_defaults::default_true;
use serde::{Deserialize, Serialize};

/// Default of [`SkillsSection::per_turn_bytes`] (same value as
/// `harw_catalog::DEFAULT_PER_TURN_BYTES`).
pub const DEFAULT_SKILLS_PER_TURN_BYTES: usize = 4096;
/// Default of [`SkillsSection::per_session_bytes`] (same value as
/// `harw_catalog::DEFAULT_PER_SESSION_BYTES`).
pub const DEFAULT_SKILLS_PER_SESSION_BYTES: usize = 16384;
/// Allowed range of [`SkillsSection::per_turn_bytes`] (inclusive).
pub const SKILLS_PER_TURN_BYTES_RANGE: (usize, usize) = (256, 65_536);
/// Allowed range of [`SkillsSection::per_session_bytes`] (inclusive).
pub const SKILLS_PER_SESSION_BYTES_RANGE: (usize, usize) = (256, 65_536);

/// `[skills]` — see the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    /// Deliver triggered skill sections into the per-round context.
    /// `false` switches the whole L1 stage off (no fragment, no event); the
    /// L0 catalog lines and `skills.search`/`skills.load` stay available.
    #[serde(default = "default_true")]
    pub triggered_injection: bool,
    /// Also deliver skills marked as imported. They are framed with trust
    /// class `Evidence` (data block), never `Instruction`. Default `false`.
    #[serde(default)]
    pub inject_imported: bool,
    /// Upper bound of injected bytes per model round. The effective cap is
    /// ~3 % of the active model window (`harw_context::sources::
    /// skills_triggered`), at least 1 KiB, at most this value.
    #[serde(
        default = "default_per_turn_bytes",
        deserialize_with = "deserialize_per_turn_bytes"
    )]
    pub per_turn_bytes: usize,
    /// Upper bound of injected bytes per session until the next compaction.
    /// The effective cap is at most four effective per-round budgets, so a
    /// small model window never carries a large session cap.
    #[serde(
        default = "default_per_session_bytes",
        deserialize_with = "deserialize_per_session_bytes"
    )]
    pub per_session_bytes: usize,
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            triggered_injection: default_true(),
            inject_imported: false,
            per_turn_bytes: DEFAULT_SKILLS_PER_TURN_BYTES,
            per_session_bytes: DEFAULT_SKILLS_PER_SESSION_BYTES,
        }
    }
}

fn default_per_turn_bytes() -> usize {
    DEFAULT_SKILLS_PER_TURN_BYTES
}

fn default_per_session_bytes() -> usize {
    DEFAULT_SKILLS_PER_SESSION_BYTES
}

// Rejects a value outside the allowed range while parsing: every layer file is
// read with `toml::from_str`, so the error surfaces as `ConfigError::TomlParse`
// (same pattern as `[jobs] max_running`).
fn deserialize_in_range<'de, D>(
    deserializer: D,
    field: &str,
    (min, max): (usize, usize),
) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = usize::deserialize(deserializer)?;
    if value < min || value > max {
        return Err(serde::de::Error::custom(format!(
            "[skills] {field} must be between {min} and {max}, got {value}"
        )));
    }
    Ok(value)
}

fn deserialize_per_turn_bytes<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_in_range(deserializer, "per_turn_bytes", SKILLS_PER_TURN_BYTES_RANGE)
}

fn deserialize_per_session_bytes<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_in_range(
        deserializer,
        "per_session_bytes",
        SKILLS_PER_SESSION_BYTES_RANGE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn defaults_round_trip_through_toml() -> TestResult {
        let defaults = SkillsSection::default();
        assert!(defaults.triggered_injection);
        assert!(!defaults.inject_imported);
        assert_eq!(defaults.per_turn_bytes, 4096);
        assert_eq!(defaults.per_session_bytes, 16384);
        let encoded = toml::to_string(&defaults).map_err(ctx("serialize"))?;
        let decoded: SkillsSection = toml::from_str(&encoded).map_err(ctx("deserialize"))?;
        assert_eq!(decoded, defaults);
        let empty: SkillsSection = toml::from_str("").map_err(ctx("empty"))?;
        assert_eq!(empty, defaults);
        Ok(())
    }

    #[test]
    fn range_bounds_are_accepted() -> TestResult {
        let section: SkillsSection =
            toml::from_str("per_turn_bytes = 256\nper_session_bytes = 65536")
                .map_err(ctx("parse"))?;
        assert_eq!(section.per_turn_bytes, SKILLS_PER_TURN_BYTES_RANGE.0);
        assert_eq!(section.per_session_bytes, SKILLS_PER_SESSION_BYTES_RANGE.1);
        Ok(())
    }

    #[test]
    fn explicit_values_are_kept() -> TestResult {
        let section: SkillsSection = toml::from_str(
            "triggered_injection = false\ninject_imported = true\nper_turn_bytes = 2048\nper_session_bytes = 8192",
        )
        .map_err(ctx("parse"))?;
        assert!(!section.triggered_injection);
        assert!(section.inject_imported);
        assert_eq!(section.per_turn_bytes, 2048);
        assert_eq!(section.per_session_bytes, 8192);
        Ok(())
    }

    #[test]
    fn out_of_range_caps_and_unknown_keys_are_rejected() -> TestResult {
        for source in [
            "per_turn_bytes = 0",
            "per_turn_bytes = 100000",
            "per_session_bytes = 10",
            "per_session_bytes = 65537",
            "per_session_bytes = 2000000",
            "per_turn = 4096",
        ] {
            let Err(error) = toml::from_str::<SkillsSection>(source) else {
                return Err(TestError::Unexpected(format!("{source} must be rejected")));
            };
            let text = error.to_string();
            assert!(
                text.contains("[skills]") || text.contains("unknown field"),
                "{source}: {text}"
            );
        }
        Ok(())
    }
}
