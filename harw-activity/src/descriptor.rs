//! Static activity metadata: [`ActivityDescriptor`] and [`Effect`].
//!
//! A descriptor says *what kind of thing* an observation is (`filesystem` /
//! `read` / argument `path` / [`Effect::Observe`]). It is structural meaning
//! only and never a permission or authority token (PL-71 I4).

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Domain used by job-runtime producers (`job.*` tools and job run events).
///
/// The reducer uses it for exactly one decision: an event keyed by a
/// provisional [`crate::ActivityKey::Call`] belongs to `job-lifecycle@1` only
/// when its descriptor domain is `job`.
pub const JOB_DOMAIN: &str = "job";

/// Domain used by agent-runtime producers (child agent run events).
pub const AGENT_DOMAIN: &str = "agent";

/// Coarse effect class of an activity.
///
/// The reducer groups only by effect and resource key, never by tool name, so
/// the effect decides which pattern an observation may join. [`Effect::Other`]
/// marks unknown semantics and always yields a standalone instance (PL-71 I5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads or inspects state without changing it (`fs.read`, `job.status`).
    Observe,
    /// Changes an existing resource (`fs.edit`, `fs.write` on an existing file).
    Mutate,
    /// Runs something whose side effects are not described by the key
    /// (`shell.exec`, `job.start`, a child agent run).
    Execute,
    /// Talks to another party (messages, network requests).
    Communicate,
    /// Creates a new resource.
    Create,
    /// Removes a resource.
    Delete,
    /// Steers another activity (stop, cancel, pause).
    Control,
    /// Unknown or unclassified semantics.
    Other,
}

impl Effect {
    /// Every effect, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Observe,
        Self::Mutate,
        Self::Execute,
        Self::Communicate,
        Self::Create,
        Self::Delete,
        Self::Control,
        Self::Other,
    ];

    /// Stable lower-case name (`"observe"`, `"mutate"`, ...), identical to the
    /// serde representation and to the `effect = "..."` spelling used by the
    /// `#[tool(activity(...))]` macro key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Mutate => "mutate",
            Self::Execute => "execute",
            Self::Communicate => "communicate",
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Control => "control",
            Self::Other => "other",
        }
    }

    /// Parses a stable name produced by [`Effect::as_str`].
    ///
    /// Returns `None` for anything else; callers must fail closed on `None`
    /// (unknown effect spelling), not fall back to a guess.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| effect.as_str() == name)
    }

    /// `true` for effects that may change state the event's key does not
    /// describe: [`Effect::Mutate`], [`Effect::Create`], [`Effect::Delete`]
    /// and [`Effect::Execute`].
    pub const fn may_mutate(self) -> bool {
        matches!(
            self,
            Self::Mutate | Self::Create | Self::Delete | Self::Execute
        )
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Static semantic metadata attached to an event producer (usually a tool).
///
/// Live producers build descriptors from `'static` strings (macro-generated
/// constants, hand-written tables) through the `const fn`
/// [`ActivityDescriptor::new`]; the [`Cow`] fields additionally let recorded
/// traces deserialize into owned strings so replay never needs a registry.
///
/// A descriptor is descriptive only: it carries no permission, grant or
/// approval meaning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityDescriptor {
    /// Subsystem vocabulary (`"filesystem"`, `"job"`, `"agent"`, ...).
    pub domain: Cow<'static, str>,
    /// Action inside the domain (`"read"`, `"edit"`, `"status"`, ...). Pattern
    /// projections tally constituents per verb.
    pub verb: Cow<'static, str>,
    /// Name of the argument field the producer reads the resource key from
    /// (`Some("path")`), if any. Informational for the reducer: the adapter
    /// resolves and canonicalizes the key and puts it into
    /// [`crate::ActivityEvent::resource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_field: Option<Cow<'static, str>>,
    /// Coarse effect class.
    pub effect: Effect,
}

impl ActivityDescriptor {
    /// Descriptor for producers without activity metadata. Events carrying it
    /// always become standalone instances (PL-71 I5).
    pub const UNKNOWN: Self = Self::new("unknown", "unknown", None, Effect::Other);

    /// Conventional descriptor for scope boundary events
    /// ([`crate::Lifecycle::Boundary`]); the reducer ignores the descriptor
    /// of boundary events.
    pub const SCOPE: Self = Self::new("scope", "boundary", None, Effect::Control);

    /// Conventional descriptor for a job run reported by the job runtime
    /// itself (resource `Job(W)`, no call id), as opposed to `job.*` tool calls.
    ///
    /// Only such a run event ends a `job-lifecycle@1` instance: its
    /// `Finished` makes the instance terminal, `job.*` tool calls never do
    /// (see the [adapter contract](crate#adapter-contract), rule 3).
    pub const JOB_RUN: Self = Self::new(JOB_DOMAIN, "run", None, Effect::Execute);

    /// Conventional descriptor for a child agent run reported by the agent
    /// runtime (resource `Agent(id)`, no call id).
    pub const AGENT_RUN: Self = Self::new(AGENT_DOMAIN, "run", None, Effect::Execute);

    /// Builds a descriptor from static strings; usable in `const` items.
    pub const fn new(
        domain: &'static str,
        verb: &'static str,
        resource_field: Option<&'static str>,
        effect: Effect,
    ) -> Self {
        let resource_field = match resource_field {
            Some(field) => Some(Cow::Borrowed(field)),
            None => None,
        };
        Self {
            domain: Cow::Borrowed(domain),
            verb: Cow::Borrowed(verb),
            resource_field,
            effect,
        }
    }

    /// `true` when the descriptor carries usable semantics: a non-blank
    /// domain and verb and an effect other than [`Effect::Other`].
    ///
    /// Unknown descriptors never join a grouping pattern.
    pub fn is_known(&self) -> bool {
        known_semantics(&self.domain, &self.verb, self.effect)
    }
}

/// The one rule behind [`ActivityDescriptor::is_known`], shared with
/// [`crate::Constituent::is_known`] so a constituent is judged by the
/// descriptor that introduced it.
pub(crate) fn known_semantics(domain: &str, verb: &str, effect: Effect) -> bool {
    effect != Effect::Other && !domain.trim().is_empty() && !verb.trim().is_empty()
}

impl fmt::Display for ActivityDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{} ({})", self.domain, self.verb, self.effect)?;
        if let Some(field) = &self.resource_field {
            write!(f, " resource={field}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const FS_READ: ActivityDescriptor =
        ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe);

    #[test]
    fn effect_names_round_trip() {
        for effect in Effect::ALL {
            assert_eq!(Effect::from_name(effect.as_str()), Some(effect));
        }
        assert_eq!(Effect::from_name("Observe"), None);
        assert_eq!(Effect::from_name("observe "), None);
        assert_eq!(Effect::from_name(""), None);
    }

    #[test]
    fn effect_serde_matches_as_str() -> TestResult {
        for effect in Effect::ALL {
            let json = serde_json::to_string(&effect)?;
            assert_eq!(json, format!("\"{}\"", effect.as_str()));
            let back: Effect = serde_json::from_str(&json)?;
            assert_eq!(back, effect);
        }
        assert!(serde_json::from_str::<Effect>("\"teleport\"").is_err());
        Ok(())
    }

    #[test]
    fn may_mutate_covers_exactly_the_state_changing_effects() {
        let mutating: Vec<Effect> = Effect::ALL
            .into_iter()
            .filter(|effect| effect.may_mutate())
            .collect();
        assert_eq!(
            mutating,
            vec![
                Effect::Mutate,
                Effect::Execute,
                Effect::Create,
                Effect::Delete
            ]
        );
    }

    #[test]
    fn const_descriptor_is_known_and_unknown_is_not() {
        assert!(FS_READ.is_known());
        assert!(!ActivityDescriptor::UNKNOWN.is_known());
        assert!(!ActivityDescriptor::new("", "read", None, Effect::Observe).is_known());
        assert!(!ActivityDescriptor::new("filesystem", " ", None, Effect::Observe).is_known());
        assert!(!ActivityDescriptor::new("filesystem", "read", None, Effect::Other).is_known());
    }

    #[test]
    fn descriptor_serde_round_trips_into_owned_strings() -> TestResult {
        let json = serde_json::to_string(&FS_READ)?;
        assert_eq!(
            json,
            r#"{"domain":"filesystem","verb":"read","resource_field":"path","effect":"observe"}"#
        );
        let back: ActivityDescriptor = serde_json::from_str(&json)?;
        assert_eq!(back, FS_READ);

        let no_field: ActivityDescriptor =
            serde_json::from_str(r#"{"domain":"job","verb":"start","effect":"execute"}"#)?;
        assert_eq!(no_field.resource_field, None);
        assert_eq!(
            no_field,
            ActivityDescriptor::new("job", "start", None, Effect::Execute)
        );
        Ok(())
    }

    #[test]
    fn descriptor_display_is_readable() {
        assert_eq!(
            FS_READ.to_string(),
            "filesystem.read (observe) resource=path"
        );
        assert_eq!(ActivityDescriptor::JOB_RUN.to_string(), "job.run (execute)");
    }
}
