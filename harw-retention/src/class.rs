//! Data-class metadata, per-class config overrides and policy resolution.
//!
//! A *class* is one kind of ephemeral data (for example "telemetry rotated
//! files"). Classes are declared once with
//! [`harw_macros::retention_classes!`](harw_macros::retention_classes) (see
//! `classes.rs`); the macro derives the config struct, the `CLASSES`
//! registry and `policy_for` from that single list, using the types here.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::policy::{NameMatch, Report, RetentionError, RetentionPolicy, SweepMode};
use crate::sweep::sweep;

/// How sensitive a class's data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// Logs, caches, spools of no evidentiary value: on by default with
    /// default limits.
    Ephemeral,
    /// Security evidence (findings, spools, freeze records, transcripts):
    /// opt-in. Default is to never delete; dry runs only report. Apply is
    /// refused without explicit opt-in.
    SecurityRelevant,
}

impl ClassKind {
    /// Whether a class of this kind is enabled when config says nothing.
    #[must_use]
    pub const fn default_enabled(self) -> bool {
        matches!(self, Self::Ephemeral)
    }

    /// Stable label for logs and doctor output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::SecurityRelevant => "security-relevant",
        }
    }
}

/// Default limits of a class. `None` = no such limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassDefaults {
    /// Default `max_age` in seconds.
    pub max_age_secs: Option<u64>,
    /// Default `max_bytes`.
    pub max_bytes: Option<u64>,
    /// Default `max_files`.
    pub max_files: Option<u64>,
    /// Default `keep_newest`.
    pub keep_newest: u64,
}

/// Where a class lives on disk. Resolvers are pure path computations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The harness home (`~/.harw` or an override).
    pub home: PathBuf,
    /// The project root, when there is one.
    pub project: Option<PathBuf>,
}

/// Resolver of the directories to sweep for one class.
pub type DirResolver = fn(&Roots) -> Vec<PathBuf>;

/// Declaration of one retention class (one row of the registry).
#[derive(Debug, Clone)]
pub struct Class {
    /// Stable snake_case id; also the `[retention.<id>]` config key.
    pub id: &'static str,
    /// Sensitivity.
    pub kind: ClassKind,
    /// Which files in the class directories belong to the class.
    pub name_match: NameMatch,
    /// Default limits.
    pub defaults: ClassDefaults,
    /// Directory resolver.
    pub resolve_dirs: DirResolver,
}

/// Names of the [`ClassConfig`] keys, in declaration order.
pub const CLASS_CONFIG_FIELDS: [&str; 5] = [
    "enabled",
    "max_age_secs",
    "max_bytes",
    "max_files",
    "keep_newest",
];

/// `[retention.<class>]` override table. Every key is optional; an unset
/// key means "use the class default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "crate::serde", deny_unknown_fields)]
pub struct ClassConfig {
    /// Switch the class on or off. Ephemeral classes default to on;
    /// security-relevant classes default to off and are enabled **only** by
    /// an explicit `true` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Override of the age limit (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_secs: Option<u64>,
    /// Override of the byte limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Override of the file-count limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u64>,
    /// Override of the always-kept newest files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_newest: Option<u64>,
}

impl ClassConfig {
    /// Whether no key is set (the table is omitted when serializing).
    #[must_use]
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    /// Checks invariants plain deserialization cannot express.
    ///
    /// # Errors
    /// A message naming the key when a limit is `0` (use a large value for
    /// "effectively unlimited"; `0` would delete everything).
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("max_age_secs", self.max_age_secs),
            ("max_bytes", self.max_bytes),
            ("max_files", self.max_files),
        ] {
            if value == Some(0) {
                return Err(format!("{name} must be greater than 0"));
            }
        }
        Ok(())
    }
}

/// A class with its config applied: everything a sweep job needs.
#[derive(Debug, Clone)]
pub struct ResolvedClass {
    /// The declaration.
    pub class: &'static Class,
    /// Effective on/off state.
    pub enabled: bool,
    /// The config explicitly said `enabled = true`.
    pub explicit_optin: bool,
    /// Effective age limit (seconds).
    pub max_age_secs: Option<u64>,
    /// Effective byte limit.
    pub max_bytes: Option<u64>,
    /// Effective file-count limit.
    pub max_files: Option<u64>,
    /// Effective always-kept newest files.
    pub keep_newest: u64,
}

impl ResolvedClass {
    /// Applies `cfg` on top of the class defaults and opt-in rules.
    #[must_use]
    pub fn resolve(class: &'static Class, cfg: &ClassConfig) -> Self {
        let explicit_optin = cfg.enabled == Some(true);
        let enabled = cfg.enabled.unwrap_or(class.kind.default_enabled());
        Self {
            class,
            enabled,
            explicit_optin,
            max_age_secs: cfg.max_age_secs.or(class.defaults.max_age_secs),
            max_bytes: cfg.max_bytes.or(class.defaults.max_bytes),
            max_files: cfg.max_files.or(class.defaults.max_files),
            keep_newest: cfg.keep_newest.unwrap_or(class.defaults.keep_newest),
        }
    }

    /// Builds the sweep policy for `mode`.
    ///
    /// # Errors
    /// For [`SweepMode::Apply`]: [`RetentionError::OptInRequired`] for a
    /// security-relevant class without explicit opt-in, and
    /// [`RetentionError::Disabled`] for a disabled ephemeral class. A dry
    /// run is always allowed (it only reports).
    pub fn policy(
        &self,
        mode: SweepMode,
        deadline: Option<Instant>,
    ) -> Result<RetentionPolicy, RetentionError> {
        if mode == SweepMode::Apply {
            match self.class.kind {
                ClassKind::SecurityRelevant if !(self.enabled && self.explicit_optin) => {
                    return Err(RetentionError::OptInRequired(self.class.id));
                }
                ClassKind::Ephemeral if !self.enabled => {
                    return Err(RetentionError::Disabled(self.class.id));
                }
                _ => {}
            }
        }
        Ok(RetentionPolicy {
            max_age: self.max_age_secs.map(std::time::Duration::from_secs),
            max_bytes: self.max_bytes,
            max_files: self
                .max_files
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
            keep_newest: usize::try_from(self.keep_newest).unwrap_or(usize::MAX),
            name_match: self.class.name_match.clone(),
            dry_run: mode == SweepMode::DryRun,
            deadline,
        })
    }

    /// A doctor line when this class leaves data growing unbounded, else
    /// `None`.
    #[must_use]
    pub fn doctor_warning(&self) -> Option<String> {
        let id = self.class.id;
        if !self.enabled {
            return Some(match self.class.kind {
                ClassKind::SecurityRelevant => format!(
                    "retention class `{id}` is security-relevant and not opted in: \
                     data is never deleted (dry runs only report); set \
                     `[retention.{id}] enabled = true` to bound it"
                ),
                ClassKind::Ephemeral => {
                    format!("retention class `{id}` is disabled: data grows unbounded")
                }
            });
        }
        if self.max_age_secs.is_none() && self.max_bytes.is_none() && self.max_files.is_none() {
            return Some(format!(
                "retention class `{id}` has no limits: data grows unbounded"
            ));
        }
        None
    }

    /// Sweeps every directory of the class under `roots`.
    ///
    /// # Errors
    /// The policy errors of [`ResolvedClass::policy`]; per-directory
    /// failures are in the returned outcomes.
    pub fn sweep(
        &self,
        roots: &Roots,
        mode: SweepMode,
        deadline: Option<Instant>,
        now: SystemTime,
    ) -> Result<Vec<DirOutcome>, RetentionError> {
        let policy = self.policy(mode, deadline)?;
        Ok((self.class.resolve_dirs)(roots)
            .into_iter()
            .map(|dir| {
                let result = sweep(&dir, &policy, now);
                DirOutcome { dir, result }
            })
            .collect())
    }
}

/// Result of sweeping one directory of a class.
#[derive(Debug)]
pub struct DirOutcome {
    /// The directory.
    pub dir: PathBuf,
    /// Its report or failure.
    pub result: Result<Report, RetentionError>,
}
