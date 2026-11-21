//! Prozess-Umfang der auflistenden Werkzeuge `sys.ps`, `sys.pgrep` und
//! `sys.top` (PL-100 Phase 4, THREAT-MODEL B22).
//!
//! # Verantwortung
//! - [`PsScope`]: der angefragte Umfang, `own` (Standard) oder `all`.
//! - [`ScopePolicy`]: entscheidet, ob ein vom Modell angefragtes `all`
//!   gilt. Ohne Freigabe-Zelle (Vorgabe) nie; mit Zelle nur, solange sie
//!   [`ApprovalMode::FullAccess`] zeigt (live gelesen, eine Umschaltung wirkt
//!   sofort). Dazu optionale weitere cgroup-Wurzeln (die delegierte
//!   Job-Wurzel aus `[jobs] cgroup_root`).
//! - `ScopeFilter` (crate-intern): löst `own` gegen `/proc` auf und filtert
//!   einen Prozess-Schnappschuss.
//!
//! # Was `own` umfasst
//! 1. den cgroup-v2-Teilbaum des aufrufenden Prozesses; sitzt er im Blatt
//!    `harw-main` (`DelegateSubgroup=harw-main` der harw-Units), gilt dessen
//!    Elternteil als Wurzel, damit die benachbarten Job-cgroups dazugehören
//!    (wie `harw_job_runtime::host::own_cgroup_root_candidate`);
//! 2. die per [`ScopePolicy::with_extra_root`] übergebenen Wurzeln;
//! 3. immer die Nachfahren des aufrufenden Prozesses (über `ppid`), auch
//!    wenn sie in eine fremde cgroup gewechselt sind.
//!
//! Ist der eigene cgroup unbrauchbar (Wurzel `/` wie in Containern mit
//! cgroup-Namespace, cgroup v1, unlesbar, oder ein Pfad mit `..`), bleibt nur
//! Punkt 3 (plus 2): Umfang `own-process-tree`. Ist auch die eigene PID
//! unlesbar, scheitert das Werkzeug (fail closed) statt alles zu listen.
//!
//! # Nebenläufigkeit
//! Rein lesend; [`ScopePolicy`] teilt nur die klonbare Freigabe-Zelle.
//!
//! # Examples
//! ```
//! use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
//! use harw_tool_sys::scope::{PsScope, ScopePolicy};
//!
//! assert_eq!(ScopePolicy::own_only().resolve(None), Ok(PsScope::Own));
//! assert!(ScopePolicy::own_only().resolve(Some("all")).is_err());
//!
//! let mode = ApprovalModeCell::new(ApprovalMode::Delegated);
//! let policy = ScopePolicy::own_only().with_full_access_gate(mode.clone());
//! assert!(policy.resolve(Some("all")).is_err());
//! mode.set(ApprovalMode::FullAccess);
//! assert_eq!(policy.resolve(Some("all")), Ok(PsScope::All));
//! ```

use crate::procfs::{ProcFs, ProcInfo};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use std::collections::{BTreeMap, HashSet};

/// Blattname, in dem harw unter `DelegateSubgroup=harw-main` selbst sitzt
/// (gleicher Wert wie `harw_job_runtime::host::DELEGATE_SUBGROUP`).
pub const DELEGATE_SUBGROUP: &str = "harw-main";

/// Einhängepunkt von cgroup v2; Dateisystem-Pfade darunter werden in die
/// Form von `/proc/<pid>/cgroup` umgerechnet.
const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";

/// Höchstzahl zusätzlicher cgroup-Wurzeln einer [`ScopePolicy`].
pub const MAX_EXTRA_ROOTS: usize = 16;

/// Welche Prozesse ein auflistendes `sys.*`-Werkzeug zeigen darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsScope {
    /// Nur die eigenen Prozesse (cgroup-Teilbaum, Job-cgroups, Nachfahren).
    Own,
    /// Alle sichtbaren Prozesse (nur unter Full Access oder aus Host-Code).
    All,
}

impl PsScope {
    /// Der Wert des Werkzeug-Arguments `scope`.
    ///
    /// # Returns
    /// `"own"` oder `"all"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::All => "all",
        }
    }

    /// Liest das Werkzeug-Argument `scope`; fehlend heißt `own`.
    ///
    /// # Errors
    /// Eine Meldung für jeden anderen Wert als `own`/`all`.
    pub fn parse(requested: Option<&str>) -> Result<Self, String> {
        match requested {
            None | Some("own") => Ok(Self::Own),
            Some("all") => Ok(Self::All),
            Some(other) => Err(format!(
                "invalid scope '{}': expected 'own' or 'all'",
                harw_untrusted_text::escape_line(other, 16)
            )),
        }
    }
}

/// Ungültige zusätzliche cgroup-Wurzel für [`ScopePolicy::with_extra_root`].
///
/// Trägt bewusst nicht den Pfad, nur den Grund.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidCgroupRoot {
    /// Nicht absolut (weder `/<pfad>` noch unter `/sys/fs/cgroup`).
    NotAbsolute,
    /// Die Wurzel `/` selbst (wäre „alles“).
    RootCgroup,
    /// Ein Bestandteil `.` oder `..` bzw. ein leerer Bestandteil.
    NotNormalized,
    /// Mehr als [`MAX_EXTRA_ROOTS`] Wurzeln.
    TooMany,
}

impl std::fmt::Display for InvalidCgroupRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::NotAbsolute => "path is not absolute",
            Self::RootCgroup => "the root cgroup would list every process",
            Self::NotNormalized => "path has empty, '.' or '..' components",
            Self::TooMany => "too many extra roots",
        };
        write!(f, "invalid cgroup root: {reason}")
    }
}

impl std::error::Error for InvalidCgroupRoot {}

/// Entscheidet über den Umfang der auflistenden `sys.*`-Werkzeuge.
///
/// # Description
/// Die Vorgabe ([`Self::own_only`], `Default`) erlaubt nur `own`. Erst
/// [`Self::with_full_access_gate`] macht `scope = "all"` möglich, und auch
/// dann nur, solange die übergebene Zelle [`ApprovalMode::FullAccess`] zeigt.
///
/// # Concurrency
/// `Clone` teilt die Freigabe-Zelle (`Arc` innen); Lesen ist sperrarm.
///
/// # Examples
/// ```
/// use harw_tool_sys::scope::ScopePolicy;
///
/// let policy = ScopePolicy::own_only().with_extra_root("/sys/fs/cgroup/harw.slice/jobs")?;
/// assert_eq!(policy.extra_roots(), ["/harw.slice/jobs"]);
/// assert!(!policy.allows_all());
/// # Ok::<(), harw_tool_sys::scope::InvalidCgroupRoot>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct ScopePolicy {
    /// Freigabe-Zelle der Sitzung; `None`: `all` ist nie erlaubt.
    full_access: Option<ApprovalModeCell>,
    /// Weitere cgroup-v2-Wurzeln (Form von `/proc/<pid>/cgroup`).
    extra_roots: Vec<String>,
}

impl ScopePolicy {
    /// Nur `own`, keine weiteren Wurzeln (die Vorgabe).
    #[must_use]
    pub fn own_only() -> Self {
        Self::default()
    }

    /// Erlaubt `scope = "all"`, solange `mode` [`ApprovalMode::FullAccess`] zeigt.
    ///
    /// # Arguments
    /// - `mode`: die Freigabe-Zelle der Sitzung (ein Klon teilt den Modus).
    #[must_use]
    pub fn with_full_access_gate(mut self, mode: ApprovalModeCell) -> Self {
        self.full_access = Some(mode);
        self
    }

    /// Nimmt eine weitere cgroup-Wurzel in `own` auf (z. B. die delegierte
    /// Job-Wurzel aus `[jobs] cgroup_root`).
    ///
    /// # Arguments
    /// - `root`: Dateisystem-Pfad unter `/sys/fs/cgroup` oder cgroup-v2-Pfad
    ///   in der Form von `/proc/<pid>/cgroup` (beginnt mit `/`).
    ///
    /// # Errors
    /// [`InvalidCgroupRoot`] für relative, nicht normalisierte Pfade, die
    /// Wurzel `/` oder mehr als [`MAX_EXTRA_ROOTS`] Wurzeln.
    pub fn with_extra_root(mut self, root: &str) -> Result<Self, InvalidCgroupRoot> {
        if self.extra_roots.len() >= MAX_EXTRA_ROOTS {
            return Err(InvalidCgroupRoot::TooMany);
        }
        let path = normalize_root(root)?;
        if !self.extra_roots.contains(&path) {
            self.extra_roots.push(path);
        }
        Ok(self)
    }

    /// Die zusätzlichen Wurzeln (cgroup-v2-Form).
    #[must_use]
    pub fn extra_roots(&self) -> &[String] {
        &self.extra_roots
    }

    /// Ob `scope = "all"` gerade gilt.
    #[must_use]
    pub fn allows_all(&self) -> bool {
        self.full_access
            .as_ref()
            .is_some_and(|mode| mode.get() == ApprovalMode::FullAccess)
    }

    /// Löst das angefragte `scope`-Argument gegen diese Politik auf.
    ///
    /// # Errors
    /// Ungültiger Wert, oder `all` ohne Full Access.
    pub fn resolve(&self, requested: Option<&str>) -> Result<PsScope, String> {
        match PsScope::parse(requested)? {
            PsScope::Own => Ok(PsScope::Own),
            PsScope::All if self.allows_all() => Ok(PsScope::All),
            PsScope::All => Err(
                "scope 'all' is only available in Full Access approval mode; \
                 use the default scope 'own' (this session's processes)"
                    .to_owned(),
            ),
        }
    }
}

/// Normalisiert eine zusätzliche Wurzel auf die cgroup-v2-Form.
fn normalize_root(root: &str) -> Result<String, InvalidCgroupRoot> {
    let trimmed = root.trim_end_matches('/');
    let relative = match trimmed.strip_prefix(CGROUP2_MOUNT) {
        Some(rest) if rest.is_empty() => return Err(InvalidCgroupRoot::RootCgroup),
        Some(rest) if rest.starts_with('/') => rest,
        Some(_) => return Err(InvalidCgroupRoot::NotNormalized),
        None if trimmed.is_empty() && root.starts_with('/') => {
            return Err(InvalidCgroupRoot::RootCgroup);
        }
        None if trimmed.starts_with('/') => trimmed,
        None => return Err(InvalidCgroupRoot::NotAbsolute),
    };
    if !is_normalized(relative) {
        return Err(InvalidCgroupRoot::NotNormalized);
    }
    Ok(relative.to_owned())
}

/// `true`, wenn `path` mit `/` beginnt und keinen leeren, `.`- oder
/// `..`-Bestandteil hat (die Wurzel `/` allein ist nicht normalisiert).
fn is_normalized(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    })
}

/// Liegt `candidate` im Teilbaum `root` (gleich oder `root/…`)?
///
/// Pfade mit `..` (cgroups außerhalb des eigenen cgroup-Namespace) liegen nie
/// im Teilbaum.
pub(crate) fn in_subtree(candidate: &str, root: &str) -> bool {
    if !is_normalized(candidate) || !is_normalized(root) {
        return false;
    }
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Wurzel des eigenen cgroup-Teilbaums; `None`, wenn unbrauchbar.
///
/// Das Blatt [`DELEGATE_SUBGROUP`] wird durch seinen Elternteil ersetzt, damit
/// die Job-cgroups daneben dazugehören. Die Wurzel `/` ist nie brauchbar.
pub(crate) fn own_scope_root(procfs: &ProcFs) -> Option<String> {
    let own = procfs.own_cgroup()?;
    let parent = own
        .rsplit_once('/')
        .and_then(|(parent, leaf)| (leaf == DELEGATE_SUBGROUP).then(|| parent.to_owned()));
    let root = parent.unwrap_or(own);
    is_normalized(&root).then_some(root)
}

/// Aufgelöster Umfang für genau einen Aufruf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeFilter {
    /// Alles.
    All,
    /// Eigene cgroup-Wurzeln und/oder der eigene Prozessbaum.
    Own {
        /// cgroup-v2-Wurzeln (eigener Teilbaum zuerst, dann die zusätzlichen).
        roots: Vec<String>,
        /// `true`, wenn der eigene cgroup brauchbar war.
        own_cgroup: bool,
        /// Eigene PID als Wurzel des Prozessbaums, falls lesbar.
        tree_root: Option<i32>,
    },
}

impl ScopeFilter {
    /// Löst `scope` gegen `procfs` auf.
    ///
    /// # Errors
    /// Bei `Own`, wenn weder ein brauchbarer eigener cgroup noch die eigene
    /// PID noch eine zusätzliche Wurzel bekannt ist (fail closed).
    pub(crate) fn resolve(
        procfs: &ProcFs,
        scope: PsScope,
        extra_roots: &[String],
    ) -> Result<Self, String> {
        match scope {
            PsScope::All => Ok(Self::All),
            PsScope::Own => {
                let own_root = own_scope_root(procfs);
                let own_cgroup = own_root.is_some();
                let mut roots: Vec<String> = own_root.into_iter().collect();
                for root in extra_roots {
                    if is_normalized(root) && !roots.contains(root) {
                        roots.push(root.to_owned());
                    }
                }
                let tree_root = procfs.own_pid();
                if roots.is_empty() && tree_root.is_none() {
                    return Err(
                        "own cgroup and own process are unknown; this tool lists only the calling session's processes"
                            .to_owned(),
                    );
                }
                Ok(Self::Own {
                    roots,
                    own_cgroup,
                    tree_root,
                })
            }
        }
    }

    /// Kennung für die Ausgabe: `all`, `own-cgroup` oder `own-process-tree`.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Own {
                own_cgroup: true, ..
            } => "own-cgroup",
            Self::Own { .. } => "own-process-tree",
        }
    }

    /// Behält nur die Prozesse im Umfang (Reihenfolge bleibt).
    pub(crate) fn retain(&self, procfs: &ProcFs, processes: Vec<ProcInfo>) -> Vec<ProcInfo> {
        let Self::Own {
            roots, tree_root, ..
        } = self
        else {
            return processes;
        };
        let tree = tree_root.map_or_else(HashSet::new, |pid| descendants(&processes, pid));
        processes
            .into_iter()
            .filter(|info| {
                tree.contains(&info.stat.pid)
                    || (!roots.is_empty()
                        && procfs.cgroup_of(info.stat.pid).is_some_and(|cgroup| {
                            roots.iter().any(|root| in_subtree(&cgroup, root))
                        }))
            })
            .collect()
    }
}

/// `root` und alle seine Nachfahren im Schnappschuss (über `ppid`).
///
/// Zyklen in kaputten Bäumen enden am `seen`-Satz; die Arbeit ist durch die
/// Schnappschussgröße begrenzt.
fn descendants(processes: &[ProcInfo], root: i32) -> HashSet<i32> {
    let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for info in processes {
        if info.stat.pid != info.stat.ppid {
            children
                .entry(info.stat.ppid)
                .or_default()
                .push(info.stat.pid);
        }
    }
    let mut seen: HashSet<i32> = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::StatFields;
    use crate::test_support::{TestError, TestResult};
    use std::fs;

    fn info(pid: i32, ppid: i32) -> ProcInfo {
        ProcInfo {
            stat: StatFields {
                pid,
                comm: "p".to_owned(),
                state: 'S',
                ppid,
                utime: 0,
                stime: 0,
                threads: 1,
                starttime: 1,
                vsize: 0,
                rss_pages: 0,
            },
            uid: 1000,
            vm_rss_kib: None,
            cmdline: Vec::new(),
        }
    }

    fn pids(list: &[ProcInfo]) -> Vec<i32> {
        list.iter().map(|info| info.stat.pid).collect()
    }

    /// `/proc`-Baum mit `self/cgroup` (optional), `self/stat` (optional) und
    /// je PID `stat` plus `cgroup`.
    fn tree(
        own_cgroup: Option<&str>,
        own_pid: Option<i32>,
        procs: &[(i32, i32, &str)],
    ) -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::create_dir_all(root.join("self"))?;
        if let Some(cgroup) = own_cgroup {
            fs::write(root.join("self/cgroup"), format!("0::{cgroup}\n"))?;
        }
        if let Some(pid) = own_pid {
            fs::write(
                root.join("self/stat"),
                format!("{pid} (harw) S 1 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 1 4096 1 0"),
            )?;
        }
        for (pid, _, cgroup) in procs {
            let base = root.join(pid.to_string());
            fs::create_dir_all(&base)?;
            fs::write(base.join("cgroup"), format!("0::{cgroup}\n"))?;
        }
        Ok(dir)
    }

    fn snapshot(procs: &[(i32, i32, &str)]) -> Vec<ProcInfo> {
        procs
            .iter()
            .map(|(pid, ppid, _)| info(*pid, *ppid))
            .collect()
    }

    #[test]
    fn parse_and_resolve_scope() -> TestResult {
        assert_eq!(PsScope::parse(None), Ok(PsScope::Own));
        assert_eq!(PsScope::parse(Some("own")), Ok(PsScope::Own));
        assert_eq!(PsScope::parse(Some("all")), Ok(PsScope::All));
        let bad = PsScope::parse(Some("ALL\u{1b}"))
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(
            bad.contains("\\u{001b}") && !bad.contains('\u{1b}'),
            "{bad}"
        );
        assert_eq!(PsScope::Own.as_str(), "own");
        assert_eq!(PsScope::All.as_str(), "all");
        Ok(())
    }

    #[test]
    fn scope_all_only_under_full_access() -> TestResult {
        // Ohne Zelle: nie.
        let plain = ScopePolicy::own_only();
        assert!(!plain.allows_all());
        assert!(plain.resolve(Some("all")).is_err());
        assert_eq!(plain.resolve(Some("own")), Ok(PsScope::Own));
        // Mit Zelle: nur bei FullAccess, live umgeschaltet.
        let mode = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let gated = ScopePolicy::own_only().with_full_access_gate(mode.clone());
        for (current, allowed) in [
            (ApprovalMode::AlwaysAsk, false),
            (ApprovalMode::Delegated, false),
            (ApprovalMode::FullAccess, true),
            (ApprovalMode::Delegated, false),
        ] {
            mode.set(current);
            assert_eq!(gated.allows_all(), allowed, "{current}");
            assert_eq!(gated.resolve(Some("all")).is_ok(), allowed, "{current}");
            assert_eq!(gated.resolve(None), Ok(PsScope::Own));
        }
        let message = plain
            .resolve(Some("all"))
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(message.contains("Full Access"), "{message}");
        Ok(())
    }

    #[test]
    fn extra_roots_are_normalized_and_validated() -> TestResult {
        let policy = ScopePolicy::own_only()
            .with_extra_root("/sys/fs/cgroup/harw.slice/jobs/")
            .map_err(|e| TestError::Unexpected(e.to_string()))?
            .with_extra_root("/user.slice/x.service")
            .map_err(|e| TestError::Unexpected(e.to_string()))?
            .with_extra_root("/user.slice/x.service")
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(
            policy.extra_roots(),
            ["/harw.slice/jobs", "/user.slice/x.service"]
        );
        for (bad, expected) in [
            ("/", InvalidCgroupRoot::RootCgroup),
            ("/sys/fs/cgroup", InvalidCgroupRoot::RootCgroup),
            ("/sys/fs/cgroup/", InvalidCgroupRoot::RootCgroup),
            ("relative/x", InvalidCgroupRoot::NotAbsolute),
            ("", InvalidCgroupRoot::NotAbsolute),
            ("/a/../b", InvalidCgroupRoot::NotNormalized),
            ("/a//b", InvalidCgroupRoot::NotNormalized),
            ("/sys/fs/cgroupX/a", InvalidCgroupRoot::NotNormalized),
        ] {
            assert_eq!(
                ScopePolicy::own_only().with_extra_root(bad).err(),
                Some(expected),
                "{bad}"
            );
        }
        let mut full = ScopePolicy::own_only();
        for index in 0..MAX_EXTRA_ROOTS {
            full = full
                .with_extra_root(&format!("/r{index}"))
                .map_err(|e| TestError::Unexpected(e.to_string()))?;
        }
        assert_eq!(
            full.with_extra_root("/one-more").err(),
            Some(InvalidCgroupRoot::TooMany)
        );
        Ok(())
    }

    #[test]
    fn subtree_check_rejects_lookalike_prefixes_and_dotdot() {
        assert!(in_subtree("/a/b", "/a/b"));
        assert!(in_subtree("/a/b/c", "/a/b"));
        assert!(!in_subtree("/a/bc", "/a/b"));
        assert!(!in_subtree("/a/b/../../x", "/a/b"));
        assert!(!in_subtree("/../x", "/a"));
        assert!(!in_subtree("/a/b", "/"));
        assert!(!in_subtree("/", "/"));
    }

    #[test]
    fn delegate_subgroup_leaf_widens_to_the_unit() -> TestResult {
        let procs = [
            (10, 1, "/system.slice/harw-control.service/harw-main"),
            (20, 1, "/system.slice/harw-control.service/job-1"),
            (21, 20, "/system.slice/harw-control.service/job-1/sub"),
            (30, 1, "/system.slice/sshd.service"),
            (31, 1, "/system.slice/harw-control.service2/job"),
        ];
        let dir = tree(
            Some("/system.slice/harw-control.service/harw-main"),
            None,
            &procs,
        )?;
        let procfs = ProcFs::at(dir.path());
        assert_eq!(
            own_scope_root(&procfs).as_deref(),
            Some("/system.slice/harw-control.service")
        );
        let filter =
            ScopeFilter::resolve(&procfs, PsScope::Own, &[]).map_err(TestError::Unexpected)?;
        assert_eq!(filter.label(), "own-cgroup");
        assert_eq!(
            pids(&filter.retain(&procfs, snapshot(&procs))),
            vec![10, 20, 21]
        );
        // Ein anderes Blatt wird nicht erweitert.
        let other = tree(Some("/x.service/harw-main2"), None, &[])?;
        assert_eq!(
            own_scope_root(&ProcFs::at(other.path())).as_deref(),
            Some("/x.service/harw-main2")
        );
        // `/harw-main` direkt unter der Wurzel ergäbe `/`: unbrauchbar.
        let top = tree(Some("/harw-main"), None, &[])?;
        assert_eq!(own_scope_root(&ProcFs::at(top.path())), None);
        Ok(())
    }

    #[test]
    fn extra_roots_and_own_descendants_are_in_scope() -> TestResult {
        let procs = [
            (10, 1, "/user.slice/s1"),
            // Kind von 10, aber in eine fremde cgroup gewechselt.
            (11, 10, "/user.slice/app.slice/run-x.scope"),
            (12, 11, "/user.slice/app.slice/run-x.scope"),
            (20, 1, "/harw.slice/jobs/j1"),
            (30, 1, "/system.slice/sshd.service"),
        ];
        let dir = tree(Some("/user.slice/s1"), Some(10), &procs)?;
        let procfs = ProcFs::at(dir.path());
        let filter = ScopeFilter::resolve(&procfs, PsScope::Own, &["/harw.slice/jobs".to_owned()])
            .map_err(TestError::Unexpected)?;
        assert_eq!(
            pids(&filter.retain(&procfs, snapshot(&procs))),
            vec![10, 11, 12, 20]
        );
        let all =
            ScopeFilter::resolve(&procfs, PsScope::All, &[]).map_err(TestError::Unexpected)?;
        assert_eq!(all.label(), "all");
        assert_eq!(all.retain(&procfs, snapshot(&procs)).len(), 5);
        Ok(())
    }

    #[test]
    fn unusable_own_cgroup_falls_back_to_the_process_tree() -> TestResult {
        let procs = [
            (1, 0, "/"),
            (10, 1, "/"),
            (11, 10, "/"),
            (12, 11, "/"),
            (20, 1, "/"),
        ];
        for own in [Some("/"), None, Some("/../outside")] {
            let dir = tree(own, Some(10), &procs)?;
            let procfs = ProcFs::at(dir.path());
            let filter =
                ScopeFilter::resolve(&procfs, PsScope::Own, &[]).map_err(TestError::Unexpected)?;
            assert_eq!(filter.label(), "own-process-tree", "{own:?}");
            assert_eq!(
                pids(&filter.retain(&procfs, snapshot(&procs))),
                vec![10, 11, 12]
            );
        }
        // cgroup v1 (keine `0::`-Zeile) zählt ebenso als unbrauchbar.
        let dir = tree(None, Some(10), &procs)?;
        fs::write(dir.path().join("self/cgroup"), "12:cpu:/x\n")?;
        let procfs = ProcFs::at(dir.path());
        assert_eq!(
            ScopeFilter::resolve(&procfs, PsScope::Own, &[])
                .map_err(TestError::Unexpected)?
                .label(),
            "own-process-tree"
        );
        // Weder cgroup noch PID: fail closed.
        let none = tree(Some("/"), None, &procs)?;
        assert!(ScopeFilter::resolve(&ProcFs::at(none.path()), PsScope::Own, &[]).is_err());
        Ok(())
    }

    #[test]
    fn descendants_survive_cycles() {
        let procs = vec![info(5, 6), info(6, 5), info(7, 5), info(8, 8)];
        let set = descendants(&procs, 5);
        let mut found: Vec<i32> = set.into_iter().collect();
        found.sort_unstable();
        assert_eq!(found, vec![5, 6, 7]);
        assert_eq!(descendants(&procs, 8).len(), 1);
    }

    #[test]
    fn real_proc_own_scope_contains_this_process() -> TestResult {
        let procfs = ProcFs::real();
        let me = i32::try_from(std::process::id()).map_err(|_| TestError::Missing("pid"))?;
        let filter =
            ScopeFilter::resolve(&procfs, PsScope::Own, &[]).map_err(TestError::Unexpected)?;
        let mine: Vec<ProcInfo> = procfs.process(me, false).into_iter().collect();
        assert_eq!(pids(&filter.retain(&procfs, mine)), vec![me]);
        Ok(())
    }
}
