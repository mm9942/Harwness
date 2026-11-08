//! Einfrieren und Auftauen der eigenen Job-cgroup für `job.suspend` /
//! `job.resume` (PL-100 WP 3.5).
//!
//! # Verantwortungsbereich
//! - [`JobFreezer`]: plattformneutrale Schnittstelle, über die der
//!   [`crate::JobManager`] die eigene cgroup eines Jobs einfriert bzw.
//!   auftaut. Sie bekommt nur den Blattnamen (`harw-job-<attempt>`) direkt
//!   unter ihrer delegierten Wurzel, nie einen Pfad.
//! - `CgroupFreezer` (nur Linux): setzt das über
//!   `harw_job_linux::cgroup::CgroupBackend::{freeze, thaw}` um
//!   (`cgroup.freeze` = 1 bzw. 0) und weigert sich, die cgroup einzufrieren,
//!   in der harw selbst läuft (oder die harw enthält).
//! - [`FreezeError`]: typisierte, plattformneutrale Fehler.
//!
//! # Zielwahl
//! Das Ziel kommt nie aus Argumenten, auch nicht aus einer `finding_id`.
//! Der Manager nimmt ausschließlich den bewiesenen Pfad
//! [`crate::JobMeta::job_cgroup_path`] eines Jobs, den der Aufrufer steuern
//! darf (dieselbe Besitzprüfung wie `job.stop`), und reicht dessen
//! Blattnamen nur weiter, wenn der Elternpfad genau die delegierte Wurzel
//! des Freezers ist. Ohne Beweis oder bei einer anderen Wurzel wird nichts
//! eingefroren (fail closed).
//!
//! # Fremde Freezes
//! [`JobFreezer::freeze_requested`] (eigenes `cgroup.freeze`, sofort
//! sichtbar) und [`JobFreezer::is_frozen`] (Kernel-Sicht, auch über eine
//! Vorfahren-cgroup) zeigen dem Manager einen Freeze, den harw nicht
//! vermerkt hat (Operator, Warden). Den übernimmt harw nicht und hebt ihn
//! nie auf.
//!
//! # Produktion
//! Die Montage öffnet einen `CgroupFreezer` über dieselbe delegierte
//! Wurzel, unter der die Job-Runtime die Job-cgroups anlegt
//! (`[jobs] cgroup_root`), und übergibt ihn mit
//! [`crate::JobManager::set_freezer`]. Ohne Freezer lehnt `job.suspend`
//! typisiert ab.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; jeder Aufruf ist ein kurzer, synchroner Zugriff auf eine
//! cgroupfs-Datei. Einfrieren ist im Kernel asynchron;
//! [`JobFreezer::is_frozen`] meldet den Stand.
//!
//! # Examples
//! ```rust,no_run
//! # #[cfg(target_os = "linux")]
//! # fn main() -> Result<(), harw_tool_job::FreezeError> {
//! use harw_tool_job::{CgroupFreezer, JobFreezer};
//!
//! let freezer = CgroupFreezer::open(std::path::Path::new("/sys/fs/cgroup/harw.slice"))?;
//! assert_eq!(freezer.delegated_root(), "/harw.slice");
//! # Ok(())
//! # }
//! # #[cfg(not(target_os = "linux"))]
//! # fn main() {}
//! ```

#[cfg(target_os = "linux")]
use harw_job_linux::CgroupError;
#[cfg(target_os = "linux")]
use harw_job_linux::cgroup::{CgroupBackend, CgroupV2Fs};
#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::Arc;

/// Why freezing or thawing a job cgroup failed.
///
/// # Description
/// Platform-neutral classes of `harw_job_linux::CgroupError`, without the
/// absolute path of the delegated root; the remaining text names at most a
/// cgroup beneath that root.
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum FreezeError {
    /// The kernel or the backend lacks the interface (e.g. no cgroup v2
    /// freezer before Linux 5.2, or no Linux at all).
    #[msg("the cgroup freezer is not supported here ({feature})")]
    Unsupported {
        /// The missing interface.
        feature: &'static str,
    },
    /// The delegated cgroup root is not usable (not cgroup v2, missing or
    /// not delegated to this process).
    #[msg("the delegated cgroup root is not usable")]
    Unavailable,
    /// Permission denied on the job cgroup.
    #[msg("permission denied on the job cgroup")]
    PermissionDenied,
    /// The job cgroup does not exist (any more).
    #[msg("the job cgroup does not exist (any more)")]
    Gone,
    /// The leaf is not a single safe cgroup name.
    #[msg("invalid job cgroup name")]
    InvalidName,
    /// The target is the cgroup harw itself runs in, or one of its
    /// ancestors: freezing it would freeze harw.
    #[msg("refusing to freeze the cgroup harw itself runs in")]
    OwnCgroup,
    /// Any other failure (rendered cgroup error).
    #[msg("cgroup operation failed: {reason}")]
    Failed {
        /// What failed.
        reason: String,
    },
}

/// Freezes and thaws a job's own cgroup (`job.suspend` / `job.resume`).
///
/// # Description
/// Every method takes only the leaf name of a cgroup directly beneath
/// [`JobFreezer::delegated_root`]; the [`crate::JobManager`] derives it from
/// the job's proven cgroup ([`crate::JobMeta::job_cgroup_path`]) after the
/// ownership check. Implementations must never resolve anything outside
/// that root.
///
/// # Concurrency
/// `Send + Sync`; calls are short and synchronous.
pub trait JobFreezer: Send + Sync {
    /// The delegated root, hierarchy-relative as in `/proc/<pid>/cgroup`
    /// (e.g. `/harw.slice`). Only its direct children are ever touched.
    fn delegated_root(&self) -> &str;

    /// Freezes every process of the cgroup `leaf` (`cgroup.freeze` = 1).
    ///
    /// # Errors
    /// [`FreezeError`] when the cgroup is missing, not writable, not
    /// supported, or harw's own cgroup.
    fn freeze(&self, leaf: &str) -> Result<(), FreezeError>;

    /// Thaws the cgroup `leaf` (`cgroup.freeze` = 0).
    ///
    /// # Errors
    /// [`FreezeError`] when the cgroup is missing, not writable or not
    /// supported.
    fn thaw(&self, leaf: &str) -> Result<(), FreezeError>;

    /// The kernel view: whether every process of `leaf` has stopped
    /// (`cgroup.events` `frozen`); freezing is asynchronous. Also `true`
    /// while an ancestor of `leaf` is frozen.
    ///
    /// # Errors
    /// [`FreezeError::Unsupported`] without that probe, otherwise as
    /// [`JobFreezer::freeze`].
    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError>;

    /// Whether a freeze of `leaf` itself is requested (`cgroup.freeze`
    /// reads `1`), by harw or anyone else; `true` at once, before every
    /// process has stopped.
    ///
    /// # Description
    /// Together with [`JobFreezer::is_frozen`] this tells the
    /// [`crate::JobManager`] whether a freeze it did not record is in place
    /// (an operator, the Warden), which it then neither takes over nor
    /// lifts.
    ///
    /// # Errors
    /// [`FreezeError::Unsupported`] without that probe, otherwise as
    /// [`JobFreezer::freeze`].
    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError>;
}

/// The leaf of `path` when its parent is exactly `root` (both
/// hierarchy-relative); `None` otherwise, for the hierarchy root as `root`
/// and for an empty leaf.
pub(crate) fn leaf_beneath<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    if root.is_empty() || !root.starts_with('/') {
        return None;
    }
    let (parent, leaf) = path.rsplit_once('/')?;
    (parent == root && !leaf.is_empty()).then_some(leaf)
}

/// Whether the cgroup `own` lies at or beneath `target` (component-wise).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn is_within(own: &str, target: &str) -> bool {
    own.strip_prefix(target)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(target_os = "linux")]
impl From<CgroupError> for FreezeError {
    fn from(error: CgroupError) -> Self {
        match error {
            CgroupError::Unsupported { feature } => Self::Unsupported { feature },
            CgroupError::CgroupUnavailable { .. } => Self::Unavailable,
            CgroupError::PermissionDenied { .. } => Self::PermissionDenied,
            CgroupError::NotFound { .. } => Self::Gone,
            CgroupError::InvalidName { .. } => Self::InvalidName,
            other => Self::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// [`JobFreezer`] over a `harw_job_linux` cgroup backend rooted at the
/// delegated job root (Linux only).
///
/// # Description
/// Resolves the leaf with `CgroupBackend::reopen` (single safe path
/// component beneath the backend's root) and writes `cgroup.freeze` through
/// `CgroupBackend::freeze` / `thaw`. Before freezing it compares the target
/// with the cgroup this process runs in and refuses harw's own cgroup and
/// its ancestors ([`FreezeError::OwnCgroup`]).
///
/// # Examples
/// ```rust,no_run
/// # #[cfg(target_os = "linux")]
/// # fn main() -> Result<(), harw_tool_job::FreezeError> {
/// use harw_tool_job::{CgroupFreezer, JobFreezer};
///
/// let freezer = CgroupFreezer::open(std::path::Path::new("/sys/fs/cgroup/harw.slice"))?;
/// freezer.freeze("harw-job-work-1-e1")?;
/// freezer.thaw("harw-job-work-1-e1")?;
/// # Ok(())
/// # }
/// # #[cfg(not(target_os = "linux"))]
/// # fn main() {}
/// ```
#[cfg(target_os = "linux")]
pub struct CgroupFreezer {
    backend: Arc<dyn CgroupBackend>,
    root: String,
}

#[cfg(target_os = "linux")]
impl CgroupFreezer {
    /// Wraps `backend`, whose delegated root is `root`, hierarchy-relative
    /// as in `/proc/<pid>/cgroup` (e.g. `/harw.slice`; for
    /// `harw_job_linux::cgroup::CgroupV2Fs` its `root_proc_path`).
    #[must_use]
    pub fn new(backend: Arc<dyn CgroupBackend>, root: impl Into<String>) -> Self {
        Self {
            backend,
            root: root.into(),
        }
    }

    /// Opens the delegated root `path` (absolute, on the cgroup v2 mount)
    /// with the in-house cgroupfs backend.
    ///
    /// # Description
    /// Like every `CgroupV2Fs::open` it enables the available controllers
    /// for children (best effort), which the job runtime does on the same
    /// root anyway.
    ///
    /// # Errors
    /// [`FreezeError::Unavailable`] when `path` is not a delegated, writable
    /// cgroup v2 directory; other classes of the underlying error.
    pub fn open(path: &Path) -> Result<Self, FreezeError> {
        let backend = CgroupV2Fs::open(path)?;
        let root = backend.root_proc_path().to_owned();
        Ok(Self::new(Arc::new(backend), root))
    }
}

#[cfg(target_os = "linux")]
impl fmt::Debug for CgroupFreezer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CgroupFreezer")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// The cgroup-v2 path this process runs in (hierarchy-relative), if known.
#[cfg(target_os = "linux")]
fn own_cgroup() -> Option<String> {
    harw_job_linux::proc::snapshot(std::process::id())
        .ok()
        .and_then(|snapshot| snapshot.cgroup_v2_path)
}

#[cfg(target_os = "linux")]
impl JobFreezer for CgroupFreezer {
    fn delegated_root(&self) -> &str {
        &self.root
    }

    fn freeze(&self, leaf: &str) -> Result<(), FreezeError> {
        let target = format!("{}/{leaf}", self.root.trim_end_matches('/'));
        // Defence in depth: the proof already binds the leaf to the job, but
        // harw must never freeze itself. An unknown own cgroup (no cgroup
        // v2 membership) cannot be the delegated job root.
        if own_cgroup().is_some_and(|own| is_within(&own, &target)) {
            return Err(FreezeError::OwnCgroup);
        }
        let group = self.backend.reopen(leaf)?;
        self.backend.freeze(&group)?;
        Ok(())
    }

    fn thaw(&self, leaf: &str) -> Result<(), FreezeError> {
        let group = self.backend.reopen(leaf)?;
        self.backend.thaw(&group)?;
        Ok(())
    }

    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError> {
        let group = self.backend.reopen(leaf)?;
        Ok(self.backend.is_frozen(&group)?)
    }

    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError> {
        let group = self.backend.reopen(leaf)?;
        Ok(self.backend.freeze_requested(&group)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leaf_beneath_accepts_only_direct_children_of_the_root() {
        assert_eq!(
            leaf_beneath("/harw.slice/harw-job-a", "/harw.slice"),
            Some("harw-job-a")
        );
        assert_eq!(
            leaf_beneath("/harw.slice/harw-job-a", "/harw.slice/"),
            Some("harw-job-a")
        );
        for (path, root) in [
            // Another root, a deeper path, the root itself.
            ("/harw.slice/harw-job-a", "/other.slice"),
            ("/harw.slice/x/harw-job-a", "/harw.slice"),
            ("/harw.slice", "/harw.slice"),
            // The hierarchy root and relative roots are never delegated.
            ("/harw-job-a", "/"),
            ("/harw-job-a", ""),
            ("/harw.slice/harw-job-a", "harw.slice"),
            // Empty leaf, relative path.
            ("/harw.slice/", "/harw.slice"),
            ("harw-job-a", "/harw.slice"),
        ] {
            assert_eq!(leaf_beneath(path, root), None, "{path:?} {root:?}");
        }
    }

    #[test]
    fn test_is_within_is_component_wise() {
        assert!(is_within(
            "/harw.slice/harw-job-a",
            "/harw.slice/harw-job-a"
        ));
        assert!(is_within(
            "/harw.slice/harw-job-a/inner",
            "/harw.slice/harw-job-a"
        ));
        assert!(!is_within(
            "/harw.slice/harw-job-ab",
            "/harw.slice/harw-job-a"
        ));
        assert!(!is_within("/harw.slice", "/harw.slice/harw-job-a"));
    }

    #[test]
    fn test_freeze_error_messages_are_stable() {
        assert_eq!(
            FreezeError::Unsupported {
                feature: "cgroup.freeze"
            }
            .to_string(),
            "the cgroup freezer is not supported here (cgroup.freeze)"
        );
        assert_eq!(
            FreezeError::OwnCgroup.to_string(),
            "refusing to freeze the cgroup harw itself runs in"
        );
        assert_eq!(
            FreezeError::Failed {
                reason: "boom".to_owned()
            }
            .to_string(),
            "cgroup operation failed: boom"
        );
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::*;
        use crate::test_support::fake_cgroup::FakeCgroup;
        use crate::test_support::{TestError, TestResult};
        use std::path::PathBuf;

        #[test]
        fn test_cgroup_errors_map_to_typed_classes() {
            let cases = [
                (
                    CgroupError::Unsupported {
                        feature: "cgroup.freeze",
                    },
                    FreezeError::Unsupported {
                        feature: "cgroup.freeze",
                    },
                ),
                (
                    CgroupError::CgroupUnavailable {
                        root: PathBuf::from("/sys/fs/cgroup/harw.slice"),
                        reason: harw_job_linux::CgroupUnavailableReason::NotDelegated,
                    },
                    FreezeError::Unavailable,
                ),
                (
                    CgroupError::PermissionDenied {
                        path: "harw-job-a".to_owned(),
                    },
                    FreezeError::PermissionDenied,
                ),
                (
                    CgroupError::NotFound {
                        path: "harw-job-a".to_owned(),
                    },
                    FreezeError::Gone,
                ),
                (
                    CgroupError::InvalidName {
                        name: "../x".to_owned(),
                    },
                    FreezeError::InvalidName,
                ),
            ];
            for (error, expected) in cases {
                assert_eq!(FreezeError::from(error), expected);
            }
            let busy = FreezeError::from(CgroupError::Busy {
                path: "harw-job-a".to_owned(),
            });
            assert!(
                matches!(&busy, FreezeError::Failed { reason } if reason.contains("harw-job-a")),
                "{busy:?}"
            );
            // The absolute root path never reaches the message.
            let unavailable = FreezeError::from(CgroupError::CgroupUnavailable {
                root: PathBuf::from("/sys/fs/cgroup/secret.slice"),
                reason: harw_job_linux::CgroupUnavailableReason::RootMissing,
            });
            assert!(!unavailable.to_string().contains("secret"));
        }

        /// `CgroupFreezer` goes through `CgroupBackend::{freeze, thaw}` with
        /// exactly the leaf it was given.
        #[test]
        fn test_cgroup_freezer_uses_backend_freeze_and_thaw() -> TestResult {
            let fake = Arc::new(FakeCgroup::default());
            let freezer =
                CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, "/harw.slice");
            assert_eq!(freezer.delegated_root(), "/harw.slice");
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(false));
            freezer
                .freeze("harw-job-a")
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert_eq!(freezer.is_frozen("harw-job-a"), Ok(true));
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(true));
            freezer
                .thaw("harw-job-a")
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert_eq!(freezer.is_frozen("harw-job-a"), Ok(false));
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(false));
            assert_eq!(
                fake.calls(),
                vec![
                    ("freeze", "harw-job-a".to_owned()),
                    ("thaw", "harw-job-a".to_owned()),
                ]
            );
            Ok(())
        }

        #[test]
        fn test_cgroup_freezer_maps_backend_errors() {
            let fake = Arc::new(FakeCgroup::denying());
            let freezer =
                CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, "/harw.slice");
            assert_eq!(
                freezer.freeze("harw-job-a"),
                Err(FreezeError::PermissionDenied)
            );
            assert_eq!(
                freezer.thaw("harw-job-a"),
                Err(FreezeError::PermissionDenied)
            );
        }

        /// harw never freezes the cgroup it runs in: with the freezer rooted
        /// at the parent of this test's own cgroup, freezing its leaf is
        /// refused before the backend is asked.
        #[test]
        fn test_cgroup_freezer_refuses_its_own_cgroup() {
            let Some(own) = own_cgroup() else {
                return; // No cgroup v2 membership on this host.
            };
            let Some((parent, leaf)) = own.rsplit_once('/') else {
                return;
            };
            if parent.is_empty() || leaf.is_empty() {
                return; // Running in the hierarchy root itself.
            }
            let fake = Arc::new(FakeCgroup::default());
            let freezer = CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, parent);
            assert_eq!(freezer.freeze(leaf), Err(FreezeError::OwnCgroup));
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }
    }
}
