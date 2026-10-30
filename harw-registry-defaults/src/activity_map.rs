//! Tool name → activity descriptor (PL-97 M3, PL-71).
//!
//! # Why a lookup
//! `#[tool(activity(...))]` emits its descriptor as an inherent const of the
//! concrete tool type, and `tool_provider!` collects them as the provider's
//! `TOOL_ACTIVITIES`. Neither survives `Arc<dyn ToolProvider>`: a consumer
//! that only sees the tool name of a call (the TUI activity adapter,
//! `harw-tui/src/app/activity_glue.rs`) cannot reach them. [`activity_for`]
//! answers from two sources:
//!
//! 1. the descriptors the providers declare, collected where
//!    [`crate::profile`] constructs the concrete providers
//!    ([`crate::profile::provider_tool_activity_tables`]: every
//!    `tool_provider!` provider plus `harw_tool_fs::FsToolProvider`, whose
//!    `fs.*` descriptors are delegated, never restated here);
//! 2. [`HAND_WRITTEN_ACTIVITIES`] for the tools whose provider declares none
//!    yet: `doc.read_pdf`, `job.*`, `process.*` and `shell.exec`.
//!
//! A declared descriptor always wins. The parity tests below fail as soon as
//! a provider starts declaring a descriptor for a hand-written entry, so the
//! table can neither restate nor contradict a provider; they also check that
//! every hand-written entry names a real tool and that each `resource` field
//! is an argument of that tool's model-facing spec.
//!
//! # Not an authority
//! A descriptor is descriptive only (PL-71 I4): it grants, implies or checks
//! no permission, is never part of a `ToolSpec` and does not feed approval,
//! sandbox policy or `AUTO_APPROVED_TOOLS`. Consumers use it to group and
//! label activity, nothing else.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use harw_tools::{ActivityDescriptor, Effect};

/// Domain of the job tools; the same spelling as `harw_activity::JOB_DOMAIN`,
/// which routes `job.start` (keyed by its provisional call) into
/// `job-lifecycle@1`.
const JOB: &str = "job";

/// Domain of the process tools (`process.*`, `shell.exec`).
const PROCESS: &str = "process";

/// Hand-written descriptors of the tools whose provider declares none, as
/// `(tool name, descriptor)`, sorted by name.
///
/// - `doc.read_pdf` observes the PDF at its `path` argument (it joins the
///   file read patterns of that path like `fs.read`).
/// - `job.start` executes; its resource is learned from the result (the
///   adapter keys it by its provisional call id and rebases it onto the
///   `job_id`). `job.status`/`job.logs` observe and `job.stop` controls the
///   job named by `job_id`; `job.list` observes without a resource.
/// - `process.list` observes and `process.kill` controls processes selected
///   by a filter, so neither has a resource key.
/// - `shell.exec` executes a command whose side effects no key describes.
pub const HAND_WRITTEN_ACTIVITIES: &[(&str, ActivityDescriptor)] = &[
    (
        "doc.read_pdf",
        ActivityDescriptor::new("document", "read", Some("path"), Effect::Observe),
    ),
    (
        "job.list",
        ActivityDescriptor::new(JOB, "list", None, Effect::Observe),
    ),
    (
        "job.logs",
        ActivityDescriptor::new(JOB, "logs", Some("job_id"), Effect::Observe),
    ),
    (
        "job.start",
        ActivityDescriptor::new(JOB, "start", None, Effect::Execute),
    ),
    (
        "job.status",
        ActivityDescriptor::new(JOB, "status", Some("job_id"), Effect::Observe),
    ),
    (
        "job.stop",
        ActivityDescriptor::new(JOB, "stop", Some("job_id"), Effect::Control),
    ),
    (
        "process.kill",
        ActivityDescriptor::new(PROCESS, "kill", None, Effect::Control),
    ),
    (
        "process.list",
        ActivityDescriptor::new(PROCESS, "list", None, Effect::Observe),
    ),
    (
        "shell.exec",
        ActivityDescriptor::new(PROCESS, "exec", None, Effect::Execute),
    ),
];

/// The activity descriptor of `tool_name`, if any source knows one.
///
/// Declared descriptors (the providers' `TOOL_ACTIVITIES`) win over
/// [`HAND_WRITTEN_ACTIVITIES`]. `None` means unknown semantics: consumers
/// must keep such a call standalone (PL-71 I5), never guess from the name.
///
/// The index is built once per process from static tables; the lookup is a
/// map access.
///
/// # Example
/// ```
/// use harw_registry_defaults::activity_for;
/// use harw_tools::Effect;
///
/// let read = activity_for("fs.read").ok_or("fs.read declares an activity")?;
/// assert_eq!((read.verb.as_ref(), read.effect), ("read", Effect::Observe));
/// assert_eq!(read.resource_field.as_deref(), Some("path"));
/// assert_eq!(activity_for("custom.unknown"), None);
/// # Ok::<(), &'static str>(())
/// ```
#[must_use]
pub fn activity_for(tool_name: &str) -> Option<ActivityDescriptor> {
    index().get(tool_name).cloned()
}

/// Every descriptor the providers declare, as `(tool name, descriptor)`, in
/// the order of [`crate::profile::provider_tool_activity_tables`]. Tools
/// whose `TOOL_ACTIVITIES` entry is `None` are left out.
#[must_use]
pub fn declared_tool_activities() -> Vec<(&'static str, ActivityDescriptor)> {
    crate::profile::provider_tool_activity_tables()
        .iter()
        .flat_map(|(names, activities)| {
            names
                .iter()
                .zip(activities.iter())
                .filter_map(|(name, activity)| activity.clone().map(|activity| (*name, activity)))
        })
        .collect()
}

/// Name → descriptor, hand-written entries first so the declared ones
/// overwrite them.
fn index() -> &'static BTreeMap<&'static str, ActivityDescriptor> {
    static INDEX: OnceLock<BTreeMap<&'static str, ActivityDescriptor>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: BTreeMap<&'static str, ActivityDescriptor> = HAND_WRITTEN_ACTIVITIES
            .iter()
            .map(|(name, descriptor)| (*name, descriptor.clone()))
            .collect();
        // Deklarierte Deskriptoren gewinnen (der Paritätstest verbietet
        // ohnehin eine Überschneidung).
        index.extend(declared_tool_activities());
        index
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use harw_extension_api::{ToolProvider, ToolSpec};
    use harw_tool_fs::FsToolProvider;

    use super::*;
    use crate::profile::{
        JobWiring, known_tool_specs, provider_tool_activity_tables, provider_tool_tables,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    /// Argument names of a model-facing spec.
    fn spec_arguments(spec: &ToolSpec) -> BTreeSet<String> {
        let ToolSpec::Function(function) = spec;
        function
            .parameters
            .properties
            .as_ref()
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Specs of every tool a descriptor may name: the profile tools plus
    /// `job.*` (the job provider needs a `JobManager` and is in no profile;
    /// a manager over a temporary state directory starts nothing).
    fn specs_by_name() -> TestResult<BTreeMap<String, ToolSpec>> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let manager = harw_tool_job::JobManager::new(
            harw_tool_job::JobManagerConfig::new(dir.path()),
            Arc::new(harw_tool_job::NoopNotifier),
        )
        .map_err(ctx("job manager"))?;
        let jobs = JobWiring::new(manager, Arc::new(harw_tool_job::NoLineage))
            .tool_provider(&harw_tool_shell::ShellToolProvider::default());
        Ok(known_tool_specs()
            .iter()
            .cloned()
            .chain(jobs.tools())
            .map(|spec| (spec.name().to_owned(), spec))
            .collect())
    }

    /// PL-97 M3, Parität (PR #154, Thread zu `harw-tools/src/provider_macro.rs`):
    /// jeder Deskriptor, den ein Provider in `TOOL_ACTIVITIES` deklariert,
    /// kommt über `activity_for` unverändert an — die Typlöschung der
    /// Provider verliert keinen.
    #[test]
    fn test_activity_for_matches_every_declared_descriptor() {
        let mut declared = 0usize;
        for (names, activities) in provider_tool_activity_tables() {
            assert_eq!(names.len(), activities.len(), "indexgleich: {names:?}");
            for (name, activity) in names.iter().zip(activities) {
                if let Some(activity) = activity {
                    declared += 1;
                    assert_eq!(activity_for(name).as_ref(), Some(activity), "{name}");
                }
            }
        }
        assert_eq!(declared, declared_tool_activities().len());
        assert!(declared >= FsToolProvider::TOOL_NAMES.len());
    }

    /// PL-97 M3: `fs.*` wird an `FsToolProvider::TOOL_ACTIVITIES` delegiert,
    /// nicht in der Handtabelle wiederholt.
    #[test]
    fn test_fs_activities_are_delegated_to_the_fs_provider() -> TestResult {
        for (name, activity) in FsToolProvider::TOOL_NAMES
            .iter()
            .zip(FsToolProvider::TOOL_ACTIVITIES)
        {
            let activity = activity
                .as_ref()
                .ok_or(TestError::Missing("every fs.* tool declares an activity"))?;
            assert_eq!(activity_for(name).as_ref(), Some(activity), "{name}");
            assert!(
                HAND_WRITTEN_ACTIVITIES.iter().all(|(hand, _)| hand != name),
                "{name}: fs.* gehört nicht in die Handtabelle"
            );
        }
        Ok(())
    }

    /// PL-97 M3: die Handtabelle wiederholt keinen deklarierten Deskriptor,
    /// ist sortiert, dublettenfrei und trägt nur bekannte Semantik. Beginnt
    /// ein Provider, einen dieser Deskriptoren selbst zu deklarieren, schlägt
    /// dieser Test fehl und der Eintrag muss weg.
    #[test]
    fn test_hand_written_activities_never_restate_a_declared_one() {
        let declared: BTreeMap<&str, ActivityDescriptor> =
            declared_tool_activities().into_iter().collect();
        let names: Vec<&str> = HAND_WRITTEN_ACTIVITIES
            .iter()
            .map(|(name, _)| *name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "sortiert und dublettenfrei");
        for (name, descriptor) in HAND_WRITTEN_ACTIVITIES {
            assert!(
                !declared.contains_key(name),
                "{name}: der Provider deklariert den Deskriptor bereits"
            );
            assert!(descriptor.is_known(), "{name}: {descriptor}");
            assert_eq!(activity_for(name).as_ref(), Some(descriptor), "{name}");
        }
    }

    /// PL-97 M3: jeder Deskriptor (deklariert oder handgeschrieben) gehört zu
    /// einem echten Werkzeug, und sein Ressourcenfeld ist ein Argument der
    /// modell-sichtbaren Spezifikation.
    #[test]
    fn test_descriptors_name_real_tools_and_arguments() -> TestResult {
        let specs = specs_by_name()?;
        let all = HAND_WRITTEN_ACTIVITIES
            .iter()
            .map(|(name, descriptor)| (*name, descriptor.clone()))
            .chain(declared_tool_activities());
        for (name, descriptor) in all {
            let spec = specs
                .get(name)
                .ok_or_else(|| TestError::Unexpected(format!("{name}: kein solches Werkzeug")))?;
            if let Some(field) = descriptor.resource_field.as_deref() {
                assert!(
                    spec_arguments(spec).contains(field),
                    "{name}: Ressourcenfeld `{field}` ist kein Argument"
                );
            }
        }
        Ok(())
    }

    /// PL-97 M3, Drift: die Aktivitätstabelle führt genau die Provider der
    /// Skill-Tabelle (in derselben Reihenfolge) plus `FsToolProvider`; ein
    /// neuer `tool_provider!`-Provider kann nicht nur in einer der beiden
    /// fehlen.
    #[test]
    fn test_activity_tables_follow_the_provider_tables() {
        let activities = provider_tool_activity_tables();
        let skills = provider_tool_tables();
        assert_eq!(activities.len(), skills.len() + 1);
        for ((activity_names, _), (skill_names, _)) in activities.iter().zip(skills.iter()) {
            assert_eq!(activity_names, skill_names);
        }
        assert_eq!(
            activities.last().map(|(names, _)| *names),
            Some(FsToolProvider::TOOL_NAMES)
        );
    }

    /// PL-97 M3: die Effekte, an denen die TUI gruppiert — Jobs über ihre
    /// `job_id`, `shell.exec` als Ausführung, Unbekanntes bleibt unbekannt.
    #[test]
    fn test_hand_written_effects_and_unknown_tools() -> TestResult {
        let effect = |name: &str| -> Result<(Effect, Option<String>), TestError> {
            let descriptor =
                activity_for(name).ok_or_else(|| TestError::Unexpected(format!("{name} fehlt")))?;
            Ok((
                descriptor.effect,
                descriptor.resource_field.map(|field| field.into_owned()),
            ))
        };
        assert_eq!(effect("job.start")?, (Effect::Execute, None));
        for name in ["job.status", "job.logs"] {
            assert_eq!(effect(name)?, (Effect::Observe, Some("job_id".into())));
        }
        assert_eq!(
            effect("job.stop")?,
            (Effect::Control, Some("job_id".into()))
        );
        assert_eq!(effect("shell.exec")?, (Effect::Execute, None));
        assert_eq!(
            effect("doc.read_pdf")?,
            (Effect::Observe, Some("path".into()))
        );
        for unknown in ["custom.x", "transfer_to_explorer", "tool.result", ""] {
            assert_eq!(activity_for(unknown), None, "{unknown}");
        }
        Ok(())
    }
}
