# W5 — N-SBX: `NetworkMode` im bwrap-Backend (F-003, F-120)

Owned (nach Orchestrator-Entscheidung erweitert): `harw-sandbox/src/bwrap.rs`, `harw-sandbox/src/lib.rs`
(`NetworkMode`, `RelaySpec`, zwei `SandboxError`-Varianten, Re-Exporte), dieses Ledger.
BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle. Ausgeführt: `bwrap --version`,
`bwrap --help`, `cargo metadata --offline --no-deps --format-version 1` (grün). **Nicht kompiliert** — Build durch Orchestrator.

## 0. Vorgeschichte / Entscheidungen

Erster Lauf BLOCKED: bubblewrap 0.11.0 hat keinen netns-Join (`bwrap --help`: nur `--userns`, `--userns2`, `--pidns`) und
startet genau ein `COMMAND`; `sh -c` verboten. Orchestrator-Entscheidung (verbindlich):
1. **V1** — Relay im Exec-Modus `harw-netns-relay <port> <socket> -- <cmd…>` (N-EGRESS): bindet zuerst, startet cmd als Kind,
   reicht Exit-Code durch.
2. kill-on-drop über **std-Drop-Guard** `SandboxChild`, keine tokio-Dep.
3. Builder `with_network_mode(NetworkMode)`, Default `NetworkMode::None`.
4. Neue `SandboxError`-Variante(n) erlaubt.

## 1. Öffentliche API (wie geschrieben)

```rust
// lib.rs
pub use bwrap::{BwrapCommandPlan, BwrapLauncher, SANDBOX_PROXY_SOCKET_PATH, SANDBOX_RELAY_PATH, SandboxChild};
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkMode { #[default] None, ProxyOnly(RelaySpec) }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySpec { pub binary: PathBuf, pub listen_port: u16, pub proxy_socket: PathBuf }
pub enum SandboxError { …, NetworkModeNotGranted, InvalidRelaySpec { field: &'static str, reason: String } }

// bwrap.rs
pub const SANDBOX_RELAY_PATH: &str = "/run/harw/netns-relay";
pub const SANDBOX_PROXY_SOCKET_PATH: &str = "/run/harw/egress.sock";
impl BwrapLauncher {
    pub fn with_network_mode(self, mode: NetworkMode) -> Self;   // neu
    pub fn network_mode(&self) -> &NetworkMode;                  // neu
    pub fn plan(&self, &SandboxSpec, &[OsString]) -> SandboxResult<BwrapCommandPlan>;  // Signatur unverändert
    pub fn spawn(&self, &BwrapCommandPlan) -> SandboxResult<SandboxChild>;  // BRUCH: vorher std::process::Child
}
#[derive(Debug)] pub struct SandboxChild { /* child, executable, reaped */ }
impl SandboxChild {
    pub fn id(&self) -> u32;
    pub fn take_stdout(&mut self) -> Option<ChildStdout>;
    pub fn take_stderr(&mut self) -> Option<ChildStderr>;
    pub fn wait(&mut self) -> SandboxResult<ExitStatus>;   // Fehler: SandboxError::Io { path: executable, reason }
}
impl Drop for SandboxChild;  // wenn nicht reaped: kill() → wait(); Fehler tracing::warn!, nie Panic
```

`into_inner()` bewusst **nicht** angeboten (kein Aufrufer; würde die Garantie aufheben). `take_stdout/take_stderr/wait/id`
sind das Minimum, um Pipes überhaupt nutzen zu können; einziger Aufrufer von `spawn` sind derzeit Tests.

## 2. Semantik des Plans

- Immer: `--die-with-parent --new-session --unshare-all --unshare-net` (explizit, obwohl `--unshare-all` net impliziert).
  `--share-net` wird nirgends mehr erzeugt; `Permission::NetworkAccess` allein ergibt **kein** Netz.
- `ProxyOnly(spec)`:
  - ohne `NetworkAccess` → `NetworkModeNotGranted` (fail-closed, vor jedem Arg-Aufbau);
  - `listen_port == 0` / `binary` bzw. `proxy_socket` nicht absolut, ohne normale Komponente oder mit `..` → `InvalidRelaySpec`
    (`Path::components` normalisiert `.` weg; ein Relay-Vertrauens-Check à la `BWRAP_CANDIDATES` erfolgt **nicht**, weil das
    Relay i. d. R. nutzer-eigen im Build-Verzeichnis liegt — siehe §6);
  - nach `--clearenv` und HOME/PATH: `--setenv ALL_PROXY socks5h://127.0.0.1:<port>`;
  - **nach** der Workspace-Bindung (kann sie nicht überdecken): `--dir /run --dir /run/harw`,
    `--ro-bind <binary> /run/harw/netns-relay --bind <socket> /run/harw/egress.sock`; Ziele liegen auf dem tmpfs-Root der
    Sandbox, Host-Pfade werden nicht sichtbar;
  - Befehl: `-- /run/harw/netns-relay <port> /run/harw/egress.sock -- <cmd…>`.
- W1-03 unverändert: feste bwrap-Pfade, `--size` direkt vor `--tmpfs /tmp`, relative Executables in `spawn` abgelehnt.
- `spawn`: `stdin(Stdio::null())`, `stdout/stderr(Stdio::piped())`; weitere fds erbt das Kind nicht (std öffnet fds mit
  `O_CLOEXEC`). Nachkommen beendet `--die-with-parent` + PID-Namespace, wenn Drop `bwrap` tötet.
- `lo` in der neuen netns bringt bubblewrap selbst hoch (`loopback_setup` in bubblewrap-Quelle; lokal nicht ausgeführt —
  empirischer Nachweis gehört in X-PI `pi-smoke`).

## 3. dep-request (Pflicht vor Build)

`harw-sandbox/Cargo.toml` (nicht Owned): **`tracing = { workspace = true }`** (Workspace `Cargo.toml:139` `tracing = "0.1.44"`).
Benötigt für `tracing::warn!/debug!` im `Drop` von `SandboxChild` (Vorgabe „loggt Fehler“). Ohne diesen Eintrag kompiliert
harw-sandbox nicht.

## 4. Tests (bwrap.rs)

Umgeschrieben: `writable_networked_plan_requires_each_explicit_permission` — erwartet jetzt `--unshare-net`, kein `--share-net`.
Neu (prozessfrei):
- `test_with_network_mode_default_is_none`
- `test_plan_network_none_unshares_net_without_proxy` (genau ein `--unshare-net`, kein ALL_PROXY/Relay/Socket, `--die-with-parent`)
- `test_plan_proxy_only_binds_exactly_socket_and_sets_all_proxy` (Fenster `--bind <socket> /run/harw/egress.sock` genau 1×,
  Host-Socket-Pfad genau 1×, `--ro-bind <relay> /run/harw/netns-relay`, `--setenv ALL_PROXY socks5h://127.0.0.1:18080`,
  argv-Ende nach erstem `--` exakt `[relay, 18080, socket, --, /bin/echo, hi]`, ALL_PROXY nach `--clearenv`)
- `test_plan_proxy_only_without_network_access_is_denied`
- `test_plan_proxy_only_rejects_invalid_relay_spec` (Port 0, relative/`..`-Binary, relativer Socket, `/`)
- `test_plan_never_shares_net_in_any_combination` (Property-artig: 2^6 optionale Permissions × ExecuteProcess an/aus × beide
  Modi × tmpfs-Größe an/aus; jeder erfolgreiche Plan: kein `--share-net`, `--unshare-net` vorhanden, ALL_PROXY ⇔ ProxyOnly;
  Zählung 192 erfolgreiche Pläne)
Prozessabhängig (`#[ignore = "…"]` + Laufzeit-Erkennung):
- `test_sandbox_child_drop_kills_and_reaps_process` (`/bin/sleep 300` → Drop → `/proc/<pid>` weg, < 10 s) — struktureller
  kill-on-drop-Nachweis
- `test_spawn_pipes_output_and_wait_collects_status` (`/bin/echo` als „bwrap“: stdout-Pipe liefert Plan mit `--unshare-net`,
  stderr-Pipe vorhanden, `wait` erfolgreich und wiederholbar)

## 5. Aufrufer / Folgearbeit (nicht geändert)

| Datei:Zeile | Wirkung |
|---|---|
| `harw-tool-shell/src/exec.rs:334` | `launcher.plan(sandbox, &shell_command)` — kompiliert weiter; Launcher dort setzt keinen NetworkMode → `None` (vorher bei `NetworkAccess` `--share-net`). Netz-Shell braucht `with_network_mode(ProxyOnly(..))` + Relay/Socket aus N-EGRESS. |
| `harw-tool-shell/src/exec.rs:355,485-490` | eigener tokio-Spawn mit `kill_on_drop(true)`, umgeht `BwrapLauncher::spawn` — nicht betroffen vom Rückgabetyp-Bruch. |
| `harw-tool-shell/src/limits.rs:28,194,215-219,365,408-410` | `BwrapCommandPlan`/`BwrapLauncher`/`find_pinned_executable`/`plan` — API unverändert; Golden-Test `launch_command_golden_prlimit_then_bwrap_then_plan` vergleicht dynamisch gegen `plan.args()` (`:393`) → nicht gebrochen. Laufzeit-Probe `exec.rs:1177-1206` nutzt eigenes festes argv → nicht betroffen. |
| `BwrapLauncher::spawn` → `SandboxChild` | keine Produktionsaufrufer (`harw-core/src/mcp_runtime.rs` existiert nicht mehr). |
| `harw-browser-thirtyfour/**` (B-ADAPT) | künftiger Konsument von `ProxyOnly`. |
| N-EGRESS | Relay muss exakt `<port> <socket> -- <cmd…>` akzeptieren, zuerst binden, Exit-Code durchreichen. |

## 6. Annahmen / Risiken

1. `InvalidRelaySpec` zusätzlich zu `NetworkModeNotGranted` eingeführt (Brief erlaubte „z. B.“).
2. Keine Eigentümer-/Schreibrechtsprüfung des Relay-Binaries (nutzer-eigen im Target-Verzeichnis). Empfehlung für I-CLI:
   Relay aus festem Installationspfad oder neben dem Harness-Binary mit `check_pinned_executable`-analoger Regel wählen.
3. `SandboxChild::drop` blockiert bis `wait` zurückkehrt (nach SIGKILL an `bwrap` kurz). Nicht in async-Kontexten droppen.
4. `SandboxChild` hat kein `kill()`-API (keine Aufrufer); Drop ist der Kill-Pfad.
5. Formatierung manuell nach rustfmt-Defaults; `cargo fmt --check` durch Orchestrator.

## 7. Ausgabe

```json
{"agent":"N-SBX",
 "files_created":["/home/mia/projects/harwness/docs/remediation/ledger/W5/N-SBX.md"],
 "files_modified":["/home/mia/projects/harwness/harw-sandbox/src/bwrap.rs","/home/mia/projects/harwness/harw-sandbox/src/lib.rs"],
 "verification":{"command":"read-only; parent: cargo check/clippy --tests/test -p harw-sandbox nach dep-request tracing","exit_code":null,"pass":null},
 "stubbed_imports":[{"module":"tracing","reason":"dep-request: harw-sandbox/Cargo.toml tracing = { workspace = true }"},
                    {"module":"harw-netns-relay exec mode","reason":"N-EGRESS parallel"}],
 "blocked":false}
```
