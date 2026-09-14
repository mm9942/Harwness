# W5 — B-ADAPT: `harw-browser-thirtyfour` gepinnt, sandboxed, Origin nach jeder Aktion (F-009)

Rolle: focused-coding-task (Opus). Owned: `harw-browser-thirtyfour/**`, dieses Ledger.
BUILD-POLICY eingehalten: kein cargo build/check/test/clippy/run/add, keine Prozesse, keine git-Schreibbefehle.
Ausgeführt: Lesen, grep, `cargo metadata --offline --no-deps --format-version 1` (OK), eine Doku-Abfrage
(geckodriver-Flags). Verifikation durch Lesen gegen `~/.cargo/registry/src/index.crates.io-*/`.

Pflichtlektüre gelesen: AGENT-BRIEF, Plan W5 B-ADAPT + „Egress ohne Landlock“, Register F-009, Ledger
W3/C-BROWSER, W3/C-EGRESS, W5/N-SBX (anfangs BLOCKED, inzwischen umgesetzt: `NetworkMode`, `RelaySpec`,
`with_network_mode`), W5/N-EGRESS (Relay-argv mit `-- <cmd>`).

## 1. Dateien

| Datei | Änderung |
|---|---|
| `Cargo.toml` | thirtyfour-Feature **`manager` entfernt** (`WebDriver::managed` existiert damit nicht mehr); direkte Dep `reqwest = { version = "0.13.4", default-features = false }` (Version im Lock, identisch zu thirtyfours reqwest) für proxy-freien Client |
| `src/launcher.rs` (neu) | `GeckodriverPin` (abs. Pfad + SHA-256), `verify()` → `VerifiedGeckodriver`, `DriverPorts`, `GeckodriverCommand`, `RelayEndpoint`, `PreparedLaunch`, Traits `BrowserLauncher`/`DriverProcess`, `LaunchedDriver`, `validate_prepared_launch`, `launch_pinned_driver`; 8 Tests |
| `src/firefox_prefs.rs` (neu) | `PrefValue`, `hardened_preferences(proxy_port)`; 3 Tests |
| `src/location_guard.rs` (neu) | `LocationProbe` (crate-privat), `enforce_location_policy`; 4 Tests mit Fake-Probe |
| `src/journal.rs` | Deckel nach Einträgen **und** Bytes, Feldkürzung, `EventJournal` (crate-privat) mit Verdrängung; 5 Tests |
| `src/config.rs` | `managed_driver` entfernt; `with_geckodriver_pin`, `with_launcher`, `with_journal_policy` + Getter; fail-closed Default; 2 Tests |
| `src/driver.rs` | `start(plan, LaunchedDriver)`: `WebDriver::builder(url).client(reqwest no_proxy, redirect none)`, Session-Start-Deadline 60 s, gehärtete Prefs; hält `Box<dyn DriverProcess>`, `quit(&mut self)` gibt ihn frei (Kill) |
| `src/host.rs` | `validate_open_request` → `request.validate()`; `open`: Pin + Launcher Pflicht, `launch_pinned_driver`, Start-URL-Landeort via `check_observed_location`; `FirefoxBindingMetadata.managed_driver_available` → `pinned_driver_required`; `let _ =` auf close/quit durch geloggte Fehler ersetzt |
| `src/runtime.rs` | hält `OpenBrowserRequest` + `Mutex<ActionBudget>` + `Mutex<EventJournal>`; `request()`, `consume_action_budget()`, `enforce_location_policy()`, `events_since()`, `last_event_cursor()`; `impl LocationProbe` (alle Fenster: `windows()` → `switch_to_window` → `current_url`; Abbruch = `close_runtime`) |
| `src/runtime_impl.rs` | `act`: `request.validate(limits)`, `check_navigation_target`, Budget, danach **immer** Location-Guard (auch bei Aktionsfehler); `wait`: `condition.validate`/`timeout.validate`, danach Guard; `observe`/`find`: Selektor-/Target-Validierung, Guard vor Rückgabe; `Upload`-Arm entfernt |
| `src/navigation.rs` | `origin_policy().is_allowed` → `request().check_navigation_target` |
| `src/element_actions.rs` | `BrowserAction::Upload` entfernt |
| `src/waits.rs` | `WaitCondition::CustomScript` entfernt |
| `src/observe.rs` | Event-Cursor über `last_event_cursor()` |
| `src/error.rs` | Varianten `DriverPin { path, detail }`, `Launch { detail }` + Display + Mapping → `CapabilityUnavailable` |
| `src/lib.rs` | `mod wait_script` entfernt; `pub mod launcher`, `pub mod firefox_prefs`, `mod location_guard`; Re-Exporte |
| `tests/*.rs` | `OpenBrowserRequest` mit `authentication_origins`/`limits`, `OriginPolicy::from_origins(["https://…"])`, `managed_driver` entfernt; neuer Test `test_open_without_pinned_geckodriver_fails_closed_before_any_process_start` |

**Orchestrator-Aktion:** `git rm harw-browser-thirtyfour/src/wait_script.rs` (Modul nicht mehr eingebunden; Datei
bleibt bis dahin unkompiliert auf der Platte, Leeren war untersagt).

## 2. Öffentliche API (neu/geändert)

```rust
// launcher
pub const NETNS_RELAY_BINARY_NAME: &str = "harw-netns-relay";
pub const MAX_GECKODRIVER_BYTES: u64 = 256 MiB; pub const GECKODRIVER_LISTEN_HOST: &str = "127.0.0.1";
pub struct GeckodriverPin;  impl { new(PathBuf, &str) -> Result<Self, AdapterError>; path(); sha256_hex(); verify() -> Result<VerifiedGeckodriver, AdapterError> }
pub struct VerifiedGeckodriver; impl { path(); sha256_hex() }            // nur via verify()
pub struct DriverPorts { pub webdriver: u16, pub bidi: u16 }
pub struct GeckodriverCommand; impl { new(VerifiedGeckodriver, DriverPorts) -> Result<..>; geckodriver(); ports(); argv() -> &[OsString] }
pub struct RelayEndpoint { pub binary: PathBuf, pub listen_port: u16, pub proxy_socket: PathBuf }   // = harw_sandbox::RelaySpec
pub struct PreparedLaunch { pub argv: Vec<OsString>, pub webdriver_url: url::Url, pub relay: RelayEndpoint }
pub trait DriverProcess: Send + Sync + Debug { fn id(&self) -> Option<u32>; }                      // Drop MUSS Sandbox beenden
#[async_trait] pub trait BrowserLauncher: Send + Sync + Debug {
    fn driver_ports(&self) -> DriverPorts;
    fn prepare(&self, &GeckodriverCommand) -> Result<PreparedLaunch, AdapterError>;
    async fn spawn(&self, &PreparedLaunch) -> Result<Box<dyn DriverProcess>, AdapterError>;       // resolve erst bei Erreichbarkeit
}
pub struct LaunchedDriver; impl { prepared(); webdriver_url(); proxy_port(); into_parts() }
pub fn validate_prepared_launch(&GeckodriverCommand, &PreparedLaunch) -> Result<(), AdapterError>;
pub async fn launch_pinned_driver(&dyn BrowserLauncher, &GeckodriverPin) -> Result<LaunchedDriver, AdapterError>;
// firefox_prefs
pub enum PrefValue { Bool(bool), Int(i64), Str(&'static str) }
pub const RELAY_PROXY_HOST: &str; pub const DISABLED_DOWNLOAD_DIR: &str;
pub fn hardened_preferences(proxy_port: u16) -> Vec<(&'static str, PrefValue)>;
// journal
pub const DEFAULT_JOURNAL_CAPACITY = 1_024; DEFAULT_JOURNAL_MAX_BYTES = 1 MiB; HARD_MAX_JOURNAL_BYTES = 16 MiB;
pub const HARD_MAX_JOURNAL_CAPACITY = 65_536; MAX_EVENT_FIELD_BYTES = 8 KiB;
impl EventJournalPolicy { bounded(usize) /* jetzt auch ≤ HARD */; with_max_bytes(usize) -> Result<Self>; max_bytes() }
// config (FirefoxHostConfig) – ENTFERNT: with_managed_driver, managed_driver
with_geckodriver_pin(GeckodriverPin); with_launcher(Arc<dyn BrowserLauncher>); with_journal_policy(EventJournalPolicy);
geckodriver_pin() -> Option<&GeckodriverPin>; launcher() -> Option<Arc<dyn BrowserLauncher>>; journal_policy() -> Result<EventJournalPolicy>
// host
FirefoxBindingMetadata { …, pinned_driver_required: bool }   // WAR managed_driver_available
// error
AdapterError::DriverPin { path: PathBuf, detail: String }, AdapterError::Launch { detail: String }
```

`FirefoxHost::new(FirefoxHostConfig)` unverändert (Aufrufer `harw-registry-defaults/src/profile.rs:720` kompiliert weiter,
`open` liefert dort bis I-CONTRIB `CapabilityUnavailable`).

## 3. Semantik

**Start (`open`):** `request.validate()` → Pin fehlt / Launcher fehlt → `CapabilityUnavailable` (kein Prozess) →
Capability-Plan → `launch_pinned_driver`: SHA-256 auf Blocking-Pool (kein Symlink, reguläre Datei, ≤ 256 MiB, unter Unix
nicht group/world-writable, gestreamt, Vergleich über volle Länge) → `GeckodriverCommand`
`[<pfad>, --host 127.0.0.1, --port P, --websocket-port W, --allow-hosts 127.0.0.1, --allow-origins http://127.0.0.1:P, --log warn]`
→ `launcher.prepare` → **Audit** `validate_prepared_launch`: Programm absolut; kein `--share-net`; `--unshare-net` oder
`--unshare-all` vorhanden; argv endet mit dem unveränderten geckodriver-argv (und ist länger); `webdriver_url` =
`http://127.0.0.1:P/` bzw. `[::1]`, ohne Credentials/Pfad/Query; Relay-Binary absolut mit Dateiname `harw-netns-relay`;
Relay-Port ≠ 0, ≠ P, ≠ W; Proxy-Socket absolut → erst dann `spawn`. WebDriver-Client: `reqwest::Client::builder()
.no_proxy().redirect(none).timeout(60 s)`, Session-Handshake-Deadline 60 s (`DriverTimeout`). Nach `goto(start_url)`:
`current_url` → `check_observed_location`; Verstoß → quit + Fehler.

**Firefox-Prefs:** SOCKS5 `127.0.0.1:<relay>` mit `socks_remote_dns` (socks5h), `no_proxies_on=""`,
`allow_hijacking_localhost=true` (auch localhost über Proxy → geckodriver/Marionette im netns nicht aus Seiten erreichbar),
`failover_direct=false`, TRR aus, Prefetch/Predictor/Speculative aus, WebRTC aus, Captive-Portal/Connectivity aus;
Telemetrie/Health-Report/Studies/Normandy/Crash-Submit aus; App-/Extension-/Suchmaschinen-Updates, Safe-Browsing-Updates
aus; Downloads in nicht existentes Verzeichnis, `forbid_open_with`, keine Auto-Handler; `file://`:
`security.fileuri.strict_origin_policy`, `privacy.file_unique_origin`.

**F-009 (Origin-Nachkontrolle):** nach `act` (auch wenn die Aktion fehlschlug), `wait`, `observe`, `find` liest die Runtime
die URL **aller** Top-Level-Fenster (erfasst Redirects, Klick-/Submit-/JS-Navigation, `window.open`) und prüft
`check_observed_location` (allowed ∪ authentication). Verstoß **oder** nicht lesbare Locations → Session abbrechen
(`close_runtime`: closed-Flag, WebDriver-Quit, Prozess-Handle freigeben) und Fehler statt Inhalt. `about:blank`, `data:`,
`file:`, Loopback werden damit ebenfalls abgebrochen (keine Ausnahme nach dem Start).

**Limits:** `ActionBudget` je Session (`try_consume` vor jeder Aktion), `ActionRequest::validate`, `WaitCondition::validate`,
`WaitTimeout::validate` (≤ `max_wait_ms`), `validate_target`/`validate_selector` für Find/Observe.

**Journal:** Einträge ≤ `capacity`, geschätzte Bytes (Feldlängen + serialisiertes Payload + 128 B Overhead) ≤ `max_bytes`;
Strings auf 8 KiB (UTF-8-Grenze) gekürzt, Script-Payload > 8 KiB durch Marker ersetzt. Voll: Critical verdrängt älteste
Nicht-Critical-, dann älteste Einträge (vorher unbeschränktes Wachstum bei Critical-Flut behoben); andere Klassen werden in
`BackpressureStats` gezählt, nicht gespeichert.

**`time` → `jiff`:** in `harw-browser-thirtyfour` gibt es keine `time`-Nutzung und keine `time`-Dep (Lock-Eintrag
`harw-browser-thirtyfour` ohne `time`). Die einzige Browser-Nutzung ist `harw-browser/src/event.rs:159`
(`EventEnvelope.timestamp: time::OffsetDateTime`) — nicht in B-ADAPT-Zuständigkeit → Folgearbeit W13/Eigentümer harw-browser.

## 4. Belege (Registry / Doku)

| Item | Beleg |
|---|---|
| `manager`-Feature gated `WebDriver::managed` | `thirtyfour-0.37.2/Cargo.toml` `[features] manager`, `src/web_driver.rs:168,251,268` `#[cfg(feature = "manager")]` |
| `WebDriver::builder(url, caps)`, `.client(impl HttpClient)`, `.connect()` | `thirtyfour-0.37.2/src/web_driver.rs:104,331-439` |
| `impl HttpClient for reqwest::Client` (reqwest 0.13, `default-features = false`) | `thirtyfour-0.37.2/src/session/http.rs:56`, `Cargo.toml:288-292` |
| BiDi nutzt `webSocketUrl` aus der Session | `thirtyfour-0.37.2/src/bidi/capabilities.rs:18`, `web_driver.rs:204` |
| `FirefoxPreferences::{new,set}`, `FirefoxCapabilities::set_preferences` | `thirtyfour-0.37.2/src/common/capabilities/firefox.rs:89,174,180`; Pfad `thirtyfour::common::capabilities::firefox` (`lib.rs:214`, `capabilities/mod.rs:10`) |
| `windows()`, `current_url() -> Url`, `switch_to_window` | `thirtyfour-0.37.2/src/session/handle.rs:258,519`, `src/switch_to.rs:150` |
| `ClientBuilder::{no_proxy,redirect,timeout,build}`, `pub mod redirect` | `reqwest-0.13.4/src/async_impl/client.rs:1430,1386,1444,407`, `src/lib.rs:380` |
| geckodriver `--host --port --websocket-port --allow-hosts --allow-origins --log` | https://firefox-source-docs.mozilla.org/testing/geckodriver/Flags.html |
| `sha2 0.10.9` bereits Dep + im Lock | `Cargo.lock` Paket `harw-browser-thirtyfour` |
| `OpenBrowserRequest::{validate,check_navigation_target,check_observed_location}`, `ActionBudget`, `validate_target/selector`, `WaitCondition/WaitTimeout::validate` | `harw-browser/src/policy.rs:1039-1073`, `action.rs:277,308,464,490-528`, `wait.rs:102,230` |
| `RelaySpec`, bwrap-Befehl `-- /run/harw/netns-relay <port> /run/harw/egress.sock -- <cmd…>`, `--unshare-all` | `docs/remediation/ledger/W5/N-SBX.md:24-64`; Relay-argv `N-EGRESS.md:141-159` |

Lock-Auswirkung: Wegfall von `manager` kann `dirs`/`fs4`/`tar`/`flate2` aus thirtyfours Lock-Abhängigkeiten entfernen;
neue Kante `harw-browser-thirtyfour → reqwest 0.13.4` (vorhandene Version). Kein `dep-request`.

## 5. Tests (ohne Prozessstart)

- Hash: `launcher::test_geckodriver_pin_verify_wrong_hash_is_error`, `…_accepts_matching_hash`,
  `…_rejects_symlink_and_world_writable`, `test_geckodriver_pin_new_rejects_relative_path_and_bad_digest`.
- Kommandozeile/Launcher: `test_geckodriver_command_new_builds_loopback_argv_and_rejects_bad_ports`,
  `test_validate_prepared_launch_accepts_isolated_command_without_share_net`,
  `test_validate_prepared_launch_rejects_unsafe_command_lines` (share-net, kein unshare, manipulierte argv, kein Wrapper,
  Nicht-Loopback-URL, falscher Port, falsches Relay, Port-Kollision, relatives Programm),
  `test_launch_pinned_driver_never_spawns_share_net_or_wrong_hash` (Fake-Launcher zählt Spawns = 0).
- Prefs: `firefox_prefs::test_hardened_preferences_routes_everything_through_socks5h_relay`,
  `…_disables_telemetry_updates_downloads_and_file_access`, `…_names_are_unique`.
- Origin nach Aktion: `location_guard::test_enforce_location_policy_redirect_outside_policy_aborts_session`,
  `…_file_and_blank_locations_abort_session`, `…_unreadable_locations_fail_closed`, `…_allows_policy_and_auth_origins`.
- Journal: `journal::test_event_journal_push_caps_entries_for_critical_flood`, `…_caps_retained_bytes`,
  `…_truncates_oversized_fields`, `…_critical_evicts_non_critical_first`, `test_event_journal_policy_bounded_rejects_zero_and_oversize`.
- Config/Host: `config::test_firefox_host_config_new_is_fail_closed`, `…_with_geckodriver_pin_is_retained`,
  `tests/adapter_contract.rs::test_open_without_pinned_geckodriver_fails_closed_before_any_process_start`.
- Dateisystem: Hash-Tests nutzen Tempdirs unter `std::env::temp_dir()` (Muster wie `profile_archive.rs`).

Fake-Driver-Trait existierte nicht; die Origin-Durchsetzung ist deshalb hinter `LocationProbe` gekapselt und mit Fake getestet.
Die Verdrahtung `act/wait/observe/find → enforce_location_policy` ist nur per Lesen verifiziert (braucht echten Driver).

## 6. Offene Punkte / Folgearbeit

1. **Blocker für den Live-Betrieb (I-CONTRIB + N-SBX/N-EGRESS): Rückkanal Harness → geckodriver.** Mit `--unshare-net`
   lauscht geckodriver nur im Sandbox-netns; der Harness im Host-netns erreicht `127.0.0.1:P` nicht. Der Relay ist nur
   ausgehend (TCP im netns → Unix-Socket). Nötig: eingehender Forwarder (Host-Loopback `127.0.0.1:P` und `:W` → Unix-Socket
   → im netns `127.0.0.1:P/W`) mit **identischen Portnummern** (sonst bricht der von geckodriver gemeldete `webSocketUrl`).
   Der `BrowserLauncher`-Vertrag verlangt genau das (`webdriver_url` Port = P, `spawn` löst erst bei Erreichbarkeit auf).
   Host-Loopback-Listener sind für andere lokale Prozesse erreichbar → Forwarder sollte zufällige Ports und geckodrivers
   `--allow-origins`/`--allow-hosts` nutzen; besser Peer-Prüfung (SO_PEERCRED) über Unix-Socket.
2. **Konkreter Launcher (I-CONTRIB):** `impl BrowserLauncher` über `harw_sandbox::BwrapLauncher::with_network_mode(
   NetworkMode::ProxyOnly(RelaySpec{..}))`; der verifizierte geckodriver muss **unter identischem Pfad** read-only in die
   Sandbox gebunden werden (Audit verlangt argv-Suffix mit Host-Pfad), Firefox-Binary und `/usr`/`/lib` ebenso;
   `DriverProcess` = `tokio::process::Child` mit `kill_on_drop` + `--die-with-parent`. `harw-browser-thirtyfour` hat
   bewusst keine `harw-sandbox`-Dep.
3. **Konfiguration (I-CONTRIB):** `BrowserSection::{geckodriver_path, geckodriver_sha256}` → `GeckodriverPin::new`,
   `BrowserSection::max_actions` → `BrowserLimits::with_max_actions_per_session` (Grant, B-TOOL);
   `harw-registry-defaults/src/profile.rs:720` nutzt `FirefoxHostConfig::default()` → `open` fail-closed bis dahin.
4. **TOCTOU Hash→Exec:** Datei kann zwischen Prüfung und Start getauscht werden. Mitigation hier: Symlink- und
   Schreibrechte-Prüfung; Betriebsauflage: Pfad in root-eigenem, nicht vom Harness-User beschreibbaren Verzeichnis
   (D-DEPLOY). Stärker wäre Kopie in ein privates Staging-Verzeichnis oder Exec über fd.
5. **Downloads/`file://`** sind per Prefs nur neutralisiert, nicht hart gesperrt; harte Grenze = schreibgeschützte Sandbox
   ohne Host-Dateien (I-CONTRIB) + Origin-Abbruch bei `file:`-Location.
6. **Verhalten:** Location-Lesefehler (z. B. Popup schließt während der Prüfung) brechen die Session ab (fail closed);
   `Back` auf die initiale `about:blank`-History bricht ebenfalls ab. Asynchrone Navigationen nach Rückkehr einer Aktion
   werden bei der nächsten Operation erkannt (Observe/Find/Wait prüfen vor Rückgabe).
7. `install_page_bridge` (Connector-Skript, nicht modellgesteuert) unverändert; `bidi_pump.rs` verwirft
   `append_event`-Ergebnisse weiterhin mit `let _ =` (vorbestehend; nach Abbruch erwartete `SessionNotFound`).
8. Pref-Namen nicht einzeln gegen die aktuelle Firefox-Version geprüft (unbekannte Prefs ignoriert Firefox); Nachweis im
   Live-Test/X-PI.
9. `rustfmt` nicht ausgeführt; einige Testzeilen > 100 Zeichen → `cargo fmt` durch Orchestrator.

## 7. Ausgabe

```json
{"agent":"B-ADAPT",
 "files_created":["/home/mia/projects/harwness/harw-browser-thirtyfour/src/launcher.rs",
  "/home/mia/projects/harwness/harw-browser-thirtyfour/src/firefox_prefs.rs",
  "/home/mia/projects/harwness/harw-browser-thirtyfour/src/location_guard.rs",
  "/home/mia/projects/harwness/docs/remediation/ledger/W5/B-ADAPT.md"],
 "files_modified":["harw-browser-thirtyfour/Cargo.toml","src/{lib,config,driver,host,runtime,runtime_impl,navigation,element_actions,waits,observe,journal,error}.rs",
  "tests/{adapter_contract,bidi_api_contract,profile_contract,page_bridge_live_contract,runtime_trait_contract}.rs"],
 "files_to_delete":["harw-browser-thirtyfour/src/wait_script.rs (git rm durch Orchestrator)"],
 "verification":{"command":"read-only; cargo metadata --offline --no-deps OK; Orchestrator: cargo fmt, cargo clippy --tests -D warnings -p harw-browser-thirtyfour, cargo test -p harw-browser-thirtyfour","exit_code":null,"pass":null},
 "stubbed_imports":[{"module":"BrowserLauncher-Implementierung","reason":"I-CONTRIB (harw-sandbox ProxyOnly + eingehender WebDriver-Forwarder)"}],
 "blocked":false}
```
