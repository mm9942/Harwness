# W5 — N-EGRESS: SOCKS5-`EgressProxy` + `harw-netns-relay`

Owned: `harw-egress/src/proxy.rs` (neu), `harw-egress/src/relay.rs` (neu), `harw-egress/src/bin/harw-netns-relay.rs`
(neu), `harw-egress/src/lib.rs` (nur `mod`/`pub use`), `harw-egress/Cargo.toml` (nur Feature-/Dev-Dep-Ergänzung), dieses
Ledger. BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle. Einzige Ausführung:
`cargo metadata --offline --no-deps --format-version 1` (Ziel `harw-netns-relay` wird als `bin` erkannt, kein `[[bin]]`
nötig). Verifikation durch Lesen gegen `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/…`.
Grundlage: Plan Teil B „Egress ohne Landlock“; Ledger W3/C-EGRESS (`check_host`, `filter_resolved`, `HostLookup`).
Vertragserweiterung Orchestrator (N-SBX Option V1): argv mit `-- <cmd>` und Exit-Code-Weitergabe — eingebaut.

## 1. Öffentliche API (exakt wie geschrieben)

```rust
// lib.rs (ergänzt)
mod proxy;
mod relay;
pub use proxy::{EgressProxy, ProxyLimits, serve};
pub use relay::{
    ChildCommand, DEFAULT_RELAY_MAX_CONNECTIONS, EXIT_BIND_FAILED, EXIT_CHILD_FAILED,
    EXIT_CHILD_NOT_EXECUTABLE, EXIT_CHILD_NOT_FOUND, EXIT_USAGE, RELAY_USAGE, RelayConfig,
    RelayDirection, RelayError, RelayReporter, bind_relay, exit_code_from_status, relay_connection,
    run_child, run_relay,
};

// proxy.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyLimits {
    pub handshake_timeout: Duration,        // Default 10 s
    pub connect_timeout: Duration,          // Default 15 s (Auflösung + alle Connect-Versuche)
    pub idle_timeout: Duration,             // Default 300 s
    pub max_connections: usize,             // Default 256
    pub max_bytes_per_direction: u64,       // Default 1 GiB  (Zusatzfeld, siehe §6.2)
}
impl Default for ProxyLimits;

pub struct EgressProxy { /* Arc<Shared{policy, limits, lookup: Arc<dyn HostLookup>}> */ }
impl fmt::Debug for EgressProxy;
impl EgressProxy {
    pub fn new(policy: Arc<EgressPolicy>, limits: ProxyLimits) -> Self;                 // System-Resolver
    pub(crate) fn with_lookup(policy: Arc<EgressPolicy>, limits: ProxyLimits,
                              lookup: Arc<dyn HostLookup>) -> Self;                     // Tests
    pub fn policy(&self) -> &EgressPolicy;
    pub fn limits(&self) -> ProxyLimits;
    pub async fn run<F>(self, listener: tokio::net::UnixListener, shutdown: F) -> Result<(), EgressError>
    where F: Future<Output = ()>;
}
pub async fn serve(
    listener: tokio::net::UnixListener,
    policy: Arc<EgressPolicy>,
    limits: ProxyLimits,
    shutdown: impl Future<Output = ()>,
) -> Result<(), EgressError>;                  // = EgressProxy::new(policy, limits).run(listener, shutdown)

// relay.rs
pub const EXIT_USAGE: u8 = 64;
pub const EXIT_BIND_FAILED: u8 = 70;
pub const EXIT_CHILD_FAILED: u8 = 71;
pub const EXIT_CHILD_NOT_FOUND: u8 = 127;
pub const EXIT_CHILD_NOT_EXECUTABLE: u8 = 126;
pub const DEFAULT_RELAY_MAX_CONNECTIONS: usize = 128;
pub const RELAY_USAGE: &str;
pub type RelayReporter = Arc<dyn Fn(&RelayError) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCommand { pub program: OsString, pub args: Vec<OsString> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig { pub port: u16, pub proxy_socket: PathBuf, pub command: Option<ChildCommand> }
impl RelayConfig {
    pub fn from_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Self, RelayError>;  // ohne argv[0]
}
pub fn bind_relay(port: u16) -> Result<std::net::TcpListener, RelayError>;                   // immer 127.0.0.1
pub fn run_relay(listener: std::net::TcpListener, proxy_socket: &Path, max_connections: usize,
                 report: RelayReporter) -> Infallible;                                       // kehrt nie zurück
pub fn relay_connection(client: std::net::TcpStream, proxy_socket: &Path) -> Result<(), RelayError>;
pub fn run_child(command: &ChildCommand) -> Result<u8, RelayError>;
#[must_use] pub fn exit_code_from_status(status: ExitStatus) -> u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayDirection { ClientToProxy, ProxyToClient }        // + Display
#[derive(Debug)]
pub enum RelayError {                                          // handgeschrieben: Display, Error::source
    Usage { reason: String },
    Bind { addr: SocketAddr, source: io::Error },
    Accept(io::Error),
    ConnectionLimit { max: usize },
    ProxyConnect { path: PathBuf, source: io::Error },
    Copy { direction: RelayDirection, source: io::Error },
    Thread { source: io::Error },
    ThreadPanicked,
    Spawn { program: String, source: io::Error },
    Wait { program: String, source: io::Error },
}
impl RelayError { #[must_use] pub fn exit_code(&self) -> u8; }
```

`EgressError` (W3) blieb unverändert; Sitzungsfehler des Proxys sind private Typen (`HandshakeError`,
`ConnectFailure`, `PumpEnd`) und erscheinen nur als SOCKS-Antwortcode + tracing-Event.

## 2. Proxy-Semantik (`proxy.rs`)

**Erreichbarkeit:** nur `tokio::net::UnixListener` (kein TCP-Listen). Zugriffskontrolle = Einbinden des Sockets in die
bwrap-Sandbox; Socketpfad/-rechte setzt der Aufrufer (I-CONTRIB/N-SBX). Methode ausschließlich `0x00` (NO AUTH);
bietet der Client sie nicht an → `[05 FF]` und schließen. Version ≠ 5 → schließen ohne Antwort.

**Ablauf je Sitzung:**
1. `handshake_timeout` über Greeting + Request (+ Ablehnungsantwort). Request exakt gelesen (kein Puffer, pipelined
   Nutzdaten bleiben im Socket). `RSV ≠ 0` → `0x01`; leerer Domain-Name → `0x01`; ATYP ∉ {1,3,4} → `0x08`;
   CMD ≠ CONNECT (BIND `0x02`, UDP ASSOCIATE `0x03`, sonst) → `0x07`.
2. Ziel normalisieren: Domain-Bytes nur `[A-Za-z0-9._-]` plus `[ ] :`; dann `url::Host::parse` (derselbe WHATWG-Parser wie
   Allowlist/URLs) → `Domain` (IDNA-ASCII, klein, ein abschließender Punkt weg, keine leeren Labels) oder IP-Literal
   (`127.1`, `2130706433`, `[::1]` → IP). Sonst `0x02`, geloggt wird nur `name_len`.
3. `connect_timeout` über Prüfung + Auflösung + Verbindungsversuche:
   - **IP** (ATYP 1/4 oder IP-Literal im Domain-Feld): `check_addr` **und** `check_host(ip.to_string())`.
   - **Domain:** `check_host` **vor** jeder Auflösung → `HostLookup::lookup` (System: `tokio::net::lookup_host`) →
     Port setzen → `filter_resolved` → Kandidaten in Auflösungsreihenfolge, `TcpStream::connect` **nur** auf geprüfte
     `SocketAddr` (kein zweiter Resolve → kein Rebinding-Fenster).
4. Antwort `0x00` mit `BND.ADDR 0.0.0.0`, `BND.PORT 0` (auch bei allen Fehlern) — keine Harness-Adressen in die Sandbox.
5. Kopieren (`UnixStream::split`/`TcpStream::split`, `select!`): Leerlauf-Timer wird bei jedem Lesevorgang neu gestartet;
   jeder Schreibvorgang ebenfalls durch `idle_timeout` begrenzt; Überschreitung von `max_bytes_per_direction` beendet die
   Sitzung ohne den überschießenden Block; EOF einer Richtung → `shutdown(Write)` der Gegenseite (Half-Close), die andere
   Richtung läuft weiter.

**Antwortcodes:** `HostNotAllowed`/`LocalHostName`/`AddressDenied`/`NoPermittedAddress{denied≠[]}`/ungültiger Name →
`0x02` · `Lookup`/`NoPermittedAddress{denied=[]}`/Timeout/`HostUnreachable`/`TimedOut` → `0x04` · `ConnectionRefused` →
`0x05` · `NetworkUnreachable` → `0x03` · sonstige Connect-Fehler → `0x01`.

**Accept-Schleife:** `select!(biased; shutdown → break; join_next → Panics loggen; accept)`. Vor der Limitprüfung
`try_join_next` (fertige Sitzungen zählen nicht). Limit erreicht → Verbindung ohne Antwort schließen + `warn!`.
`accept`-Fehler → `warn!` + 100 ms Pause, weiter. Shutdown → keine neuen Verbindungen, `JoinSet::shutdown` bricht alle
Sitzungen ab, `Ok(())`.

**tracing (keine Nutzdaten):** `info!(allow_hosts, allow_private, max_connections, "gestartet")`;
`warn!(session, error, "Handshake abgelehnt")` / `"Handshake-Timeout"`; `warn!(session, host, port, reply, reason,
"Verbindung abgelehnt")` (bei ungültigem Namen `name_len` statt `host`); `info!(session, host, port, addr, "verbunden")`;
`info!(session, host, port, bytes_up, bytes_down, end, "Sitzung beendet")` (`end` ∈ `closed`, `idle-timeout`,
`byte-limit-up|down`, `io-error-up|down: <kind>`); `warn!(session, active, max_connections, "Verbindungslimit …")`;
`error!(error, "Sitzung abgestürzt")`; `filter_resolved` (W3) loggt verworfene Adressen mit Klasse.

## 3. Relay-Semantik (`relay.rs`, Binary `harw-netns-relay`)

### 3.1 argv (verbindlich)

```text
harw-netns-relay <tcp-port> <unix-socket-path>                    # reiner Weiterleitungsmodus
harw-netns-relay <tcp-port> <unix-socket-path> -- <cmd> [args…]  # Kindmodus
```

- argv[1] `<tcp-port>`: nur ASCII-Ziffern, `1..=65535` (`0`, `+80`, `65536`, Nicht-UTF-8 → Usage).
- argv[2] `<unix-socket-path>`: nicht leer und ≠ `--`; beliebiges `OsString`, nicht vorab geprüft (Verbindung je
  TCP-Verbindung).
- argv[3], falls vorhanden, **muss** exakt `--` sein; sonst Usage (keine Optionen, keine Extra-Argumente).
- Nach `--`: argv[4] = `<cmd>` (Pflicht, nicht leer), argv[5..] = Argumente **unverändert** als `OsString` (auch weitere
  `--`, Leerzeichen, `$VAR` — keine Shell, kein `sh -c`, `Command::new(cmd).args(args)`, `PATH`-Suche wie `Command`).
- Parsing nur über `std::env::args_os().skip(1)`, keine neue Dependency.

### 3.2 Ablauf und Exit-Codes

1. Argumente ungültig → stderr `harw-netns-relay: ungültige Argumente: …` + Usage, **Exit 64**.
2. `TcpListener::bind(127.0.0.1:<port>)`; Fehler → stderr, **Exit 70**. Adresse ist fest Loopback.
3. Ohne `--`: `run_relay` im Hauptthread, endet nie.
4. Mit `--`: erst **nach** erfolgreichem Bind Accept-Schleife in Hintergrundthread (`harw-relay-accept`; Thread-Start
   scheitert → Exit 71), dann Kind mit geerbter Umgebung und geerbtem stdin/stdout/stderr starten und abwarten.
   Exit = Exit-Code des Kindes; Ende durch Signal → `128 + Signal` (gekappt 255); weder noch → 1. Rückkehr aus `main`
   beendet den Prozess inkl. Relay-Threads. Readiness implizit (Kind startet erst nach Bind).
5. Kind nicht startbar: `NotFound` → **127**, `PermissionDenied` → **126**, sonst **71**; `wait`-Fehler → 71.

### 3.3 Weiterleitung

Je Verbindung ein Thread (`harw-relay-conn`) + Hilfsthread (`harw-relay-up`), `io::copy` je Richtung, nach EOF
`shutdown(Write)` der Gegenseite (Half-Close 1:1; Fehler dabei bewusst verworfen = Gegenseite schon zu). Proxy-Socket
nicht erreichbar → Client-Verbindung schließen, `ProxyConnect` an Reporter. Gleichzeitige Verbindungen begrenzt
(`DEFAULT_RELAY_MAX_CONNECTIONS` = 128, `AtomicUsize` + Drop-Guard); darüber sofort schließen + `ConnectionLimit`.
Fehler einzelner Verbindungen → `RelayReporter` → im Binary stderr. Keine Policy-Logik, kein tracing-Subscriber.

## 4. Abhängigkeiten (nur Features / Workspace-Dev-Dep; alles in Cargo.lock)

| Änderung in `harw-egress/Cargo.toml` | Grund | Lock |
|---|---|---|
| `tokio` features `["net"]` → `["net", "rt", "io-util", "time", "macros"]` | `JoinSet`/`spawn` (rt), `read_exact`/`write_all`/`split` (io-util), `timeout`/`sleep` (time), `select!` (macros) | tokio 1.53.0 |
| dev `tokio` + `"io-util", "time", "sync"` | `oneshot` im Shutdown-Test, `duplex` | tokio 1.53.0 |
| dev `tempfile = { workspace = true }` | Tempdir für Unix-Sockets | tempfile 3.27.0 (Root `[workspace.dependencies]`) |

Hinweis für den Orchestrator: der `harw-egress`-Eintrag in `Cargo.lock` bekommt beim ersten Build `tempfile` in seine
`dependencies`-Liste (keine neue Version, keine Auflösung). Kein `[[bin]]` nötig (Auto-Discovery `src/bin/*.rs`, per
`cargo metadata` bestätigt). Keine `dep-request`s.

## 5. API-Belege (Registry-Pfade)

Basis: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`

| Item | Beleg |
|---|---|
| `JoinSet::{spawn, join_next, try_join_next, shutdown, len, is_empty}` (Feature `rt`) | `tokio-1.53.0/src/task/join_set.rs:142,296,323,381,89,94` |
| `UnixStream::split`, `TcpStream::split` | `tokio-1.53.0/src/net/unix/stream.rs:995`, `src/net/tcp/stream.rs:1411` |
| `select!`: Futures werden vor dem Handler verworfen (Block um `poll_fn`) | `tokio-1.53.0/src/macros/select.rs:640-664` |
| `tokio::io::duplex` (io-util), `AsyncRead for &[u8]` | `tokio-1.53.0/src/io/util/mem.rs:104`, `src/io/async_read.rs:96` |
| Feature-Namen `rt`, `io-util`, `time`, `macros`, `net`, `sync` | `tokio-1.53.0/Cargo.toml:69-107` |
| `url::Host::<String>::parse` (`[v6]`, IDNA, `ends_in_a_number` → IPv4) | `url-2.5.8/src/host.rs:81,92-121` |
| `check_host` (pub(crate)), `filter_resolved`, `HostLookup`, `LookupFuture` | `harw-egress/src/policy.rs:236`, `src/client.rs:48-57,164` |
| `io::ErrorKind::{HostUnreachable, NetworkUnreachable}` | stabil seit Rust 1.83 (MSRV 1.85, lokal rustc 1.85.1) |
| `ExitStatusExt::{signal, from_raw}` | `std::os::unix::process` |

## 6. Annahmen / Entscheidungen (für Review)

1. **IP-Ziele brauchen zusätzlich die Allowlist** (`check_host(ip.to_string())`), nicht nur `check_addr` — wie
   `check_url` für IP-Literale. Sonst könnte Seiten-JS im Browser jede öffentliche IP erreichen. Strenger als der Brief.
2. **`ProxyLimits::max_bytes_per_direction`** als fünftes Feld für die geforderten „Byte-Limits“ (Brief nannte vier
   Felder). Default 1 GiB.
3. `serve` gibt derzeit nie `Err` zurück (kein passender `EgressError`-Variant für fatale Listener-Fehler; `error.rs`
   nicht Owned). `accept`-Fehler sind nicht fatal. Falls fatale Fehler gewünscht: Variant-Request an C-EGRESS-Nachfolger.
4. Shutdown **bricht laufende Sitzungen ab** (Sandbox-Ende = Netz weg), statt sie auslaufen zu lassen.
5. Über dem Verbindungslimit wird ohne SOCKS-Antwort geschlossen (vor dem Handshake gibt es keinen Antwortkanal).
6. Ungültiger Zielname → `0x02`; Timeout → `0x04` (nicht `0x06`).
7. Relay: Half-Close wird 1:1 durchgereicht. Schließt der Proxy (z. B. Leerlauf), sieht der Client EOF; ein Client, der
   danach nie schließt, belegt einen Relay-Platz, bis er schließt (Plätze begrenzt).
8. Exit-Test mit `/bin/true`/`/bin/false`: **nicht** `#[ignore]`, sondern Laufzeitprüfung (fehlen die Binaries, kehrt
   der Test mit stderr-Hinweis ohne Assertion zurück) — Orchestrator-Vorgabe hat Vorrang vor AGENT-BRIEF §4 („ignore +
   Laufzeit-Erkennung“); ggf. im Review auf `#[ignore]` umstellen.
9. `DEFAULT_RELAY_MAX_CONNECTIONS` ist im Binary fest (argv-Vertrag hat kein Limit-Argument).
10. rustfmt nicht ausgeführt; Zeilen ≤ 100 Zeichen (per `awk` geprüft). Kein `unsafe`, `#![forbid(unsafe_code)]` im Bin.

## 7. Tests (alle ohne externes Netz; Loopback/Unix-Sockets, Prozesse nur `/bin/true`/`/bin/false`)

**proxy.rs (19):** `test_read_greeting_detects_no_auth` · `test_read_request_parses_domain_ipv4_ipv6` ·
`test_read_request_rejects_malformed` (ATYP 9 → 0x08, leerer Name, RSV) · `test_handshake_bind_and_udp_rejected_with_0x07`
(duplex) · `test_handshake_without_no_auth_gets_0xff` · `test_normalize_domain_cases` ·
`test_checked_candidates_domain_not_allowed_is_0x02_without_lookup` (Stub zählt 0 Aufrufe) ·
`test_checked_candidates_private_ip_is_0x02` (auch allowlisted, inkl. `::ffff:10.0.0.1`, Metadaten) ·
`test_checked_candidates_public_ip_requires_allowlist` · `test_checked_candidates_rebinding_answer_is_0x02` ·
`test_checked_candidates_keeps_only_permitted_and_sets_port` · `test_connect_failure_reply_codes` ·
E2E (tempdir-`UnixListener`, TCP-Echo auf `127.0.0.1:0`, Stub-Resolver): `test_serve_end_to_end_ipv4_echo`
(`allow_private=true`) · `test_serve_end_to_end_domain_uses_checked_address` (`ECHO.test.` → Stub → 127.0.0.1, Half-Close)
· `test_serve_end_to_end_denials` (strikt: Loopback-IP, rebinding Domain, fremde Domain, ungültiger Name → 0x02 + EOF) ·
`test_serve_idle_timeout_closes_session` · `test_serve_byte_limit_closes_without_forwarding` ·
`test_serve_max_connections_rejects_extra_client` · `test_serve_shutdown_returns_ok_and_aborts_sessions`.

**relay.rs (10):** `test_from_args_forward_mode_without_separator` · `test_from_args_child_mode_passes_argv_verbatim` ·
`test_from_args_rejects_invalid_invocations` (leer, fehlender Pfad, `--` ohne cmd, leerer cmd, Extra-Argument, Port 0/65536/
`+80`, `--help`) · `test_exit_code_from_status_code_and_signal` (`from_raw`: 0, 42, SIGKILL→137, SIGTERM→143) ·
`test_run_child_forwards_exit_code` (`/bin/true`→0, `/bin/false`→1) · `test_run_child_missing_program_maps_to_127` ·
`test_bind_relay_binds_loopback_only` (Adresse = 127.0.0.1:port, zweiter Bind → `Bind`/70) ·
`test_run_relay_forwards_with_half_close` (TCP → Relay → Unix-Fake-Proxy, `ping` → `pong:ping`, Reporter leer) ·
`test_relay_connection_missing_proxy_socket_is_error` · `test_connection_slot_enforces_limit`.

## 8. Ausgabe

```json
{"agent":"N-EGRESS",
 "files_created":["/home/mia/projects/harwness/harw-egress/src/proxy.rs",
  "/home/mia/projects/harwness/harw-egress/src/relay.rs",
  "/home/mia/projects/harwness/harw-egress/src/bin/harw-netns-relay.rs",
  "/home/mia/projects/harwness/docs/remediation/ledger/W5/N-EGRESS.md"],
 "files_modified":["/home/mia/projects/harwness/harw-egress/src/lib.rs",
  "/home/mia/projects/harwness/harw-egress/Cargo.toml"],
 "verification":{"command":"read-only; cargo metadata --offline --no-deps (bin-Target erkannt); Orchestrator: cargo check/clippy --tests -D warnings/test -p harw-egress","exit_code":null,"pass":null},
 "stubbed_imports":[],
 "blocked":false}
```
