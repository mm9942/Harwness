//! Linux Bubblewrap launch backend.
//!
//! The core only carries a [`SandboxSpec`](crate::SandboxSpec); this module is
//! the syscall-adjacent consumer that turns it into a minimal `bwrap` command.
//! It never mounts a host home, parent workspace, or arbitrary environment.
//!
//! W1-03 (F-021): Das `bwrap`-Binary wird ausschließlich an festen,
//! root-kontrollierten Pfaden gesucht ([`BWRAP_CANDIDATES`]), niemals über
//! `PATH`. Ein workspace-beschreibbarer `PATH`-Eintrag kann damit kein
//! gefälschtes `bwrap` mehr unterschieben, das unsandboxed auf dem Host liefe.

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command};

use crate::{Permission, SandboxError, SandboxResult, SandboxSpec};

/// Feste Suchpfade für Bubblewrap in Prioritätsreihenfolge. `PATH` wird nie
/// ausgewertet.
pub const BWRAP_CANDIDATES: [&str; 2] = ["/usr/bin/bwrap", "/bin/bwrap"];

/// A fully determined Bubblewrap invocation. Keeping it inspectable makes
/// policy tests possible without launching a process on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapCommandPlan {
    args: Vec<OsString>,
}

impl BwrapCommandPlan {
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }
}

/// Builds and launches process sandboxes for tools and stdio MCP servers.
#[derive(Debug, Clone)]
pub struct BwrapLauncher {
    executable: PathBuf,
    /// Optionale Obergrenze für das tmpfs unter `/tmp` (bwrap `--size`, ab
    /// bubblewrap 0.7.0). `None` = Kernel-Default (halber RAM).
    tmpfs_size: Option<NonZeroU64>,
}

impl Default for BwrapLauncher {
    /// Sucht `bwrap` an den festen Pfaden ([`BWRAP_CANDIDATES`]). Ist dort
    /// keines vertrauenswürdig vorhanden, wird trotzdem der erste feste Pfad
    /// eingetragen: Der Start scheitert dann laut mit „not found“, statt über
    /// `PATH` ein beliebiges Programm zu finden.
    fn default() -> Self {
        Self::discover().unwrap_or_else(|_| Self::new(PathBuf::from(BWRAP_CANDIDATES[0])))
    }
}

impl BwrapLauncher {
    /// Verwendet genau `executable`. Der Aufrufer ist für die Vertrauenswürdigkeit
    /// des Pfads verantwortlich; [`spawn`](Self::spawn) lehnt relative Pfade ab.
    #[must_use]
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            tmpfs_size: None,
        }
    }

    /// Findet `bwrap` ausschließlich an [`BWRAP_CANDIDATES`] (nie über `PATH`).
    ///
    /// # Errors
    /// [`SandboxError::SandboxProcessSpawn`], wenn an keinem festen Pfad ein
    /// vertrauenswürdiges Binary liegt (reguläre Datei, ausführbar, Eigentümer
    /// root, nicht group-/world-beschreibbar).
    pub fn discover() -> SandboxResult<Self> {
        let candidates = BWRAP_CANDIDATES.map(Path::new);
        Self::find_pinned_executable(&candidates)
            .map(Self::new)
            .ok_or_else(|| SandboxError::SandboxProcessSpawn {
                executable: PathBuf::from(BWRAP_CANDIDATES[0]),
                reason: format!(
                    "no trusted bubblewrap at fixed paths {} (PATH is never searched)",
                    BWRAP_CANDIDATES.join(", ")
                ),
            })
    }

    /// Liefert den ersten vertrauenswürdigen Kandidaten. Auch für Hilfsprogramme
    /// nutzbar, die vor `bwrap` gestartet werden (z. B. `prlimit`), damit es nur
    /// eine Vertrauensregel gibt. Relative Kandidaten werden immer übersprungen.
    #[must_use]
    pub fn find_pinned_executable(candidates: &[&Path]) -> Option<PathBuf> {
        candidates
            .iter()
            .find(|candidate| check_pinned_executable(candidate).is_ok())
            .map(|candidate| candidate.to_path_buf())
    }

    /// Pfad des Bubblewrap-Binaries, das [`spawn`](Self::spawn) startet und das
    /// asynchrone Aufrufer (harw-tool-shell) selbst starten müssen.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Begrenzt das tmpfs unter `/tmp` auf `bytes` (bwrap `--size`).
    #[must_use]
    pub fn with_tmpfs_size(mut self, bytes: NonZeroU64) -> Self {
        self.tmpfs_size = Some(bytes);
        self
    }

    /// Konfigurierte tmpfs-Obergrenze, falls gesetzt.
    #[must_use]
    pub fn tmpfs_size(&self) -> Option<NonZeroU64> {
        self.tmpfs_size
    }

    /// Produces a hermetic command plan. `command` must include the program as
    /// its first item and is only allowed when the sandbox granted process
    /// execution. Network stays unshared unless specifically authorized.
    pub fn plan(
        &self,
        sandbox: &SandboxSpec,
        command: &[OsString],
    ) -> SandboxResult<BwrapCommandPlan> {
        if !sandbox.permissions().contains(Permission::ExecuteProcess) {
            return Err(SandboxError::ProcessExecutionDenied);
        }
        if command.is_empty() {
            return Err(SandboxError::MissingSandboxCommand);
        }
        let workspace = sandbox.workspace().canonical_root();
        let mut args = vec![
            OsString::from("--die-with-parent"),
            OsString::from("--new-session"),
            OsString::from("--unshare-all"),
        ];
        if sandbox.permissions().contains(Permission::NetworkAccess) {
            args.push(OsString::from("--share-net"));
        }
        args.extend([
            OsString::from("--clearenv"),
            OsString::from("--proc"),
            OsString::from("/proc"),
        ]);
        args.extend([OsString::from("--dev"), OsString::from("/dev")]);
        // `--size` wirkt nur auf die unmittelbar folgende `--tmpfs`-Aktion
        // (bwrap(1): "must be followed by --tmpfs"), muss also direkt davor stehen.
        if let Some(size) = self.tmpfs_size {
            args.extend([OsString::from("--size"), OsString::from(size.to_string())]);
        }
        args.extend([OsString::from("--tmpfs"), OsString::from("/tmp")]);
        args.extend([OsString::from("--dir"), OsString::from("/tmp/home")]);
        args.extend([
            OsString::from("--setenv"),
            OsString::from("HOME"),
            OsString::from("/tmp/home"),
            OsString::from("--setenv"),
            OsString::from("PATH"),
            OsString::from("/usr/local/bin:/usr/bin:/bin"),
        ]);

        for directory in ["/usr", "/bin", "/lib", "/lib64"] {
            let path = Path::new(directory);
            if path.exists() {
                args.extend([
                    OsString::from("--ro-bind"),
                    path.as_os_str().to_owned(),
                    path.as_os_str().to_owned(),
                ]);
            }
        }
        append_destination_dirs(&mut args, workspace)?;
        args.push(
            if sandbox.permissions().contains(Permission::WriteWorkspace) {
                OsString::from("--bind")
            } else {
                OsString::from("--ro-bind")
            },
        );
        args.push(workspace.as_os_str().to_owned());
        args.push(workspace.as_os_str().to_owned());
        args.extend([OsString::from("--chdir"), workspace.as_os_str().to_owned()]);
        args.push(OsString::from("--"));
        args.extend(command.iter().cloned());
        Ok(BwrapCommandPlan { args })
    }

    /// Executes a previously checked command through Bubblewrap. The plan is
    /// deliberately built first so callers can audit/log its non-secret mount
    /// shape before process creation.
    ///
    /// Ein relativer `executable` wird abgelehnt, weil `Command::new` ihn sonst
    /// über `PATH` auflösen würde.
    pub fn spawn(&self, plan: &BwrapCommandPlan) -> SandboxResult<Child> {
        if !self.executable.is_absolute() {
            return Err(SandboxError::SandboxProcessSpawn {
                executable: self.executable.clone(),
                reason: "sandbox executable must be an absolute path".to_owned(),
            });
        }
        Command::new(&self.executable)
            .args(&plan.args)
            .spawn()
            .map_err(|error| SandboxError::SandboxProcessSpawn {
                executable: self.executable.clone(),
                reason: error.to_string(),
            })
    }
}

/// Prüft einen festen Kandidatenpfad: absolut, reguläre Datei (Symlinks werden
/// aufgelöst, z. B. `/bin -> usr/bin`), mindestens ein x-Bit, Eigentümer root
/// und weder group- noch world-beschreibbar. Liefert den Ablehnungsgrund.
fn check_pinned_executable(candidate: &Path) -> Result<(), &'static str> {
    if !candidate.is_absolute() {
        return Err("candidate path is not absolute");
    }
    let metadata = std::fs::metadata(candidate).map_err(|_| "candidate does not exist")?;
    if !metadata.is_file() {
        return Err("candidate is not a regular file");
    }
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err("candidate is not executable");
    }
    if metadata.uid() != 0 {
        return Err("candidate is not owned by root");
    }
    if mode & 0o022 != 0 {
        return Err("candidate is group- or world-writable");
    }
    Ok(())
}

fn append_destination_dirs(args: &mut Vec<OsString>, destination: &Path) -> SandboxResult<()> {
    if !destination.is_absolute() {
        return Err(SandboxError::InvalidSandboxWorkspaceDestination {
            path: destination.to_path_buf(),
        });
    }
    let mut current = PathBuf::new();
    for component in destination.components() {
        match component {
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(part) => {
                current.push(part);
                args.extend([OsString::from("--dir"), current.as_os_str().to_owned()]);
            }
            _ => {
                return Err(SandboxError::InvalidSandboxWorkspaceDestination {
                    path: destination.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    fn sandbox(permissions: PermissionSet) -> SandboxSpec {
        let root = std::env::temp_dir().join(format!("harwness-bwrap-{}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .unwrap();
        SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("tenant"),
                    &WorkspaceId::from_str("workspace"),
                )
                .unwrap(),
            permissions,
        )
    }

    fn strings(plan: &BwrapCommandPlan) -> Vec<String> {
        plan.args()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn readonly_plan_has_no_network_or_workspace_write_mount() {
        let plan = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([
                    Permission::ReadWorkspace,
                    Permission::ExecuteProcess,
                ])),
                &[OsString::from("/bin/true")],
            )
            .unwrap();
        let args = strings(&plan);
        assert!(args.contains(&"--unshare-all".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--ro-bind".to_owned()));
    }

    #[test]
    fn writable_networked_plan_requires_each_explicit_permission() {
        let plan = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([
                    Permission::ReadWorkspace,
                    Permission::WriteWorkspace,
                    Permission::ExecuteProcess,
                    Permission::NetworkAccess,
                ])),
                &[OsString::from("/bin/true")],
            )
            .unwrap();
        let args = strings(&plan);
        assert!(args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--bind".to_owned()));
    }

    #[test]
    fn process_start_is_denied_without_execute_permission() {
        let error = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([Permission::ReadWorkspace])),
                &[OsString::from("/bin/true")],
            )
            .unwrap_err();
        assert!(matches!(error, SandboxError::ProcessExecutionDenied));
    }

    fn execute_sandbox() -> SandboxSpec {
        sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]))
    }

    #[test]
    fn tmpfs_size_directly_precedes_tmpfs_action() {
        let size = NonZeroU64::new(268_435_456).unwrap();
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap")).with_tmpfs_size(size);
        assert_eq!(launcher.tmpfs_size(), Some(size));
        let args = strings(
            &launcher
                .plan(&execute_sandbox(), &[OsString::from("/bin/true")])
                .unwrap(),
        );
        let tmpfs = args.iter().position(|argument| argument == "--tmpfs").unwrap();
        // bwrap(1): `--size` gilt nur für die unmittelbar folgende `--tmpfs`-Aktion.
        assert_eq!(args[tmpfs - 2], "--size");
        assert_eq!(args[tmpfs - 1], "268435456");
        assert_eq!(args[tmpfs + 1], "/tmp");
        assert_eq!(args.iter().filter(|argument| *argument == "--size").count(), 1);
    }

    #[test]
    fn plan_without_tmpfs_size_has_no_size_option() {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(launcher.tmpfs_size(), None);
        let args = strings(
            &launcher
                .plan(&execute_sandbox(), &[OsString::from("/bin/true")])
                .unwrap(),
        );
        assert!(!args.contains(&"--size".to_owned()));
    }

    #[test]
    fn default_executable_is_always_a_fixed_absolute_path() {
        let launcher = BwrapLauncher::default();
        assert!(launcher.executable().is_absolute());
        assert!(
            BWRAP_CANDIDATES
                .iter()
                .any(|candidate| Path::new(candidate) == launcher.executable()),
            "default must never fall back to a PATH lookup: {:?}",
            launcher.executable()
        );
    }

    #[test]
    fn discover_returns_only_trusted_fixed_candidates() {
        match BwrapLauncher::discover() {
            Ok(launcher) => {
                assert!(BWRAP_CANDIDATES.contains(&launcher.executable().to_str().unwrap()));
                assert!(check_pinned_executable(launcher.executable()).is_ok());
            }
            Err(error) => {
                assert!(matches!(error, SandboxError::SandboxProcessSpawn { .. }));
                assert!(
                    BWRAP_CANDIDATES
                        .iter()
                        .all(|candidate| check_pinned_executable(Path::new(candidate)).is_err())
                );
            }
        }
    }

    #[test]
    fn relative_candidates_are_never_resolved_via_path() {
        // `sh` und `bwrap` liegen typischerweise im PATH; relative Kandidaten
        // dürfen trotzdem nie gefunden werden.
        let candidates = [Path::new("bwrap"), Path::new("./bwrap"), Path::new("sh")];
        assert_eq!(BwrapLauncher::find_pinned_executable(&candidates), None);
        assert_eq!(
            check_pinned_executable(Path::new("bwrap")),
            Err("candidate path is not absolute")
        );
    }

    #[test]
    fn directories_and_missing_candidates_are_rejected() {
        assert_eq!(
            check_pinned_executable(Path::new("/")),
            Err("candidate is not a regular file")
        );
        let missing = std::env::temp_dir().join(format!(
            "harwness-bwrap-missing-{}/bwrap",
            std::process::id()
        ));
        assert_eq!(
            check_pinned_executable(&missing),
            Err("candidate does not exist")
        );
    }

    #[test]
    fn user_owned_executable_is_not_trusted() {
        let directory =
            std::env::temp_dir().join(format!("harwness-bwrap-fake-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let fake = directory.join("bwrap");
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        if std::fs::metadata(&fake).unwrap().uid() == 0 {
            eprintln!("übersprungen: Test läuft als root, Eigentümerprüfung nicht beobachtbar");
            return;
        }
        assert_eq!(
            check_pinned_executable(&fake),
            Err("candidate is not owned by root")
        );
        assert_eq!(
            BwrapLauncher::find_pinned_executable(&[fake.as_path()]),
            None
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn first_trusted_candidate_wins_over_missing_ones() {
        let shell = Path::new("/bin/sh");
        if check_pinned_executable(shell).is_err() {
            eprintln!("übersprungen: /bin/sh ist hier kein root-eigenes, geschütztes Binary");
            return;
        }
        let missing = std::env::temp_dir().join(format!(
            "harwness-bwrap-none-{}/bwrap",
            std::process::id()
        ));
        assert_eq!(
            BwrapLauncher::find_pinned_executable(&[missing.as_path(), shell]),
            Some(PathBuf::from("/bin/sh"))
        );
    }

    #[test]
    fn spawn_rejects_relative_executable() {
        let plan = BwrapLauncher::default()
            .plan(&execute_sandbox(), &[OsString::from("/bin/true")])
            .unwrap();
        let error = BwrapLauncher::new(PathBuf::from("bwrap"))
            .spawn(&plan)
            .unwrap_err();
        assert!(matches!(
            error,
            SandboxError::SandboxProcessSpawn { ref reason, .. } if reason.contains("absolute")
        ));
    }
}
