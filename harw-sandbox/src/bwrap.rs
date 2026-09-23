//! Linux Bubblewrap launch backend.
//!
//! The core only carries a [`SandboxSpec`](harw_authority::SandboxSpec); this module is
//! the syscall-adjacent consumer that turns it into a minimal `bwrap` command.
//! By default it never mounts a host home, parent workspace, or arbitrary
//! environment; [`BwrapLauncher::with_host_path`] is the sole, explicit
//! opt-in that binds Host-`PATH` directories (Plan Teil C1).
//!
//! Jeder Plan bindet unabhängig von einem Profil `/etc/passwd`, `/etc/group`
//! und `/etc/nsswitch.conf` (`--ro-bind-try`) und setzt, wenn ermittelbar,
//! `--uid`/`--gid`/`--unshare-user` auf die echte Prozessidentität des
//! `harw`-Prozesses sowie `USER`/`LOGNAME`: `whoami`/`id` sollen in der
//! Sandbox denselben Nutzer zeigen wie außerhalb, statt mit einer uid ohne
//! `passwd`-Eintrag zu scheitern (siehe [`BwrapLauncher::with_identity`]).
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

use std::collections::HashSet;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::time::Duration;

use harw_authority::{Permission, SandboxSpec};
use harw_types::cancel::CancelToken;

use crate::{
    CargoExecutionMode, CargoSandboxProfile, NetworkMode, RelaySpec, SANDBOX_TMUX_SOCKET_PATH,
    SandboxError, SandboxProfile, SandboxResult, TmuxSandboxProfile,
};

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

/// Poll-Intervall für [`SandboxChild::wait_or_cancel`]: `std::process::Child`
/// kennt kein async-natives Warten, daher wird `try_wait` in dieser
/// Schrittweite wiederholt, geracet gegen `CancelToken::cancelled`.
const WAIT_OR_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Host-`PATH` (und optional Host-Cargo/-Rustup-Verzeichnisse), die
/// [`BwrapLauncher::with_host_path`] in den Plan übernimmt (Teil C1, Plan
/// `recursive-cooking-lobster.md`).
///
/// # Description
/// Reiner Werte-Typ ohne eigene Invarianten-Prüfung — [`BwrapLauncher::plan`]
/// entscheidet, welche Einträge tatsächlich gebunden werden (siehe dort).
/// `home`/`rustup_home`/`cargo_home` sind unabhängig von `path` optional:
/// fehlen sie, greifen die in `plan` dokumentierten Defaults bzw. entfällt
/// die jeweilige Zusatzbindung.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostPathBinding {
    /// Der vollständige, unveränderte Host-`PATH` (colon-separiert).
    pub path: String,
    /// Host-`HOME`, nur zum Ausschließen aus den Zusatzbindungen und als
    /// Default-Wurzel für `rustup_home`/`cargo_home` genutzt. Die Sandbox
    /// selbst bekommt weiterhin `/tmp/home` als `HOME`.
    pub home: Option<PathBuf>,
    /// Host-`RUSTUP_HOME`, falls explizit gesetzt (sonst `home/.rustup`).
    pub rustup_home: Option<PathBuf>,
    /// Host-`CARGO_HOME`, falls explizit gesetzt (sonst `home/.cargo`).
    pub cargo_home: Option<PathBuf>,
}

impl HostPathBinding {
    /// Liest `PATH`, `HOME`, `RUSTUP_HOME` und `CARGO_HOME` aus der eigenen
    /// Prozessumgebung.
    ///
    /// # Description
    /// `harw-tui`/`harw-tool-shell` lesen den zsh-`PATH` des Nutzers beim
    /// Start (Plan Teil C, Nutzerentscheidung 2026-09-21); dieser Konstruktor
    /// spiegelt genau das für den Sandbox-Planer.
    ///
    /// # Returns
    /// `None`, wenn `PATH` in der Umgebung fehlt oder leer ist — ohne
    /// Host-`PATH` gäbe es nichts zu binden. `home`/`rustup_home`/
    /// `cargo_home` sind unabhängig davon jeweils `None`, wenn die
    /// entsprechende Variable fehlt.
    ///
    /// # Concurrency
    /// Liest nur `std::env`; kein geteilter Zustand.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_sandbox::HostPathBinding;
    ///
    /// if let Some(binding) = HostPathBinding::from_env() {
    ///     assert!(!binding.path.is_empty());
    /// }
    /// ```
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let path = std::env::var("PATH").ok()?;
        if path.is_empty() {
            return None;
        }
        Some(Self {
            path,
            home: std::env::var_os("HOME").map(PathBuf::from),
            rustup_home: std::env::var_os("RUSTUP_HOME").map(PathBuf::from),
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
        })
    }
}

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
    /// Optionales tmux-Inspektionsprofil; bindet nur einen einzelnen,
    /// beim Aufbau validierten Socket unter
    /// [`SANDBOX_TMUX_SOCKET_PATH`] in die Sandbox.
    tmux_profile: Option<TmuxSandboxProfile>,
    /// Optionale Host-`PATH`-Bindung (Teil C1); ohne sie bleibt der Plan
    /// byte-identisch zum bisherigen Minimal-`PATH`.
    host_path: Option<HostPathBinding>,
    /// Überschreibt die uid/gid, die [`plan`](Self::plan) als `--uid`/`--gid`
    /// einträgt (Zusatzauftrag „echte Prozessidentität in der Sandbox“).
    /// Ohne [`Self::with_identity`] ermittelt `plan` die echte uid/gid des
    /// laufenden `harw`-Prozesses aus `/proc/self`.
    identity: Option<(u32, u32)>,
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
            tmux_profile: None,
            host_path: None,
            identity: None,
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
    /// use harw_sandbox ::{BwrapLauncher, NetworkMode};
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

    /// Bindet ein zuvor validiertes tmux-Inspektionsprofil an diesen Launcher.
    /// Das Profil kann nur beim Aufbau des Launchers gesetzt werden; Tool-Aufrufe
    /// können es nicht liefern oder überschreiben.
    #[must_use]
    pub fn with_tmux_profile(mut self, profile: TmuxSandboxProfile) -> Self {
        self.tmux_profile = Some(profile);
        self
    }

    /// Das konfigurierte tmux-Profil, falls die Sandbox tmux anbietet.
    #[must_use]
    pub fn tmux_profile(&self) -> Option<&TmuxSandboxProfile> {
        self.tmux_profile.as_ref()
    }

    /// Bindet den Host-`PATH` (und optional Host-Cargo/-Rustup-Verzeichnisse)
    /// an diesen Launcher; siehe [`plan`](Self::plan) für die genaue Wirkung
    /// (Teil C1, Plan `recursive-cooking-lobster.md`). Ohne Aufruf bleibt der
    /// Plan byte-identisch zum bisherigen Minimal-`PATH`.
    #[must_use]
    pub fn with_host_path(mut self, binding: HostPathBinding) -> Self {
        self.host_path = Some(binding);
        self
    }

    /// Die konfigurierte Host-`PATH`-Bindung, falls per
    /// [`Self::with_host_path`] gesetzt.
    #[must_use]
    pub fn host_path(&self) -> Option<&HostPathBinding> {
        self.host_path.as_ref()
    }

    /// Überschreibt die uid/gid, die [`plan`](Self::plan) als `--uid`/`--gid`
    /// in den Plan einträgt. Ohne Aufruf ermittelt `plan` die echte uid/gid
    /// des laufenden `harw`-Prozesses aus `/proc/self`; dieser Builder dient
    /// vor allem Tests eine feste, von der tatsächlichen Prozessidentität
    /// unabhängige uid/gid vorzugeben.
    #[must_use]
    pub fn with_identity(mut self, uid: u32, gid: u32) -> Self {
        self.identity = Some((uid, gid));
        self
    }

    /// Die per [`Self::with_identity`] gesetzte uid/gid-Überschreibung, falls
    /// vorhanden.
    #[must_use]
    pub fn identity(&self) -> Option<(u32, u32)> {
        self.identity
    }

    /// Setzt ein gebündeltes [`SandboxProfile`] und konfiguriert damit alle
    /// Module in einem Schritt. `Strict` löscht Cargo/tmux; `Cargo` und `Tmux`
    /// setzen das jeweilige Profil; `Host` ist ein Marker, der hier keine
    /// Sandbox-Bindungen aktiviert — die eigentliche Host-Ausführung läuft in
    /// harw-tool-shell ohne bwrap.
    ///
    /// Diese Methode ist der bevorzugte Weg, ein Profil zu setzen. Sie ersetzt
    /// nicht die individuellen Builder, erlaubt aber dem Runtime-Aufbau, ein
    /// komplettes Profil in einem Aufruf zu übernehmen.
    #[must_use]
    pub fn with_profile(mut self, profile: &SandboxProfile) -> Self {
        match profile {
            SandboxProfile::Strict => {
                self.cargo_profile = None;
                self.tmux_profile = None;
            }
            SandboxProfile::Cargo(cargo) => {
                self.cargo_profile = Some(cargo.clone());
                self.tmux_profile = None;
            }
            SandboxProfile::Tmux(tmux) => {
                self.cargo_profile = None;
                self.tmux_profile = Some(tmux.clone());
            }
            SandboxProfile::Host => {
                // Host-Ausführung erfolgt in harw-tool-shell ohne bwrap:
                // dieser Launcher plant dafür keine Sandbox-Bindungen.
                self.cargo_profile = None;
                self.tmux_profile = None;
            }
        }
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
        // Zusatzauftrag „echte Prozessidentität in der Sandbox“: `--uid`/
        // `--gid` verlangen laut bwrap(1) zwingend ein explizites
        // `--unshare-user` — das in `--unshare-all` bereits enthaltene
        // `--unshare-user-try` genügt dafür nicht (bwrap lehnt `--uid`/
        // `--gid` sonst beim Start ab). Das macht den Sandbox-Start hart
        // abhängig von einem verfügbaren unprivilegierten User-Namespace
        // statt wie bisher best-effort; bewusste Entscheidung, damit
        // `whoami`/`id` in der Sandbox stabil auf die echte
        // Prozessidentität auflösen (siehe `/etc/passwd`-Bindung unten).
        // Scheitert die Identitätsermittlung, bleibt der Plan ohne
        // `--uid`/`--gid`/`--unshare-user` (siehe `resolve_process_identity`).
        let identity = self.identity.or_else(resolve_process_identity);
        if let Some((uid, gid)) = identity {
            args.extend([
                OsString::from("--unshare-user"),
                OsString::from("--uid"),
                OsString::from(uid.to_string()),
                OsString::from("--gid"),
                OsString::from(gid.to_string()),
            ]);
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
        // C1: Mit `with_host_path` gesetzter Launcher setzt sofort den
        // vollen Host-`PATH` statt des bisherigen Minimal-`PATH`; ohne
        // Aufruf bleibt der Minimal-`PATH` unverändert (Plan bleibt
        // byte-identisch zu vorher). `HOME` bleibt in jedem Fall
        // `/tmp/home` (unverändert, siehe Plan Teil C1).
        let minimal_path = "/usr/local/bin:/usr/bin:/bin";
        let base_path = self
            .host_path
            .as_ref()
            .map_or_else(|| minimal_path.to_owned(), |binding| binding.path.clone());
        args.extend([
            OsString::from("--setenv"),
            OsString::from("HOME"),
            OsString::from("/tmp/home"),
            OsString::from("--setenv"),
            OsString::from("PATH"),
            OsString::from(base_path),
        ]);
        // Zusatzauftrag „echte Prozessidentität in der Sandbox“: `USER`/
        // `LOGNAME` aus der Umgebung des `harw`-Prozesses übernehmen, damit
        // Tools, die den Namen statt der uid lesen, denselben Nutzer sehen
        // wie `--uid`/`--gid` oben. Weggelassen, wenn keine der beiden
        // Host-Variablen gesetzt ist.
        if let Some(username) = host_username() {
            args.extend([
                OsString::from("--setenv"),
                OsString::from("USER"),
                OsString::from(username.clone()),
                OsString::from("--setenv"),
                OsString::from("LOGNAME"),
                OsString::from(username),
            ]);
        }
        if let Some(spec) = relay {
            args.extend([
                OsString::from("--setenv"),
                OsString::from("ALL_PROXY"),
                OsString::from(format!("socks5h://127.0.0.1:{}", spec.listen_port)),
            ]);
        }
        if let Some(profile) = &self.cargo_profile {
            let sandbox_cargo_dir = Path::new(SANDBOX_CARGO_PATH).parent().ok_or_else(|| {
                SandboxError::FixedPathWithoutParent {
                    path: PathBuf::from(SANDBOX_CARGO_PATH),
                }
            })?;
            if matches!(profile.mode(), CargoExecutionMode::Fetch)
                && (relay.is_none() || sandbox.network_scope().is_empty())
            {
                return Err(SandboxError::CargoFetchNetworkDenied);
            }
            // C1: bei gesetztem `host_path` bleibt der bisherige Vortritt des
            // Sandbox-Cargo-bin-Verzeichnisses erhalten; nur der Rest des
            // `PATH` wird der volle Host-`PATH` statt des Minimal-`PATH`.
            let cargo_path_tail = self
                .host_path
                .as_ref()
                .map_or_else(|| minimal_path.to_owned(), |binding| binding.path.clone());
            args.extend([
                OsString::from("--setenv"),
                OsString::from("RUSTUP_HOME"),
                OsString::from(SANDBOX_RUSTUP_HOME),
                OsString::from("--setenv"),
                OsString::from("CARGO_HOME"),
                OsString::from(SANDBOX_CARGO_HOME),
                OsString::from("--setenv"),
                OsString::from("PATH"),
                OsString::from(format!("{}:{cargo_path_tail}", sandbox_cargo_dir.display())),
            ]);
            if profile.mode().offline() {
                args.extend([
                    OsString::from("--setenv"),
                    OsString::from("CARGO_NET_OFFLINE"),
                    OsString::from("true"),
                ]);
            }
        }

        if let Some(tmux) = &self.tmux_profile {
            let socket_writable = tmux.mode().socket_writable();
            append_destination_dirs(
                &mut args,
                Path::new(SANDBOX_TMUX_SOCKET_PATH)
                    .parent()
                    .ok_or_else(|| SandboxError::FixedPathWithoutParent {
                        path: PathBuf::from(SANDBOX_TMUX_SOCKET_PATH),
                    })?,
            )?;
            args.push(if socket_writable {
                OsString::from("--bind")
            } else {
                OsString::from("--ro-bind")
            });
            args.extend([
                tmux.socket_path().as_os_str().to_owned(),
                OsString::from(SANDBOX_TMUX_SOCKET_PATH),
            ]);
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
        // Zusatzauftrag „echte Prozessidentität in der Sandbox“: glibc-NSS
        // (`whoami`/`id`) braucht `/etc/passwd`, `/etc/group` und
        // `/etc/nsswitch.conf`, um die oben gesetzte `--uid`/`--gid` auf
        // einen Namen aufzulösen. `--ro-bind-try` statt `--ro-bind`, weil
        // keine dieser Dateien in jeder Host-Umgebung existieren muss (wie
        // bei der tmux-Socket-Bindung wird nur der Elternpfad per
        // `append_destination_dirs` angelegt, nicht die Zieldatei selbst).
        // Kein anderer Teil dieses Plans bindet bisher etwas unter `/etc`,
        // also genügt ein einzelner Aufruf für den gemeinsamen Elternpfad.
        append_destination_dirs(&mut args, Path::new("/etc"))?;
        for nss_file in ["/etc/passwd", "/etc/group", "/etc/nsswitch.conf"] {
            args.extend([
                OsString::from("--ro-bind-try"),
                OsString::from(nss_file),
                OsString::from(nss_file),
            ]);
        }
        if let Some(profile) = &self.cargo_profile {
            let cargo_dir = profile.cargo_bin().parent().ok_or_else(|| {
                SandboxError::FixedPathWithoutParent {
                    path: profile.cargo_bin().to_path_buf(),
                }
            })?;
            let sandbox_cargo_dir = Path::new(SANDBOX_CARGO_PATH).parent().ok_or_else(|| {
                SandboxError::FixedPathWithoutParent {
                    path: PathBuf::from(SANDBOX_CARGO_PATH),
                }
            })?;
            append_destination_dirs(&mut args, sandbox_cargo_dir)?;
            args.extend([
                OsString::from("--ro-bind"),
                cargo_dir.as_os_str().to_owned(),
                sandbox_cargo_dir.as_os_str().to_owned(),
            ]);
            append_destination_dirs(&mut args, Path::new(SANDBOX_RUSTUP_HOME))?;
            args.extend([
                OsString::from("--ro-bind"),
                profile.rustup_home().as_os_str().to_owned(),
                OsString::from(SANDBOX_RUSTUP_HOME),
            ]);
            append_destination_dirs(&mut args, Path::new(SANDBOX_CARGO_HOME))?;
            args.push(if profile.mode().cache_writable() {
                OsString::from("--bind")
            } else {
                OsString::from("--ro-bind")
            });
            args.extend([
                profile.cargo_home().as_os_str().to_owned(),
                OsString::from(SANDBOX_CARGO_HOME),
            ]);
        }
        if let Some(binding) = &self.host_path {
            let mut created_dirs: HashSet<PathBuf> = HashSet::new();
            let mut seen_entries: HashSet<&str> = HashSet::new();
            let stdlib_prefixes = [
                Path::new("/usr"),
                Path::new("/bin"),
                Path::new("/lib"),
                Path::new("/lib64"),
            ];
            // Für jeden PATH-Eintrag (in Reihenfolge, Duplikate raus): leer/
            // relativ überspringen; `/` und exakt `home` überspringen;
            // Einträge unter /usr,/bin,/lib,/lib64 (oben bereits gebunden)
            // nicht erneut binden; Einträge gleich/unter dem Workspace nicht
            // binden (der folgt gleich selbst); sonst `--ro-bind-try`.
            for entry in binding.path.split(':') {
                if entry.is_empty() {
                    continue;
                }
                let dir = Path::new(entry);
                if !dir.is_absolute() {
                    continue;
                }
                if !seen_entries.insert(entry) {
                    continue;
                }
                if dir == Path::new("/") {
                    continue;
                }
                if binding.home.as_deref().is_some_and(|home| dir == home) {
                    continue;
                }
                if stdlib_prefixes.iter().any(|prefix| dir.starts_with(prefix)) {
                    continue;
                }
                if dir.starts_with(workspace) {
                    continue;
                }
                append_destination_dirs_dedup(&mut args, dir, &mut created_dirs)?;
                args.extend([
                    OsString::from("--ro-bind-try"),
                    dir.as_os_str().to_owned(),
                    dir.as_os_str().to_owned(),
                ]);
            }

            // rustup-Proxies unter `<cargo_home>/bin` brauchen zusätzlich den
            // gesamten `rustup_home`/`cargo_home`-Baum (Toolchains,
            // Registry-Cache), nicht nur das `bin`-Verzeichnis aus der
            // Schleife oben. Ist ein `cargo_profile` aktiv, hat dessen
            // eigene, oben bereits gesetzte Bindung/Umgebung Vorrang: kein
            // doppeltes/konkurrierendes `--setenv CARGO_HOME`/`RUSTUP_HOME`
            // bzw. `--ro-bind-try` auf denselben Sandbox-Zielpfad.
            let cargo_home = binding
                .cargo_home
                .clone()
                .or_else(|| binding.home.as_ref().map(|home| home.join(".cargo")));
            if let Some(cargo_home) = cargo_home {
                let cargo_bin = cargo_home.join("bin");
                let path_has_cargo_bin = binding
                    .path
                    .split(':')
                    .any(|entry| !entry.is_empty() && Path::new(entry) == cargo_bin.as_path());
                if path_has_cargo_bin && self.cargo_profile.is_none() {
                    let rustup_home = binding
                        .rustup_home
                        .clone()
                        .or_else(|| binding.home.as_ref().map(|home| home.join(".rustup")));
                    if let Some(rustup_home) = rustup_home {
                        append_destination_dirs_dedup(&mut args, &rustup_home, &mut created_dirs)?;
                        args.extend([
                            OsString::from("--ro-bind-try"),
                            rustup_home.as_os_str().to_owned(),
                            rustup_home.as_os_str().to_owned(),
                            OsString::from("--setenv"),
                            OsString::from("RUSTUP_HOME"),
                            rustup_home.as_os_str().to_owned(),
                        ]);
                    }
                    append_destination_dirs_dedup(&mut args, &cargo_home, &mut created_dirs)?;
                    args.extend([
                        OsString::from("--ro-bind-try"),
                        cargo_home.as_os_str().to_owned(),
                        cargo_home.as_os_str().to_owned(),
                        OsString::from("--setenv"),
                        OsString::from("CARGO_HOME"),
                        cargo_home.as_os_str().to_owned(),
                    ]);
                }
            }
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

    /// Wartet nicht-blockierend auf das Prozessende, geracet gegen `cancel`
    /// (Plan „Welle 3 — 3c", Nachzieh-Task: `SandboxChild` konsistent
    /// abbrechbar wie `shell.exec` in harw-tool-shell).
    ///
    /// # Description
    /// `std::process::Child` kennt kein async-natives Warten; die Methode
    /// pollt daher `Child::try_wait` alle [`WAIT_OR_CANCEL_POLL_INTERVAL`]
    /// (20 ms) und racet die Wartezeit dazwischen per `tokio::select!` gegen
    /// [`CancelToken::cancelled`]. Beendet sich der Prozess von selbst zuerst,
    /// liefert die Methode dessen Status. Feuert `cancel` zuerst, wird der
    /// Prozess per `SIGKILL` beendet (`Child::kill`) und blockierend
    /// eingesammelt (`Child::wait`), damit kein Zombie zurückbleibt. In
    /// beiden Fällen markiert die Methode den Prozess als eingesammelt, sodass
    /// `Drop` ihn danach nicht erneut tötet.
    ///
    /// # Arguments
    /// - `cancel` (`&CancelToken`): Token, gegen das die Wartezeit geracet
    ///   wird; `cancel.is_cancelled()` kann beim Aufruf bereits `true` sein,
    ///   dann kehrt der erste `select!`-Durchlauf sofort in den
    ///   Abbruchzweig ein.
    ///
    /// # Returns
    /// `Ok(Some(status))` bei normalem Prozessende (`ExitStatus` des
    /// `bwrap`-Prozesses). `Ok(None)`, wenn `cancel` zuerst gefeuert hat und
    /// der Prozess deswegen getötet wurde — bewusst kein eigener
    /// Abbruch-Fehlervariant, da `Ok(None)` den Abbruch bereits eindeutig vom
    /// Erfolgsfall unterscheidet und Kill/Reap dabei planmäßig ablaufen (kein
    /// Fehlerfall).
    ///
    /// # Errors
    /// [`SandboxError::Io`] mit dem Executable-Pfad, wenn `try_wait`, `kill`
    /// oder das abschließende `wait` scheitert (echte E/A-Fehler bleiben
    /// [`SandboxError::Io`], konsistent mit [`wait`](Self::wait)).
    ///
    /// # Concurrency
    /// Blockiert den aufrufenden Task höchstens für ein Poll-Intervall
    /// zwischen zwei Prüfungen; sicher gegenüber einem `cancel`, das
    /// gleichzeitig auf einem anderen Task ausgelöst wird.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # async fn demo(
    /// #     launcher: &harw_sandbox::BwrapLauncher,
    /// #     plan: &harw_sandbox::BwrapCommandPlan,
    /// #     cancel: &harw_types::cancel::CancelToken,
    /// # ) -> Result<(), harw_sandbox::SandboxError> {
    /// let mut child = launcher.spawn(plan)?;
    /// match child.wait_or_cancel(cancel).await? {
    ///     Some(status) => println!("sandbox exited: {status}"),
    ///     None => println!("sandbox cancelled"),
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn wait_or_cancel(
        &mut self,
        cancel: &CancelToken,
    ) -> SandboxResult<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| SandboxError::Io {
                path: self.executable.to_path_buf(),
                reason: format!("polling sandbox process failed: {error}"),
            })? {
                self.reaped = true;
                return Ok(Some(status));
            }
            tokio::select! {
                () = tokio::time::sleep(WAIT_OR_CANCEL_POLL_INTERVAL) => {}
                () = cancel.cancelled() => {
                    self.child.kill().map_err(|error| SandboxError::Io {
                        path: self.executable.to_path_buf(),
                        reason: format!("killing sandbox process on cancel failed: {error}"),
                    })?;
                    self.child.wait().map_err(|error| SandboxError::Io {
                        path: self.executable.to_path_buf(),
                        reason: format!("reaping sandbox process after cancel failed: {error}"),
                    })?;
                    self.reaped = true;
                    return Ok(None);
                }
            }
        }
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

/// Wie [`append_destination_dirs`], überspringt aber bereits erzeugte
/// Zielverzeichnisse: verhindert doppelte `--dir`-Einträge, wenn mehrere
/// Host-Pfade (Teil C1) denselben Elternpfad teilen, z. B. `~/.cargo/bin`
/// und `~/.cargo`. `created` wird über mehrere Aufrufe hinweg vom Aufrufer
/// weitergereicht.
fn append_destination_dirs_dedup(
    args: &mut Vec<OsString>,
    destination: &Path,
    created: &mut HashSet<PathBuf>,
) -> SandboxResult<()> {
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
                if created.insert(current.clone()) {
                    args.extend([OsString::from("--dir"), current.as_os_str().to_owned()]);
                }
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

/// Ermittelt die echte uid/gid des laufenden `harw`-Prozesses über
/// `/proc/self` (kein `unsafe`, keine neue Abhängigkeit: `libc::getuid`
/// bräuchte beides). [`std::fs::metadata`] auf `/proc/self` liefert die
/// effektive uid/gid des aufrufenden Prozesses als Eigentümer dieses
/// (virtuellen) `procfs`-Eintrags.
///
/// # Returns
/// `Some((uid, gid))` bei Erfolg; `None` (mit `tracing::warn!`), wenn
/// `/proc/self` nicht gelesen werden kann (z. B. `procfs` nicht gemountet) —
/// [`BwrapLauncher::plan`] lässt `--uid`/`--gid`/`--unshare-user` dann
/// einfach weg, statt den Plan scheitern zu lassen.
fn resolve_process_identity() -> Option<(u32, u32)> {
    match std::fs::metadata("/proc/self") {
        Ok(metadata) => Some((metadata.uid(), metadata.gid())),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to resolve process uid/gid from /proc/self; sandbox plan omits --uid/--gid"
            );
            None
        }
    }
}

/// Anzeigename des `harw`-Prozesses für `--setenv USER`/`--setenv LOGNAME`
/// in der Sandbox: `USER` aus der eigenen Umgebung, sonst `LOGNAME`.
///
/// # Returns
/// `Some(name)`, wenn mindestens eine der beiden Variablen gesetzt ist;
/// sonst `None` — der Aufrufer lässt `USER`/`LOGNAME` dann in der Sandbox
/// unverändert (aus `--clearenv` gelöscht) statt einen falschen Namen zu
/// setzen.
fn host_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    // `ExtraRootsCell` wird nur vom unten deaktivierten
    // `plan_binds_every_extra_root_with_workspace_rights_and_dedupes`
    // gebraucht; mit auskommentiertem Testkörper bliebe der Import
    // ungenutzt.
    // use crate::ExtraRootsCell;
    // Authority-Typen (`PermissionSet`, `WorkspaceRegistration`,
    // `WorkspaceRegistry`) leben in harw-authority, nicht in dieser Crate:
    // harw-sandbox re-exportiert bewusst keine Authority-Typen (siehe
    // `crate`-Doku in `src/lib.rs`).
    use crate::test_support::{TestError, TestResult};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    fn sandbox(permissions: PermissionSet) -> TestResult<SandboxSpec> {
        let root = std::env::temp_dir().join(format!("harwness-bwrap-{}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )?;
        Ok(SandboxSpec::from_resolved(
            registry.resolve(
                &TenantId::from_str("tenant"),
                &WorkspaceId::from_str("workspace"),
            )?,
            permissions,
        ))
    }

    fn strings(plan: &BwrapCommandPlan) -> Vec<String> {
        plan.args()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn readonly_plan_has_no_network_or_workspace_write_mount() -> TestResult {
        let spec = sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]))?;
        let plan = BwrapLauncher::default()
            .plan(&spec, &[OsString::from("/bin/true")])
            .map_err(TestError::Sandbox)?;
        let args = strings(&plan);
        assert!(args.contains(&"--unshare-all".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--ro-bind".to_owned()));
        Ok(())
    }

    #[test]
    fn writable_networked_plan_requires_each_explicit_permission() -> TestResult {
        // W5 N-SBX: `NetworkAccess` teilt den Host-netns nicht mehr.
        let spec = sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
        ]))?;
        let plan = BwrapLauncher::default()
            .plan(&spec, &[OsString::from("/bin/true")])
            .map_err(TestError::Sandbox)?;
        let args = strings(&plan);
        assert!(args.contains(&"--unshare-net".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--bind".to_owned()));
        Ok(())
    }

    fn relay_spec() -> RelaySpec {
        RelaySpec {
            binary: PathBuf::from("/opt/harw/bin/harw-netns-relay"),
            listen_port: 18_080,
            proxy_socket: PathBuf::from("/run/user/1000/harw/egress.sock"),
        }
    }

    fn networked_sandbox() -> TestResult<SandboxSpec> {
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

    // BLOCKED (nicht auf eine reale API übertragbar, siehe Handback-Bericht):
    // Dieser Test prüft, dass `BwrapLauncher::plan` jede in einer
    // `ExtraRootsCell` gesammelte Zusatzwurzel bindet. Der dafür nötige
    // Konstruktionsweg `SandboxSpec::with_extra_roots(ExtraRootsCell)`
    // existiert nirgends: `SandboxSpec` (harw-authority/src/lib.rs) hat außer
    // `from_resolved`, `from_authority` und dem testgated
    // `from_resolved_for_test` keine öffentlichen Konstruktoren/Setter, kein
    // Extra-Roots-Feld, und `BwrapLauncher::plan` nimmt auch keinen separaten
    // `&ExtraRootsCell`-Parameter entgegen — Extra-Roots-Bindung ist in der
    // produktiven `plan`-Pipeline schlicht nicht verdrahtet. Die Assertion
    // absichtlich nicht abgeschwächt/umgeschrieben; die ursprüngliche
    // Testlogik bleibt unten als Referenz stehen, bis die reale API existiert
    // oder der Test offiziell verworfen wird.
    //
    // #[test]
    // fn plan_binds_every_extra_root_with_workspace_rights_and_dedupes() {
    //     let base = sandbox(PermissionSet::from_policy([
    //         Permission::ReadWorkspace,
    //         Permission::WriteWorkspace,
    //         Permission::ExecuteProcess,
    //     ]));
    //     let primary = base.workspace().canonical_root().to_path_buf();
    //     let base_dir = primary.parent().unwrap().to_path_buf();
    //
    //     let extra_a = base_dir.join("extra-a");
    //     let extra_b = base_dir.join("extra-b");
    //     std::fs::create_dir_all(&extra_a).unwrap();
    //     std::fs::create_dir_all(&extra_b).unwrap();
    //
    //     let extra_roots = ExtraRootsCell::new();
    //     extra_roots.add(&extra_a, false, &primary, None).unwrap();
    //     extra_roots.add(&extra_b, false, &primary, None).unwrap();
    //     // Ein Duplikat derselben Wurzel darf keinen zweiten Bind erzeugen.
    //     assert!(!extra_roots.add(&extra_a, false, &primary, None).unwrap());
    //
    //     let spec = base.with_extra_roots(extra_roots);
    //     let plan = BwrapLauncher::default()
    //         .plan(&spec, &[OsString::from("/bin/true")])
    //         .unwrap();
    //     let args = strings(&plan);
    //
    //     let extra_a_canonical = extra_a.canonicalize().unwrap();
    //     let extra_b_canonical = extra_b.canonicalize().unwrap();
    //     let extra_a_str = extra_a_canonical.to_str().unwrap();
    //     let extra_b_str = extra_b_canonical.to_str().unwrap();
    //
    //     assert_eq!(
    //         count_window(&args, &["--bind", extra_a_str, extra_a_str]),
    //         1
    //     );
    //     assert_eq!(
    //         count_window(&args, &["--bind", extra_b_str, extra_b_str]),
    //         1
    //     );
    // }

    #[test]
    fn plan_without_extra_roots_binds_only_the_primary_workspace() -> TestResult {
        let base = sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]))?;
        let primary = base.workspace().canonical_root().to_path_buf();

        let plan = BwrapLauncher::default()
            .plan(&base, &[OsString::from("/bin/true")])
            .map_err(TestError::Sandbox)?;
        let args = strings(&plan);

        let primary_str = primary
            .to_str()
            .ok_or(TestError::Missing("primary workspace path as UTF-8"))?;
        assert_eq!(
            count_window(&args, &["--ro-bind", primary_str, primary_str]),
            1
        );
        Ok(())
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
    fn test_plan_network_none_unshares_net_without_proxy() -> TestResult {
        let spec = networked_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(args.iter().filter(|a| *a == "--unshare-net").count(), 1);
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(!args.contains(&"ALL_PROXY".to_owned()));
        assert!(!args.contains(&SANDBOX_RELAY_PATH.to_owned()));
        assert!(!args.contains(&SANDBOX_PROXY_SOCKET_PATH.to_owned()));
        assert!(args.contains(&"--die-with-parent".to_owned()));
        Ok(())
    }

    #[test]
    fn test_plan_proxy_only_binds_exactly_socket_and_sets_all_proxy() -> TestResult {
        let spec = networked_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_network_mode(NetworkMode::ProxyOnly(relay_spec()))
                .plan(&spec, &[OsString::from("/bin/echo"), OsString::from("hi")])
                .map_err(TestError::Sandbox)?,
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
        let separator = args
            .iter()
            .position(|a| a == "--")
            .ok_or(TestError::Missing("'--' separator in args"))?;
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
        let clearenv = args
            .iter()
            .position(|a| a == "--clearenv")
            .ok_or(TestError::Missing("'--clearenv' in args"))?;
        let all_proxy = args
            .iter()
            .position(|a| a == "ALL_PROXY")
            .ok_or(TestError::Missing("'ALL_PROXY' in args"))?;
        assert!(clearenv < all_proxy && all_proxy < separator);
        Ok(())
    }

    #[test]
    fn test_plan_proxy_only_without_network_access_is_denied() -> TestResult {
        let spec = execute_sandbox()?;
        let error = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
            .with_network_mode(NetworkMode::ProxyOnly(relay_spec()))
            .plan(&spec, &[OsString::from("/bin/true")]);
        let Err(error) = error else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, SandboxError::NetworkModeNotGranted));
        Ok(())
    }

    #[test]
    fn test_plan_proxy_only_rejects_invalid_relay_spec() -> TestResult {
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
            let networked = networked_sandbox()?;
            let result = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_network_mode(NetworkMode::ProxyOnly(spec))
                .plan(&networked, &[OsString::from("/bin/true")]);
            let Err(error) = result else {
                return Err(TestError::Unexpected("Err erwartet".into()));
            };
            assert!(
                matches!(error, SandboxError::InvalidRelaySpec { field, .. } if field == expected),
                "expected InvalidRelaySpec({expected}), got {error:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_plan_never_shares_net_in_any_combination() -> TestResult {
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
                let spec = sandbox(PermissionSet::from_policy(granted))?;
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
        Ok(())
    }

    #[test]
    #[ignore = "startet /bin/sleep als Host-Prozess; Laufzeit-Erkennung im Test"]
    fn test_sandbox_child_drop_kills_and_reaps_process() -> TestResult {
        let sleep = Path::new("/bin/sleep");
        if !sleep.is_file() {
            eprintln!("übersprungen: /bin/sleep fehlt");
            return Ok(());
        }
        let child = Command::new(sleep)
            .arg("300")
            .stdin(Stdio::null())
            .spawn()?;
        let guard = SandboxChild::new(child, sleep);
        let proc_entry = PathBuf::from(format!("/proc/{}", guard.id()));
        assert!(proc_entry.exists());
        let started = std::time::Instant::now();
        drop(guard);
        // Nach kill + wait ist der Prozess eingesammelt: kein /proc-Eintrag mehr.
        assert!(!proc_entry.exists());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        Ok(())
    }

    #[test]
    #[ignore = "startet /bin/echo als Host-Prozess; Laufzeit-Erkennung im Test"]
    fn test_spawn_pipes_output_and_wait_collects_status() -> TestResult {
        use std::io::Read;
        let echo = Path::new("/bin/echo");
        if !echo.is_file() {
            eprintln!("übersprungen: /bin/echo fehlt");
            return Ok(());
        }
        // `/bin/echo` statt bwrap: druckt den Argumentvektor des Plans.
        let launcher = BwrapLauncher::new(echo.to_path_buf());
        let spec = execute_sandbox()?;
        let plan = launcher
            .plan(&spec, &[OsString::from("/bin/true")])
            .map_err(TestError::Sandbox)?;
        let mut child = launcher.spawn(&plan).map_err(TestError::Sandbox)?;
        let mut stdout = child
            .take_stdout()
            .ok_or(TestError::Missing("child stdout"))?;
        assert!(child.take_stdout().is_none());
        assert!(child.take_stderr().is_some());
        let mut output = String::new();
        stdout.read_to_string(&mut output)?;
        assert!(output.contains("--unshare-net"));
        assert!(child.wait()?.success());
        // Wiederholtes Warten liefert denselben Status; Drop tötet nicht mehr.
        assert!(child.wait()?.success());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "startet /bin/sleep als Host-Prozess; Laufzeit-Erkennung im Test"]
    async fn wait_or_cancel_kills_and_reaps_long_running_process_on_cancel() -> TestResult {
        use harw_types::cancel::{CancelReason, CancelToken};

        let sleep = Path::new("/bin/sleep");
        if !sleep.is_file() {
            eprintln!("übersprungen: /bin/sleep fehlt");
            return Ok(());
        }
        let child = Command::new(sleep)
            .arg("300")
            .stdin(Stdio::null())
            .spawn()?;
        let mut guard = SandboxChild::new(child, sleep);
        let proc_entry = PathBuf::from(format!("/proc/{}", guard.id()));
        assert!(proc_entry.exists());

        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel(CancelReason::User);
        });

        let started = std::time::Instant::now();
        let result = guard.wait_or_cancel(&cancel).await?;
        assert!(result.is_none(), "cancel must yield None, got {result:?}");
        // Nach Kill + Wait ist der Prozess eingesammelt: kein /proc-Eintrag mehr.
        assert!(!proc_entry.exists());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // `Drop` darf den bereits eingesammelten Prozess nicht erneut töten.
        drop(guard);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "startet /bin/true als Host-Prozess; Laufzeit-Erkennung im Test"]
    async fn wait_or_cancel_returns_exit_status_without_cancel() -> TestResult {
        use harw_types::cancel::CancelToken;

        let true_bin = Path::new("/bin/true");
        if !true_bin.is_file() {
            eprintln!("übersprungen: /bin/true fehlt");
            return Ok(());
        }
        let child = Command::new(true_bin).stdin(Stdio::null()).spawn()?;
        let mut guard = SandboxChild::new(child, true_bin);
        let cancel = CancelToken::new();

        let status = guard.wait_or_cancel(&cancel).await?;
        assert!(
            status.is_some_and(|status| status.success()),
            "expected Some(success), got {status:?}"
        );
        Ok(())
    }

    #[test]
    fn process_start_is_denied_without_execute_permission() -> TestResult {
        let spec = sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let result = BwrapLauncher::default().plan(&spec, &[OsString::from("/bin/true")]);
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, SandboxError::ProcessExecutionDenied));
        Ok(())
    }

    fn execute_sandbox() -> TestResult<SandboxSpec> {
        sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]))
    }

    #[test]
    fn tmpfs_size_directly_precedes_tmpfs_action() -> TestResult {
        let size = NonZeroU64::new(268_435_456).ok_or(TestError::Missing("non-zero tmpfs size"))?;
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap")).with_tmpfs_size(size);
        assert_eq!(launcher.tmpfs_size(), Some(size));
        let spec = execute_sandbox()?;
        let args = strings(
            &launcher
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        let tmpfs = args
            .iter()
            .position(|argument| argument == "--tmpfs")
            .ok_or(TestError::Missing("'--tmpfs' in args"))?;
        // bwrap(1): `--size` gilt nur für die unmittelbar folgende `--tmpfs`-Aktion.
        assert_eq!(args[tmpfs - 2], "--size");
        assert_eq!(args[tmpfs - 1], "268435456");
        assert_eq!(args[tmpfs + 1], "/tmp");
        assert_eq!(
            args.iter().filter(|argument| *argument == "--size").count(),
            1
        );
        Ok(())
    }

    #[test]
    fn plan_without_tmpfs_size_has_no_size_option() -> TestResult {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(launcher.tmpfs_size(), None);
        let spec = execute_sandbox()?;
        let args = strings(
            &launcher
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert!(!args.contains(&"--size".to_owned()));
        Ok(())
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
    fn discover_returns_only_trusted_fixed_candidates() -> TestResult {
        match BwrapLauncher::discover() {
            Ok(launcher) => {
                let executable_str = launcher
                    .executable()
                    .to_str()
                    .ok_or(TestError::Missing("launcher executable as UTF-8"))?;
                assert!(BWRAP_CANDIDATES.contains(&executable_str));
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
        Ok(())
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
    fn user_owned_executable_is_not_trusted() -> TestResult {
        let directory =
            std::env::temp_dir().join(format!("harwness-bwrap-fake-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let fake = directory.join("bwrap");
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
        if std::fs::metadata(&fake)?.uid() == 0 {
            eprintln!("übersprungen: Test läuft als root, Eigentümerprüfung nicht beobachtbar");
            return Ok(());
        }
        assert_eq!(
            check_pinned_executable(&fake),
            Err("candidate is not owned by root")
        );
        assert_eq!(
            BwrapLauncher::find_pinned_executable(&[fake.as_path()]),
            None
        );
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    #[test]
    fn first_trusted_candidate_wins_over_missing_ones() {
        let shell = Path::new("/bin/sh");
        if check_pinned_executable(shell).is_err() {
            eprintln!("übersprungen: /bin/sh ist hier kein root-eigenes, geschütztes Binary");
            return;
        }
        let missing =
            std::env::temp_dir().join(format!("harwness-bwrap-none-{}/bwrap", std::process::id()));
        assert_eq!(
            BwrapLauncher::find_pinned_executable(&[missing.as_path(), shell]),
            Some(PathBuf::from("/bin/sh"))
        );
    }

    #[test]
    fn spawn_rejects_relative_executable() -> TestResult {
        let spec = execute_sandbox()?;
        let plan = BwrapLauncher::default()
            .plan(&spec, &[OsString::from("/bin/true")])
            .map_err(TestError::Sandbox)?;
        let result = BwrapLauncher::new(PathBuf::from("bwrap")).spawn(&plan);
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(
            error,
            SandboxError::SandboxProcessSpawn { ref reason, .. } if reason.contains("absolute")
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tmux_profile_binds_socket_readonly_in_inspect_mode() -> TestResult {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!(
            "harwness-bwrap-tmux-inspect-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let sock = dir.join("tmux.sock");
        let _listener = UnixListener::bind(&sock)?;
        let profile = crate::TmuxSandboxProfile::new(crate::TmuxOperationMode::Inspect, &sock)?;
        let launcher =
            BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap")).with_tmux_profile(profile);
        let spec = execute_sandbox()?;
        let args = strings(
            &launcher
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        let sock_str = sock
            .to_str()
            .ok_or(TestError::Missing("tmux socket path as UTF-8"))?;
        // Socket wird ge-binded (ro-bind im Inspect-Modus)
        assert_eq!(
            count_window(&args, &["--ro-bind", sock_str, SANDBOX_TMUX_SOCKET_PATH]),
            1,
            "tmux socket must be bound read-only in inspect mode: {args:?}"
        );
        // Kein --bind fuer den Socket
        assert_eq!(
            count_window(&args, &["--bind", sock_str, SANDBOX_TMUX_SOCKET_PATH]),
            0
        );
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tmux_profile_binds_socket_writable_in_write_mode() -> TestResult {
        use std::os::unix::net::UnixListener;
        let dir =
            std::env::temp_dir().join(format!("harwness-bwrap-tmux-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let sock = dir.join("tmux-write.sock");
        let _listener = UnixListener::bind(&sock)?;
        let profile = crate::TmuxSandboxProfile::new(crate::TmuxOperationMode::Write, &sock)?;
        let launcher =
            BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap")).with_tmux_profile(profile);
        let spec = execute_sandbox()?;
        let args = strings(
            &launcher
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        let sock_str = sock
            .to_str()
            .ok_or(TestError::Missing("tmux socket path as UTF-8"))?;
        assert_eq!(
            count_window(&args, &["--bind", sock_str, SANDBOX_TMUX_SOCKET_PATH]),
            1,
            "tmux socket must be bound writable in write mode: {args:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn tmux_profile_absent_by_default() {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert!(launcher.tmux_profile().is_none());
    }

    // ---- C1: HostPathBinding / BwrapLauncher::with_host_path ----

    #[test]
    fn from_env_reads_real_process_path_when_set() -> TestResult {
        if let Ok(expected) = std::env::var("PATH") {
            if expected.is_empty() {
                assert_eq!(HostPathBinding::from_env(), None);
            } else {
                let binding = HostPathBinding::from_env()
                    .ok_or(TestError::Missing("PATH is set and non-empty"))?;
                assert_eq!(binding.path, expected);
            }
        }
        Ok(())
    }

    #[test]
    fn test_with_host_path_sets_host_path_getter() {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert!(launcher.host_path().is_none());
        let binding = HostPathBinding {
            path: "/opt/tool/bin".to_owned(),
            ..HostPathBinding::default()
        };
        let launcher = launcher.with_host_path(binding.clone());
        assert_eq!(launcher.host_path(), Some(&binding));
    }

    #[test]
    fn plan_with_host_path_binds_each_directory_and_sets_full_path() -> TestResult {
        let binding = HostPathBinding {
            path: "/opt/tool-a/bin:/opt/tool-b/bin".to_owned(),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: None,
            cargo_home: None,
        };
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(
            count_window(
                &args,
                &["--ro-bind-try", "/opt/tool-a/bin", "/opt/tool-a/bin"]
            ),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(
                &args,
                &["--ro-bind-try", "/opt/tool-b/bin", "/opt/tool-b/bin"]
            ),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(
                &args,
                &["--setenv", "PATH", "/opt/tool-a/bin:/opt/tool-b/bin"]
            ),
            1,
            "{args:?}"
        );
        // HOME bleibt unverändert im Sandbox-tmpfs (Plan Teil C1).
        assert_eq!(count_window(&args, &["--setenv", "HOME", "/tmp/home"]), 1);
        Ok(())
    }

    #[test]
    fn plan_with_host_path_excludes_root_home_stdlib_and_workspace_entries() -> TestResult {
        let base = execute_sandbox()?;
        let workspace = base.workspace().canonical_root().to_path_buf();
        let nested_workspace_dir = workspace.join("node_modules/.bin");
        let binding = HostPathBinding {
            path: format!(
                "/:/home/tester:/usr/local/sbin:/lib/extra:{}:/opt/tool/bin",
                nested_workspace_dir.display()
            ),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: None,
            cargo_home: None,
        };
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&base, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(
            count_window(&args, &["--ro-bind-try", "/", "/"]),
            0,
            "the bare root entry must never be bound: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "/home/tester"), "{args:?}");
        assert!(!args.iter().any(|a| a == "/usr/local/sbin"), "{args:?}");
        assert!(!args.iter().any(|a| a == "/lib/extra"), "{args:?}");
        let nested_str = nested_workspace_dir.display().to_string();
        assert!(!args.iter().any(|a| a == &nested_str), "{args:?}");
        assert_eq!(
            count_window(&args, &["--ro-bind-try", "/opt/tool/bin", "/opt/tool/bin"]),
            1,
            "{args:?}"
        );
        Ok(())
    }

    #[test]
    fn plan_with_host_path_dedupes_repeated_entries() -> TestResult {
        let binding = HostPathBinding {
            path: "/opt/tool/bin:/opt/tool/bin:/opt/tool/bin".to_owned(),
            home: None,
            rustup_home: None,
            cargo_home: None,
        };
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(
            count_window(&args, &["--ro-bind-try", "/opt/tool/bin", "/opt/tool/bin"]),
            1,
            "{args:?}"
        );
        assert_eq!(count_window(&args, &["--dir", "/opt"]), 1, "{args:?}");
        assert_eq!(count_window(&args, &["--dir", "/opt/tool"]), 1, "{args:?}");
        assert_eq!(
            count_window(&args, &["--dir", "/opt/tool/bin"]),
            1,
            "{args:?}"
        );
        Ok(())
    }

    #[test]
    fn plan_with_host_path_binds_rustup_and_cargo_home_when_cargo_bin_in_path() -> TestResult {
        let binding = HostPathBinding {
            path: "/home/tester/.cargo/bin:/usr/bin".to_owned(),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: None,
            cargo_home: None,
        };
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(
            count_window(
                &args,
                &[
                    "--ro-bind-try",
                    "/home/tester/.rustup",
                    "/home/tester/.rustup"
                ]
            ),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(
                &args,
                &[
                    "--ro-bind-try",
                    "/home/tester/.cargo",
                    "/home/tester/.cargo"
                ]
            ),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(&args, &["--setenv", "RUSTUP_HOME", "/home/tester/.rustup"]),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(&args, &["--setenv", "CARGO_HOME", "/home/tester/.cargo"]),
            1,
            "{args:?}"
        );
        Ok(())
    }

    #[test]
    fn plan_with_host_path_without_cargo_bin_in_path_skips_rustup_and_cargo_home() -> TestResult {
        let binding = HostPathBinding {
            path: "/usr/bin:/opt/tool/bin".to_owned(),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: None,
            cargo_home: None,
        };
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert!(
            !args.iter().any(|a| a == "/home/tester/.rustup"),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "/home/tester/.cargo"), "{args:?}");
        assert!(!args.iter().any(|a| a == "RUSTUP_HOME"), "{args:?}");
        Ok(())
    }

    #[test]
    fn plan_with_host_path_prefers_explicit_rustup_and_cargo_home_over_home_derived() -> TestResult
    {
        let binding = HostPathBinding {
            path: "/custom/cargo/bin:/usr/bin".to_owned(),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: Some(PathBuf::from("/custom/rustup")),
            cargo_home: Some(PathBuf::from("/custom/cargo")),
        };
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(
            count_window(&args, &["--setenv", "RUSTUP_HOME", "/custom/rustup"]),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(&args, &["--setenv", "CARGO_HOME", "/custom/cargo"]),
            1,
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "/home/tester/.rustup"),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "/home/tester/.cargo"), "{args:?}");
        Ok(())
    }

    #[test]
    fn plan_with_host_path_and_cargo_profile_gives_cargo_profile_precedence() -> TestResult {
        let dir = std::env::temp_dir().join(format!(
            "harwness-bwrap-host-path-cargo-{}",
            std::process::id()
        ));
        let bin_dir = dir.join("bin");
        let rustup_home = dir.join("rustup");
        let cargo_home = dir.join("cargo");
        std::fs::create_dir_all(&bin_dir)?;
        std::fs::create_dir_all(&rustup_home)?;
        std::fs::create_dir_all(&cargo_home)?;
        let cargo_bin = bin_dir.join("cargo");
        std::fs::write(&cargo_bin, b"#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&cargo_bin, std::fs::Permissions::from_mode(0o755))?;

        let profile = crate::CargoSandboxProfile::new(
            crate::CargoExecutionMode::Inspect,
            &cargo_bin,
            &rustup_home,
            &cargo_home,
        )?;

        let binding = HostPathBinding {
            path: "/home/tester/.cargo/bin:/usr/bin".to_owned(),
            home: Some(PathBuf::from("/home/tester")),
            rustup_home: None,
            cargo_home: None,
        };

        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_cargo_profile(profile)
                .with_host_path(binding)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );

        // Das Cargo-Profil hat Vorrang: genau eine RUSTUP_HOME/CARGO_HOME-
        // Umgebung (die des Profils), keine zusätzliche Bindung/Umgebung der
        // Host-`.cargo`/`.rustup`-Verzeichnisse.
        assert_eq!(
            args.iter().filter(|a| *a == "RUSTUP_HOME").count(),
            1,
            "{args:?}"
        );
        assert_eq!(
            args.iter().filter(|a| *a == "CARGO_HOME").count(),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(&args, &["--setenv", "RUSTUP_HOME", SANDBOX_RUSTUP_HOME]),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(&args, &["--setenv", "CARGO_HOME", SANDBOX_CARGO_HOME]),
            1,
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "/home/tester/.rustup"),
            "{args:?}"
        );
        // `/home/tester/.cargo` darf nur als Einhängepunkt-Elternverzeichnis
        // (`--dir`) des gebundenen PATH-Eintrags `/home/tester/.cargo/bin`
        // auftauchen — ohne `--dir` kann bwrap den Eintrag nicht einhängen.
        // Verboten ist die Bindung des ganzen Host-`cargo_home`-Baums bzw.
        // eine konkurrierende CARGO_HOME-Umgebung.
        assert_eq!(
            count_window(&args, &["--dir", "/home/tester/.cargo"]),
            1,
            "{args:?}"
        );
        assert_eq!(
            args.iter().filter(|a| *a == "/home/tester/.cargo").count(),
            1,
            "{args:?}"
        );
        assert_eq!(
            count_window(
                &args,
                &["--ro-bind-try", "/home/tester/.cargo/bin", "/home/tester/.cargo/bin"]
            ),
            1,
            "{args:?}"
        );
        // PATH: Cargo-bin der Sandbox vorangestellt, dahinter der volle
        // Host-PATH (C1: "bei gesetztem cargo_profile: dessen bin-Dir
        // voranstellen wie heute").
        let sandbox_cargo_dir = Path::new(SANDBOX_CARGO_PATH)
            .parent()
            .ok_or(TestError::Missing("SANDBOX_CARGO_PATH parent"))?;
        let expected_path = format!(
            "{}:/home/tester/.cargo/bin:/usr/bin",
            sandbox_cargo_dir.display()
        );
        assert_eq!(
            count_window(&args, &["--setenv", "PATH", expected_path.as_str()]),
            1,
            "{args:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    // ---- Zusatzauftrag: echte Prozessidentität in der Sandbox ----

    #[test]
    fn test_with_identity_sets_identity_getter() {
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(launcher.identity(), None);
        let launcher = launcher.with_identity(7, 8);
        assert_eq!(launcher.identity(), Some((7, 8)));
    }

    #[test]
    fn plan_with_identity_sets_uid_gid_and_unshare_user_in_valid_order() -> TestResult {
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
                .with_identity(4242, 4343)
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        assert_eq!(count_window(&args, &["--uid", "4242"]), 1, "{args:?}");
        assert_eq!(count_window(&args, &["--gid", "4343"]), 1, "{args:?}");
        assert_eq!(
            args.iter().filter(|a| *a == "--unshare-user").count(),
            1,
            "{args:?}"
        );
        // bwrap(1): `--uid`/`--gid` verlangen ein explizites `--unshare-user`;
        // es muss vor beiden stehen, damit bwrap den Start nicht ablehnt.
        let unshare_user = args
            .iter()
            .position(|a| a == "--unshare-user")
            .ok_or(TestError::Missing("'--unshare-user' in args"))?;
        let uid_flag = args
            .iter()
            .position(|a| a == "--uid")
            .ok_or(TestError::Missing("'--uid' in args"))?;
        let gid_flag = args
            .iter()
            .position(|a| a == "--gid")
            .ok_or(TestError::Missing("'--gid' in args"))?;
        assert!(unshare_user < uid_flag, "{args:?}");
        assert!(unshare_user < gid_flag, "{args:?}");
        Ok(())
    }

    #[test]
    fn plan_uses_resolved_process_identity_by_default() -> TestResult {
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::default()
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        match resolve_process_identity() {
            Some((uid, gid)) => {
                assert_eq!(
                    count_window(&args, &["--uid", uid.to_string().as_str()]),
                    1,
                    "{args:?}"
                );
                assert_eq!(
                    count_window(&args, &["--gid", gid.to_string().as_str()]),
                    1,
                    "{args:?}"
                );
                assert!(args.contains(&"--unshare-user".to_owned()), "{args:?}");
            }
            None => {
                assert!(!args.contains(&"--uid".to_owned()), "{args:?}");
                assert!(!args.contains(&"--gid".to_owned()), "{args:?}");
                assert!(!args.contains(&"--unshare-user".to_owned()), "{args:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn plan_binds_etc_nss_files_for_every_profile() -> TestResult {
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::default()
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        for nss_file in ["/etc/passwd", "/etc/group", "/etc/nsswitch.conf"] {
            assert_eq!(
                count_window(&args, &["--ro-bind-try", nss_file, nss_file]),
                1,
                "{args:?}"
            );
        }
        assert_eq!(count_window(&args, &["--dir", "/etc"]), 1, "{args:?}");
        Ok(())
    }

    #[test]
    fn plan_user_and_logname_match_host_username_resolution() -> TestResult {
        let spec = execute_sandbox()?;
        let args = strings(
            &BwrapLauncher::default()
                .plan(&spec, &[OsString::from("/bin/true")])
                .map_err(TestError::Sandbox)?,
        );
        match host_username() {
            Some(name) => {
                assert_eq!(
                    count_window(&args, &["--setenv", "USER", name.as_str()]),
                    1,
                    "{args:?}"
                );
                assert_eq!(
                    count_window(&args, &["--setenv", "LOGNAME", name.as_str()]),
                    1,
                    "{args:?}"
                );
            }
            None => {
                assert!(!args.contains(&"USER".to_owned()), "{args:?}");
                assert!(!args.contains(&"LOGNAME".to_owned()), "{args:?}");
            }
        }
        Ok(())
    }
}
