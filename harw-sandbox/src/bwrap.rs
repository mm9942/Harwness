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
//!
//! W5 N-SBX (F-003, F-120): Das Backend teilt **nie** den Host-Netz-Namespace.
//! Jeder Plan enthält `--unshare-all --unshare-net`; `--share-net` wird in keiner
//! Kombination erzeugt. Netz gibt es nur über [`NetworkMode::ProxyOnly`]: Das
//! Relay (`harw-netns-relay`, Exec-Modus) wird als `COMMAND` der Sandbox
//! gestartet, bindet `127.0.0.1:<port>` und startet danach den eigentlichen
//! Befehl als Kind in derselben netns. Gestartete Prozesse laufen in
//! [`SandboxChild`]: stdin `/dev/null`, stdout/stderr als Pipes, und beim Drop
//! wird der Prozess getötet und eingesammelt.
//!
//! # Concurrency
//! [`BwrapLauncher`] und [`BwrapCommandPlan`] sind reine Werte (`Send + Sync`).
//! [`SandboxChild`] besitzt genau einen Kindprozess; Drop blockiert bis zum
//! Einsammeln nach `SIGKILL`.
//!
//! # Errors
//! [`SandboxError::ProcessExecutionDenied`], [`SandboxError::MissingSandboxCommand`],
//! [`SandboxError::NetworkModeNotGranted`], [`SandboxError::InvalidRelaySpec`],
//! [`SandboxError::InvalidSandboxWorkspaceDestination`],
//! [`SandboxError::SandboxProcessSpawn`], [`SandboxError::Io`].

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};

use crate::{CargoExecutionMode, CargoSandboxProfile, NetworkMode, Permission, RelaySpec, SandboxError, SandboxResult, SandboxSpec};

/// Feste Suchpfade für Bubblewrap in Prioritätsreihenfolge. `PATH` wird nie
/// ausgewertet.
pub const BWRAP_CANDIDATES: [&str; 2] = ["/usr/bin/bwrap", "/bin/bwrap"];

/// Fester Pfad des Relay-Binaries *in* der Sandbox (nur lesend gebunden).
pub const SANDBOX_RELAY_PATH: &str = "/run/harw/netns-relay";

/// Fester Pfad des Egress-Proxy-Sockets *in* der Sandbox (read-write gebunden).
pub const SANDBOX_PROXY_SOCKET_PATH: &str = "/run/harw/egress.sock";

// Elternverzeichnis beider fester Pfade; liegt auf dem tmpfs-Root der Sandbox.
const SANDBOX_RUN_DIR: &str = "/run/harw";
/// Fester Cargo-Programmname und Toolchain-Wurzel im Sandkasten.
pub const SANDBOX_CARGO_PATH: &str = "/opt/harw/toolchain/bin/cargo";
pub const SANDBOX_RUSTUP_HOME: &str = "/opt/harw/rustup";
pub const SANDBOX_CARGO_HOME: &str = "/var/cache/harw/cargo";

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
    /// Netzmodus; Default [`NetworkMode::None`]. Nie Host-netns.
    network_mode: NetworkMode,
    cargo_profile: Option<CargoSandboxProfile>,
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
            network_mode: NetworkMode::None,
            cargo_profile: None,
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

    /// Setzt den Netzmodus der geplanten Sandbox (W5 N-SBX).
    ///
    /// # Description
    /// [`NetworkMode::None`] (Default) ergibt eine netns ohne Außenverbindung.
    /// [`NetworkMode::ProxyOnly`] startet das Relay als Sandbox-Befehl und setzt
    /// `ALL_PROXY`; [`plan`](Self::plan) verlangt dafür
    /// [`Permission::NetworkAccess`]. Kein Modus teilt den Host-netns.
    ///
    /// # Arguments
    /// - `mode` (`NetworkMode`): gewünschter Modus, wird übernommen.
    ///
    /// # Returns
    /// Den geänderten Launcher (`Self`).
    ///
    /// # Concurrency
    /// Reiner Builder ohne Seiteneffekte.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::PathBuf;
    /// use harw_sandbox::{BwrapLauncher, NetworkMode};
    ///
    /// let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
    ///     .with_network_mode(NetworkMode::None);
    /// assert_eq!(launcher.network_mode(), &NetworkMode::None);
    /// ```
    #[must_use]
    pub fn with_network_mode(mut self, mode: NetworkMode) -> Self {
        self.network_mode = mode;
        self
    }

    /// Konfigurierter Netzmodus (Default [`NetworkMode::None`]).
    #[must_use]
    pub fn network_mode(&self) -> &NetworkMode {
        &self.network_mode
    }

    /// Bindet eine zuvor validierte Rust-Toolchain an diesen Launcher. Das Profil
    /// kann nur beim Aufbau des Launchers gesetzt werden; Tool-Aufrufe können es
    /// nicht liefern oder auf einen beliebigen Host-Pfad umbiegen.
    #[must_use]
    pub fn with_cargo_profile(mut self, profile: CargoSandboxProfile) -> Self {
        self.cargo_profile = Some(profile);
        self
    }

    /// Das konfigurierte Cargo-Profil, falls die Sandbox Rust-Builds anbietet.
    #[must_use]
    pub fn cargo_profile(&self) -> Option<&CargoSandboxProfile> {
        self.cargo_profile.as_ref()
    }

    /// Produces a hermetic command plan. `command` must include the program as
    /// its first item and is only allowed when the sandbox granted process
    /// execution. The host network namespace is never shared: every plan
    /// carries `--unshare-net`; [`NetworkMode::ProxyOnly`] additionally binds
    /// the relay and proxy socket and wraps `command` in the relay's exec mode.
    ///
    /// # Errors
    /// - [`SandboxError::ProcessExecutionDenied`]: `ExecuteProcess` fehlt.
    /// - [`SandboxError::MissingSandboxCommand`]: `command` ist leer.
    /// - [`SandboxError::NetworkModeNotGranted`]: `ProxyOnly` ohne `NetworkAccess`.
    /// - [`SandboxError::InvalidRelaySpec`]: Relay-Pfad/-Socket nicht absolut
    ///   bzw. nicht normal, oder Port `0`.
    /// - [`SandboxError::InvalidSandboxWorkspaceDestination`]: Workspace-Pfad ungültig.
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
        let relay = match &self.network_mode {
            NetworkMode::None => None,
            NetworkMode::ProxyOnly(spec) => {
                if !sandbox.permissions().contains(Permission::NetworkAccess) {
                    return Err(SandboxError::NetworkModeNotGranted);
                }
                validate_relay_spec(spec)?;
                Some(spec)
            }
        };
        let workspace = sandbox.workspace().canonical_root();
        // F-003/F-120: `--unshare-net` steht immer explizit im Plan (auch wenn
        // `--unshare-all` es bereits impliziert); `--share-net` gibt es nicht.
        let mut args = vec![
            OsString::from("--die-with-parent"),
            OsString::from("--new-session"),
            OsString::from("--unshare-all"),
            OsString::from("--unshare-net"),
        ];
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
        if let Some(spec) = relay {
            args.extend([
                OsString::from("--setenv"),
                OsString::from("ALL_PROXY"),
                OsString::from(format!("socks5h://127.0.0.1:{}", spec.listen_port)),
            ]);
        }
        if let Some(profile) = &self.cargo_profile {
            let sandbox_cargo_dir = Path::new(SANDBOX_CARGO_PATH)
                .parent()
                .expect("fixed sandbox cargo path has a parent");
            if matches!(profile.mode(), CargoExecutionMode::Fetch)
                && (relay.is_none() || sandbox.network_scope().is_empty())
            {
                return Err(SandboxError::CargoFetchNetworkDenied);
            }
            args.extend([
                OsString::from("--setenv"), OsString::from("RUSTUP_HOME"), OsString::from(SANDBOX_RUSTUP_HOME),
                OsString::from("--setenv"), OsString::from("CARGO_HOME"), OsString::from(SANDBOX_CARGO_HOME),
                OsString::from("--setenv"), OsString::from("PATH"), OsString::from(format!("{}:/usr/local/bin:/usr/bin:/bin", sandbox_cargo_dir.display())),
            ]);
            if profile.mode().offline() {
                args.extend([OsString::from("--setenv"), OsString::from("CARGO_NET_OFFLINE"), OsString::from("true")]);
            }
        }

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
        if let Some(profile) = &self.cargo_profile {
            let cargo_dir = profile.cargo_bin().parent().expect("canonical executable has a parent");
            let sandbox_cargo_dir = Path::new(SANDBOX_CARGO_PATH)
                .parent()
                .expect("fixed sandbox cargo path has a parent");
            append_destination_dirs(&mut args, sandbox_cargo_dir)?;
            args.extend([
                OsString::from("--ro-bind"),
                cargo_dir.as_os_str().to_owned(),
                sandbox_cargo_dir.as_os_str().to_owned(),
            ]);
            append_destination_dirs(&mut args, Path::new(SANDBOX_RUSTUP_HOME))?;
            args.extend([OsString::from("--ro-bind"), profile.rustup_home().as_os_str().to_owned(), OsString::from(SANDBOX_RUSTUP_HOME)]);
            append_destination_dirs(&mut args, Path::new(SANDBOX_CARGO_HOME))?;
            args.push(if profile.mode().cache_writable() { OsString::from("--bind") } else { OsString::from("--ro-bind") });
            args.extend([profile.cargo_home().as_os_str().to_owned(), OsString::from(SANDBOX_CARGO_HOME)]);
        }
        append_destination_dirs(&mut args, workspace)?;
        let write_allowed = sandbox.permissions().contains(Permission::WriteWorkspace);
        args.push(if write_allowed {
            OsString::from("--bind")
        } else {
            OsString::from("--ro-bind")
        });
        args.push(workspace.as_os_str().to_owned());
        args.push(workspace.as_os_str().to_owned());
        // Extra-Roots (`/add-workdir`, Slice A8): dieselben Rechte wie die
        // primäre Workspace-Bindung, nach ihr und dedupliziert gegen sie und
        // untereinander — eine bereits gebundene (oder darin enthaltene)
        // Wurzel bekommt keinen zweiten, redundanten Bind.
        let mut bound_roots: Vec<PathBuf> = vec![workspace.to_path_buf()];
        for extra in sandbox.extra_roots().snapshot() {
            if bound_roots
                .iter()
                .any(|bound| extra.path == *bound || extra.path.starts_with(bound))
            {
                continue;
            }
            append_destination_dirs(&mut args, &extra.path)?;
            args.push(if write_allowed {
                OsString::from("--bind")
            } else {
                OsString::from("--ro-bind")
            });
            args.push(extra.path.as_os_str().to_owned());
            args.push(extra.path.as_os_str().to_owned());
            bound_roots.push(extra.path);
        }
        // Relay-Bindungen nach dem Workspace, damit eine Workspace-Bindung sie
        // nicht überdecken kann. Ziele liegen auf dem tmpfs-Root der Sandbox.
        if let Some(spec) = relay {
            append_destination_dirs(&mut args, Path::new(SANDBOX_RUN_DIR))?;
            args.extend([
                OsString::from("--ro-bind"),
                spec.binary.as_os_str().to_owned(),
                OsString::from(SANDBOX_RELAY_PATH),
                OsString::from("--bind"),
                spec.proxy_socket.as_os_str().to_owned(),
                OsString::from(SANDBOX_PROXY_SOCKET_PATH),
            ]);
        }
        args.extend([OsString::from("--chdir"), workspace.as_os_str().to_owned()]);
        args.push(OsString::from("--"));
        if let Some(spec) = relay {
            // Exec-Modus N-EGRESS: `<relay> <port> <socket> -- <cmd…>`.
            args.extend([
                OsString::from(SANDBOX_RELAY_PATH),
                OsString::from(spec.listen_port.to_string()),
                OsString::from(SANDBOX_PROXY_SOCKET_PATH),
                OsString::from("--"),
            ]);
        }
        args.extend(command.iter().cloned());
        Ok(BwrapCommandPlan { args })
    }

    /// Executes a previously checked command through Bubblewrap. The plan is
    /// deliberately built first so callers can audit/log its non-secret mount
    /// shape before process creation.
    ///
    /// Ein relativer `executable` wird abgelehnt, weil `Command::new` ihn sonst
    /// über `PATH` auflösen würde.
    ///
    /// F-120: stdin ist `/dev/null`, stdout/stderr sind Pipes; weitere fds erbt
    /// das Kind nicht (std öffnet eigene fds mit `O_CLOEXEC`). Der Prozess steckt
    /// in einem [`SandboxChild`], der ihn beim Drop tötet und einsammelt.
    ///
    /// # Errors
    /// [`SandboxError::SandboxProcessSpawn`] bei relativem Pfad oder
    /// fehlgeschlagenem Start.
    pub fn spawn(&self, plan: &BwrapCommandPlan) -> SandboxResult<SandboxChild> {
        if !self.executable.is_absolute() {
            return Err(SandboxError::SandboxProcessSpawn {
                executable: self.executable.to_path_buf(),
                reason: "sandbox executable must be an absolute path".to_owned(),
            });
        }
        Command::new(&self.executable)
            .args(&plan.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map(|child| SandboxChild::new(child, &self.executable))
            .map_err(|error| SandboxError::SandboxProcessSpawn {
                executable: self.executable.to_path_buf(),
                reason: error.to_string(),
            })
    }
}

/// Kill-on-drop-Hülle um einen gestarteten Sandbox-Prozess (F-120).
///
/// # Description
/// Besitzt genau einen [`std::process::Child`]. Wird die Hülle verworfen,
/// ohne dass [`wait`](Self::wait) den Exit-Status erfolgreich eingesammelt
/// hat, sendet `Drop` `SIGKILL` (`Child::kill`) und sammelt den Prozess mit
/// `Child::wait` ein — kein Zombie, kein weiterlaufender Sandbox-Baum
/// (`--die-with-parent` + PID-Namespace beenden die Nachkommen). Fehler beim
/// Töten/Einsammeln werden per `tracing::warn!` protokolliert; `Drop` panict nie.
/// Ein `into_inner` gibt es bewusst nicht, weil es die Garantie aufheben würde.
///
/// # Concurrency
/// `Send`, nicht geteilt; `Drop` und [`wait`](Self::wait) blockieren bis zum
/// Prozessende.
///
/// # Examples
/// ```rust,no_run
/// # fn demo(launcher: &harw_sandbox::BwrapLauncher, plan: &harw_sandbox::BwrapCommandPlan)
/// # -> Result<(), harw_sandbox::SandboxError> {
/// let mut child = launcher.spawn(plan)?;
/// let _stdout = child.take_stdout();
/// let status = child.wait()?;
/// println!("sandbox exited: {status}");
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct SandboxChild {
    child: Child,
    executable: PathBuf,
    // `true`, sobald `wait` den Exit-Status eingesammelt hat.
    reaped: bool,
}

impl SandboxChild {
    // Einziger Konstruktor; hält den Pfad für Fehlerkontext fest.
    fn new(child: Child, executable: &Path) -> Self {
        Self {
            child,
            executable: executable.to_path_buf(),
            reaped: false,
        }
    }

    /// Prozess-ID des gestarteten `bwrap`-Prozesses (Host-PID-Namespace).
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Entnimmt die stdout-Pipe (einmalig; danach `None`).
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Entnimmt die stderr-Pipe (einmalig; danach `None`).
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Wartet auf das Prozessende und sammelt den Exit-Status ein.
    ///
    /// # Description
    /// Nach Erfolg tötet `Drop` nicht mehr. Schlägt das Warten fehl, bleibt der
    /// Kill-on-drop-Schutz aktiv. Wiederholte Aufrufe liefern denselben Status.
    ///
    /// # Returns
    /// `ExitStatus` des `bwrap`-Prozesses.
    ///
    /// # Errors
    /// [`SandboxError::Io`] mit dem Executable-Pfad, wenn `waitpid` scheitert.
    ///
    /// # Concurrency
    /// Blockiert den aufrufenden Thread bis zum Prozessende.
    pub fn wait(&mut self) -> SandboxResult<ExitStatus> {
        let status = self.child.wait().map_err(|error| SandboxError::Io {
            path: self.executable.to_path_buf(),
            reason: format!("waiting for sandbox process failed: {error}"),
        })?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for SandboxChild {
    /// Tötet einen nicht eingesammelten Prozess und sammelt ihn ein; loggt
    /// Fehler, panict nie.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let pid = self.child.id();
        if let Err(error) = self.child.kill() {
            tracing::warn!(pid, error = %error, "sandbox child kill on drop failed");
        }
        match self.child.wait() {
            Ok(status) => {
                tracing::debug!(pid, status = %status, "sandbox child reaped on drop");
            }
            Err(error) => {
                tracing::warn!(pid, error = %error, "sandbox child reap on drop failed");
            }
        }
    }
}

/// Prüft die Invarianten von [`RelaySpec`] vor dem Planen.
fn validate_relay_spec(spec: &RelaySpec) -> SandboxResult<()> {
    if spec.listen_port == 0 {
        return Err(SandboxError::InvalidRelaySpec {
            field: "listen_port",
            reason: "port 0 is not a fixed listen port".to_owned(),
        });
    }
    for (field, path) in [
        ("binary", spec.binary.as_path()),
        ("proxy_socket", spec.proxy_socket.as_path()),
    ] {
        if !is_absolute_normal(path) {
            return Err(SandboxError::InvalidRelaySpec {
                field,
                reason: format!(
                    "path must be absolute without '.' or '..' components: '{}'",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

// Absolut, mindestens eine normale Komponente, nur RootDir/Normal-Komponenten.
fn is_absolute_normal(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
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
    use crate::{ExtraRootsCell, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
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
        // W5 N-SBX: `NetworkAccess` teilt den Host-netns nicht mehr.
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
        assert!(args.contains(&"--unshare-net".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--bind".to_owned()));
    }

    fn relay_spec() -> RelaySpec {
        RelaySpec {
            binary: PathBuf::from("/opt/harw/bin/harw-netns-relay"),
            listen_port: 18_080,
            proxy_socket: PathBuf::from("/run/user/1000/harw/egress.sock"),
        }
    }

    fn networked_sandbox() -> SandboxSpec {
        sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
        ]))
    }

    // Anzahl der Stellen, an denen `window` als zusammenhängende Folge vorkommt.
    fn count_window(args: &[String], window: &[&str]) -> usize {
        args.windows(window.len())
            .filter(|candidate| candidate.iter().zip(window).all(|(a, b)| a == b))
            .count()
    }

    #[test]
    fn plan_binds_every_extra_root_with_workspace_rights_and_dedupes() {
        let base = sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]));
        let primary = base.workspace().canonical_root().to_path_buf();
        let base_dir = primary.parent().unwrap().to_path_buf();

        let extra_a = base_dir.join("extra-a");
        let extra_b = base_dir.join("extra-b");
        std::fs::create_dir_all(&extra_a).unwrap();
        std::fs::create_dir_all(&extra_b).unwrap();

        let extra_roots = ExtraRootsCell::new();
        extra_roots.add(&extra_a, false, &primary, None).unwrap();
        extra_roots.add(&extra_b, false, &primary, None).unwrap();
        // Ein Duplikat derselben Wurzel darf keinen zweiten Bind erzeugen.
        assert!(!extra_roots.add(&extra_a, false, &primary, None).unwrap());

        let spec = base.with_extra_roots(extra_roots);
        let plan = BwrapLauncher::default()
            .plan(&spec, &[OsString::from("/bin/true")])
            .unwrap();
        let args = strings(&plan);

        let extra_a_canonical = extra_a.canonicalize().unwrap();
        let extra_b_canonical = extra_b.canonicalize().unwrap();
        let extra_a_str = extra_a_canonical.to_str().unwrap();
        let extra_b_str = extra_b_canonical.to_str().unwrap();

        assert_eq!(count_window(&args, &["--bind", extra_a_str, extra_a_str]), 1);
        assert_eq!(count_window(&args, &["--bind", extra_b_str, extra_b_str]), 1);
    }

    #[test]
    fn plan_without_extra_roots_binds_only_the_primary_workspace() {
        let base = sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]));
        let primary = base.workspace().canonical_root().to_path_buf();

        let plan = BwrapLauncher::default()
            .plan(&base, &[OsString::from("/bin/true")])
            .unwrap();
        let args = strings(&plan);

        assert_eq!(
            count_window(
                &args,
                &[
                    "--ro-bind",
                    primary.to_str().unwrap(),
                    primary.to_str().unwrap()
                ]
            ),
            1
        );
    }

    #[test]
    fn test_with_network_mode_default_is_none() {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(launcher.network_mode(), &NetworkMode::None);
        let proxied = launcher.with_network_mode(NetworkMode::ProxyOnly(relay_spec()));
        assert_eq!(
            proxied.network_mode(),
            &NetworkMode::ProxyOnly(relay_spec())
        );
    }

    #[test]
    fn test_plan_network_none_unshares_net_without_proxy() {
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .plan(&networked_sandbox(), &[OsString::from("/bin/true")])
                .unwrap(),
        );
        assert_eq!(args.iter().filter(|a| *a == "--unshare-net").count(), 1);
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(!args.contains(&"ALL_PROXY".to_owned()));
        assert!(!args.contains(&SANDBOX_RELAY_PATH.to_owned()));
        assert!(!args.contains(&SANDBOX_PROXY_SOCKET_PATH.to_owned()));
        assert!(args.contains(&"--die-with-parent".to_owned()));
    }

    #[test]
    fn test_plan_proxy_only_binds_exactly_socket_and_sets_all_proxy() {
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_network_mode(NetworkMode::ProxyOnly(relay_spec()))
                .plan(
                    &networked_sandbox(),
                    &[OsString::from("/bin/echo"), OsString::from("hi")],
                )
                .unwrap(),
        );
        assert!(args.contains(&"--die-with-parent".to_owned()));
        assert!(args.contains(&"--unshare-net".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert_eq!(
            count_window(
                &args,
                &[
                    "--bind",
                    "/run/user/1000/harw/egress.sock",
                    SANDBOX_PROXY_SOCKET_PATH
                ]
            ),
            1
        );
        // Kein weiteres Vorkommen des Host-Socket-Pfads (genau eine Bindung).
        assert_eq!(
            args.iter()
                .filter(|a| *a == "/run/user/1000/harw/egress.sock")
                .count(),
            1
        );
        assert_eq!(
            count_window(
                &args,
                &[
                    "--ro-bind",
                    "/opt/harw/bin/harw-netns-relay",
                    SANDBOX_RELAY_PATH
                ]
            ),
            1
        );
        assert_eq!(
            count_window(
                &args,
                &["--setenv", "ALL_PROXY", "socks5h://127.0.0.1:18080"]
            ),
            1
        );
        let separator = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &args[separator + 1..],
            &[
                SANDBOX_RELAY_PATH,
                "18080",
                SANDBOX_PROXY_SOCKET_PATH,
                "--",
                "/bin/echo",
                "hi"
            ]
        );
        // ALL_PROXY muss nach `--clearenv` gesetzt werden, sonst wäre es gelöscht.
        let clearenv = args.iter().position(|a| a == "--clearenv").unwrap();
        let all_proxy = args.iter().position(|a| a == "ALL_PROXY").unwrap();
        assert!(clearenv < all_proxy && all_proxy < separator);
    }

    #[test]
    fn test_plan_proxy_only_without_network_access_is_denied() {
        let error = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
            .with_network_mode(NetworkMode::ProxyOnly(relay_spec()))
            .plan(&execute_sandbox(), &[OsString::from("/bin/true")])
            .unwrap_err();
        assert!(matches!(error, SandboxError::NetworkModeNotGranted));
    }

    #[test]
    fn test_plan_proxy_only_rejects_invalid_relay_spec() {
        let cases = [
            (
                RelaySpec {
                    listen_port: 0,
                    ..relay_spec()
                },
                "listen_port",
            ),
            (
                RelaySpec {
                    binary: PathBuf::from("harw-netns-relay"),
                    ..relay_spec()
                },
                "binary",
            ),
            (
                RelaySpec {
                    binary: PathBuf::from("/opt/harw/../bin/relay"),
                    ..relay_spec()
                },
                "binary",
            ),
            (
                RelaySpec {
                    proxy_socket: PathBuf::from("egress.sock"),
                    ..relay_spec()
                },
                "proxy_socket",
            ),
            (
                RelaySpec {
                    proxy_socket: PathBuf::from("/"),
                    ..relay_spec()
                },
                "proxy_socket",
            ),
        ];
        for (spec, expected) in cases {
            let error = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_network_mode(NetworkMode::ProxyOnly(spec))
                .plan(&networked_sandbox(), &[OsString::from("/bin/true")])
                .unwrap_err();
            assert!(
                matches!(error, SandboxError::InvalidRelaySpec { field, .. } if field == expected),
                "expected InvalidRelaySpec({expected}), got {error:?}"
            );
        }
    }

    #[test]
    fn test_plan_never_shares_net_in_any_combination() {
        let optional = [
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ];
        let modes = [NetworkMode::None, NetworkMode::ProxyOnly(relay_spec())];
        let sizes = [None, NonZeroU64::new(1 << 20)];
        let mut planned = 0_usize;
        for mask in 0_u32..(1 << optional.len()) {
            for execute in [false, true] {
                let mut granted: Vec<Permission> = optional
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & (1 << bit) != 0)
                    .map(|(_, permission)| *permission)
                    .collect();
                if execute {
                    granted.push(Permission::ExecuteProcess);
                }
                let spec = sandbox(PermissionSet::from_policy(granted));
                for mode in &modes {
                    for size in sizes {
                        let mut launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                            .with_network_mode(mode.clone());
                        if let Some(size) = size {
                            launcher = launcher.with_tmpfs_size(size);
                        }
                        let Ok(plan) = launcher.plan(&spec, &[OsString::from("/bin/true")]) else {
                            continue;
                        };
                        planned += 1;
                        let args = strings(&plan);
                        assert!(!args.contains(&"--share-net".to_owned()), "{args:?}");
                        assert!(args.contains(&"--unshare-net".to_owned()), "{args:?}");
                        assert_eq!(
                            args.contains(&"ALL_PROXY".to_owned()),
                            matches!(mode, NetworkMode::ProxyOnly(_)),
                            "{args:?}"
                        );
                    }
                }
            }
        }
        // 64 Masken mit ExecuteProcess × 2 Größen × None + 32 mit NetworkAccess × 2 × ProxyOnly.
        assert_eq!(planned, 64 * 2 + 32 * 2);
    }

    #[test]
    #[ignore = "startet /bin/sleep als Host-Prozess; Laufzeit-Erkennung im Test"]
    fn test_sandbox_child_drop_kills_and_reaps_process() {
        let sleep = Path::new("/bin/sleep");
        if !sleep.is_file() {
            eprintln!("übersprungen: /bin/sleep fehlt");
            return;
        }
        let child = Command::new(sleep)
            .arg("300")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let guard = SandboxChild::new(child, sleep);
        let proc_entry = PathBuf::from(format!("/proc/{}", guard.id()));
        assert!(proc_entry.exists());
        let started = std::time::Instant::now();
        drop(guard);
        // Nach kill + wait ist der Prozess eingesammelt: kein /proc-Eintrag mehr.
        assert!(!proc_entry.exists());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    #[ignore = "startet /bin/echo als Host-Prozess; Laufzeit-Erkennung im Test"]
    fn test_spawn_pipes_output_and_wait_collects_status() {
        use std::io::Read;
        let echo = Path::new("/bin/echo");
        if !echo.is_file() {
            eprintln!("übersprungen: /bin/echo fehlt");
            return;
        }
        // `/bin/echo` statt bwrap: druckt den Argumentvektor des Plans.
        let launcher = BwrapLauncher::new(echo.to_path_buf());
        let plan = launcher
            .plan(&execute_sandbox(), &[OsString::from("/bin/true")])
            .unwrap();
        let mut child = launcher.spawn(&plan).unwrap();
        let mut stdout = child.take_stdout().unwrap();
        assert!(child.take_stdout().is_none());
        assert!(child.take_stderr().is_some());
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        assert!(output.contains("--unshare-net"));
        assert!(child.wait().unwrap().success());
        // Wiederholtes Warten liefert denselben Status; Drop tötet nicht mehr.
        assert!(child.wait().unwrap().success());
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
