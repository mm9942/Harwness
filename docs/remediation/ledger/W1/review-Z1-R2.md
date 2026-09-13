# Review Z1-R2 — statischer Compile-Review W1 (W1-05, W1-06a, W1-06b, W1-07)

Rolle: Review-Agent Z1-R2, **READ-ONLY**. Kein Compilerlauf — dieser Bericht
ersetzt ihn. Gelesen: `git diff HEAD` der Umfangsdateien, die neuen Dateien
`harw-home/src/trust.rs` und `harw-ops/tests/approval_declaration_gate.rs`
(untracked), sowie die echten APIs auf HEAD in `harw-fsutil/src/{lib,open,
atomic,perm,walk}.rs`, `harw-sandbox/src/egress.rs`,
`harw-extension-api/src/{contributors,registry}.rs`, `Cargo.lock`, Workspace-
`Cargo.toml` (`[workspace.lints.rust] unsafe_code = "forbid"`, **keine**
clippy-Pedantic-Gruppe → nur Default-Lints gelten unter `-D warnings`).

## Befundtabelle

| ID | Schwere | Datei:Zeile | Befund | Fix-Snippet |
|---|---|---|---|---|
| Z1-R2-01 | **Blocker (Test rot)** | `harw-registry-defaults/src/embedded_agents.rs:1680-1690` | `test_planner_admits_plan_and_goal_operations` verlangt `plan`/`goal` in `planner.admitted`; W1-05 hat beide aus `agents/planner.toml` entfernt. Kompiliert, schlägt fehl. | `fn test_planner_does_not_admit_plan_or_goal_operations() { … assert!(!admitted.iter().any(\|n\| n == "plan")); assert!(!admitted.iter().any(\|n\| n == "goal")); assert_eq!(planner.return_pipeline().contract(), Some("harwness.return.plan-proposal@1")); }` |
| Z1-R2-02 | **Blocker (Test rot + P0 funktional)** | `harw-cli/src/onboarding.rs:573` | `persist_outcome` schreibt eine `file:<home>/secrets/<p>.key`-Referenz (`onboarding.rs:384`); `build_provider(&config).unwrap()` liefert jetzt `UnresolvedCredential{FILE_CREDENTIAL_NO_HOME_REASON}` → `unwrap()` paniert. | `let provider = harw_provider_http::build_provider_with_home(&config, home.path(), None).unwrap();` |
| Z1-R2-03 | **Blocker (P0 funktional)** | `harw-cli/src/chat.rs:878-879`, `harw-cli/src/gateway.rs:408-410`, `harw-cli/src/main.rs:534-535` | Kein Aufrufer nutzt `build_provider_with_home`. Damit brechen **alle** per `harw onboard` (`onboarding.rs:384`) und `harw-oauth` (`store.rs:141`) erzeugten `file:`-Credentials in `harw chat`, `harw serve` und im Gateway (dort still, `map_err(\|_\| ())`). `home` liegt in `chat.rs:875 build_model(home, …)` und `gateway.rs:395` bereits vor; `build_serve_provider` braucht einen neuen Parameter. | `chat.rs`: `Some(r) => harw_provider_http::build_provider_with_home(config, home, Some(&r)), None => harw_provider_http::build_provider_with_home(config, home, None)`; `main.rs`: `fn build_serve_provider(config: &ResolvedConfig, home: Option<&Path>, resolver: …)` + `match home { Some(h) => build_provider_with_home(config, h, resolver), None => … }` |
| Z1-R2-04 | **Hoch (Querwirkung 2)** | `harw-config/src/discovery.rs:54-84` | `ResolvedConfig::validate` ruft `ProviderToml::validate` **nicht** auf. Der neue Klartext-Header-Gate aus W1-06b ist im Produktivpfad tot; er greift nur indirekt in `configured_headers` (und dort für Nicht-Default-Provider bloß als `UnavailableProvider`). | In der Provider-Schleife nach `has_plaintext_secret`: `provider.validate()?;` |
| Z1-R2-05 | **Mittel (clippy `-D warnings`)** | `harw-config/src/discovery.rs:1277-1278` | `let mut config = ResolvedConfig::default(); config.harness = toml::from_str(…)` → `clippy::field_reassign_with_default` (style, warn-by-default). Alle **bestehenden** Vorkommen der Datei schreiben verschachtelt (`config.harness.default_provider = …`), was der Lint ignoriert; dies ist die erste direkte Feldzuweisung. | `let config = ResolvedConfig { harness: toml::from_str(r#"…"#).unwrap(), ..Default::default() }; let mut config = config;` |
| Z1-R2-06 | Niedrig | `harw-home/src/trust.rs:505-512` | `EntryType::Dir` trägt nichts zum Digest bei → das Anlegen/Löschen eines **leeren** Verzeichnisses unter `.harw` lässt den Digest unverändert. Zudem läuft `walk_beneath(&path, …)` über den **Pfad**, nicht über `harw_fd` (Moduldoku Z. 40 behauptet das Gegenteil) → TOCTOU-Fenster zwischen Walk und `read_beneath`; der Lesevorgang selbst bleibt symlinkfest. | Doku korrigieren; optional Verzeichnisnamen mit Längenpräfix in den Hash aufnehmen. |
| Z1-R2-07 | Niedrig | `harw-project-discovery/src/discovery.rs:213` | `DiscoveryConfig::default()` liest jetzt `$HOME`. Damit hängen `default_cfg()`-Tests (`test_world_writable_marker_dir_is_ignored`, `test_discover_skips_unreadable_doc_and_loads_rest`, `test_discover_skips_invalid_utf8_doc_and_loads_rest`) am ambienten `$HOME`; liegt `TMPDIR` unter `$HOME` (oder `HOME=/tmp` im Runner), verändert die Grenze den Walk. | In den drei Tests `DiscoveryConfig { home_dir: None, ..default_cfg() }` verwenden. |
| Z1-R2-08 | Niedrig | `harw-project-discovery/src/discovery.rs:1150-1170` (`test_discover_skips_unreadable_doc_and_loads_rest`) | Mode `0o000` ist für `root` lesbar → in Root-Containern lädt das Root-Dokument doch, `docs.len() == 2`, Test rot. | `if current_uid() == Some(0) { return; }` oder Test auf `#[cfg(unix)]` + Nicht-Root-Guard. |
| Z1-R2-09 | Niedrig | `harw-provider-http/src/lib.rs:667-672` | `OpenAiResponsesProvider::from_config` baut `SecretSources { home: None }` → `file:`-Referenzen in `headers` schlagen auf diesem öffentlichen Pfad auch dann fehl, wenn ein Home bekannt wäre. | Analog zu `build_provider_with_home` ein `from_config_with_home` ergänzen. |
| Z1-R2-10 | Niedrig | `harw-provider-http/Cargo.toml:10` | Über `harw-fsutil` (rustix) ist das Crate jetzt **Unix-only**; die `keyring`-Features `apple-native`/`windows-native` sind wirkungslos. Ledger-Punkt, hier nur bestätigt. | — |
| Z1-R2-11 | Info | `harw-registry-defaults/tests/tool_admission_coverage.rs:18-32, 139-142` | Moduldoku nennt weiter `PLANNING_OPERATION_TOOLS` und `plan`/`goal`. **Logik geprüft: bleibt grün** — beide Richtungen (`admitted ⊆ tool_names`, `tool_names ⊆ admitted`) und `unclaimed = ∅` gelten, weil `plan`/`goal` auf beiden Seiten entfallen. | Nur Doku. |
| Z1-R2-12 | Info | `harw-project-discovery/src/discovery.rs:174` | `DiscoveryConfig` bekommt das **öffentliche** Feld `home_dir` ohne `#[non_exhaustive]` — semver-brechend für Fremd-Literale. `grep 'DiscoveryConfig {'` im Workspace: 5 Treffer, **alle** im eigenen Testmodul und alle mit `..default_cfg()`/`..Default::default()`. Kein Aufrufer bricht. | — |
| Z1-R2-13 | Info | `harw-ops/src/config_util.rs:66`, `harw-cli/src/chat.rs:161`, `harw-cli/src/onboarding.rs:254,571` | Vollständige Aufruferliste von `config_layers` (Querwirkung 4). **Keine** Aufrufer in `harw-tui` oder `harw-install`. Kein Compile-Bruch; Verhaltensänderung: repo-lokales `.harw` entfällt ohne Freigabe, und der Repo-Layer ist jetzt **absolut** statt relativ `".harw"`. | — |
| Z1-R2-14 | Info | `harw-cli/src/mcp_auth.rs:98-110` | Eigene `file:`/`file-json:`-Auflösung ohne `<home>/secrets`-, Symlink- und Rechteprüfung — inkonsistent zur neuen Regel. | Auf einen gemeinsamen Helfer ziehen. |

## Positiv verifiziert (kein Befund)

**W1-05 / `harw-core/src/turn_loop.rs:490-510`.** Die Aggregation kompiliert:
`match decision { ApprovalDecision::Allow => {}, ApprovalDecision::AskUser(_) => … }`
bindet nirgends per Wert, `decision` wird deshalb nicht in das Match bewegt und
ist im Arm-Rumpf noch verfügbar. `ApprovalDecision` ist
`#[derive(Debug, Clone)]` ohne `non_exhaustive`
(`harw-extension-api/src/contributors.rs:334-338`) → Match erschöpfend, keine
weitere Match-Stelle im Workspace betroffen (geprüft: `harw-tui/src/approval.rs`,
`harw-cli/src/chat.rs:1254`, `harw-core/src/policy.rs`, `child_controller.rs`,
`harw-core/tests/turn_loop.rs` — alle nur konstruierend/`matches!`).
`ExtensionRegistryBuilder::approval_handler(Arc<dyn ApprovalHandler>)`
(`registry.rs:495`) nimmt `Arc<CountingApproval>` per Unsize-Coercion am
Argument. Der neue Parallel-Test ist konsistent: `preflight_approvals` bricht
beim ersten Nicht-`Allow` ab, der sequenzielle Pfad verbraucht das Aggregat und
behandelt `Deny` mit `push_tool_result(...); continue` (`turn_loop.rs:1464-1471`)
→ `TurnOutcome::Completed`, 1 Ausführung, je Handler 1 Review pro Call.

**W1-05 / registry-defaults.** `registered_tool_names`/`tool_names`/`is_read_only`
sind `pub` und liefern `Vec<&'static str>`; alle neuen Assertions typen sauber
(`Vec<&str>::contains(&&str)`, `assert_eq!(Vec<&str>, Vec<&str>)`). Die
Subset-Prüfung ist nicht vakuum: `Planning`/`Research`/`NoTools` sind
`is_read_only()`, `lens.ask` und `web.*` sind dadurch gedeckt; `status`/`ps`
kommen aus `READ_ONLY_ROOT_OPERATIONS`.

**W1-05 / `harw-ops/tests/approval_declaration_gate.rs`.** Alle benutzten Crates
sind **direkte** `[dependencies]` von `harw-ops` (`harw-extension-api:14`,
`harw-operations:2`, `harw-plan:23`, `harw-registry-defaults:34`) — ein
Integrationstest darf sie nutzen, kein Zyklus (`harw-registry-defaults` hängt
nicht an `harw-ops`). `ToolName::new(impl Into<String>)`,
`ToolCall{id,name,arguments}`, `register_all(&mut OperationRegistry)` (→ `()`),
`register_plan_tools(..) -> usize`, `PLAN_TOOL_COUNT`,
`Surface::ModelTool{readonly, approval}` mit `ApprovalPolicy: Copy` — alle
Signaturen stimmen; `surfaces.push((meta.name, *readonly, *approval))` derefert
korrekt aus `&Surface`.

**W1-06a / `harw-home`.** `blake3`, `serde`, `toml` sind im
`[workspace.dependencies]` vorhanden (`toml = "1.1.3"`), `Cargo.lock` ist für
`harw-home` aktualisiert (`toml 0.8.23` raus, `1.1.3` rein). Sämtliche
`harw-fsutil`-Importe existieren auf HEAD mit passenden Signaturen:
`open_nofollow(&Path, OpenMode) -> io::Result<File>`,
`open_dir_nofollow(&Path) -> io::Result<OwnedFd>`,
`open_beneath(BorrowedFd, &Path, OpenMode)`, `ensure_private_regular(&File)`,
`write_atomic(&Path, &[u8], AtomicWriteOptions)`, `AtomicWriteOptions::private()`,
`walk_beneath(&Path, WalkLimits) -> io::Result<WalkBeneath>` mit
`Item = io::Result<WalkEntry>`, `WalkLimits{max_depth,max_entries,deadline}`
(nicht `non_exhaustive`), `EntryType::{File,Dir,Symlink,Other}`,
`WalkStop: Debug`. `Read::take` auf dem *owned* `File` ist zulässig;
`AsFd`/`OsStrExt`/`PermissionsExt`/`MetadataExt` sind importiert. blake3
(`Hasher::new/update/finalize().to_hex()`) und toml 1.x (`from_str`,
`to_string`, `[[project]]` nach skalarem `version`) passen.
`LayerReport: PartialEq + Eq` funktioniert, weil `TrustStatus` beides ableitet.

**W1-06a / `harw-config`.** `harw-config` benutzt weiterhin **toml 0.8** (eigene
`Cargo.toml:14`), dort existieren `toml::Value` und `Value::get` ✓. `PathBuf ==
Path` in `layers.iter().any(|l| l == repo)` ist durch `impl PartialEq<Path> for
PathBuf` gedeckt. `min_positive<T: Ord + Default + Copy>` ist erfüllbar
(`Ord ⇒ PartialEq`). `ConfigError::PlaintextSecret { file, field }` existiert
unverändert (`error.rs:35`), `impl FromStr for SecretRef` ebenfalls
(`auth_toml.rs:63`). Keine let-chains → MSRV 1.85 ok.

**W1-06b / `harw-provider-http`.** `Cargo.lock` weist dem Crate **reqwest
0.12.28** zu (die 0.13.4 im Lock gehört zu einem anderen Crate) — alle benutzten
APIs (`redirect::Policy::custom`, `Attempt::{url,previous,follow,error}`,
`HeaderValue::set_sensitive`/`is_sensitive`, `bearer_auth` markiert selbst
sensitiv) sind 0.12-APIs; `default-features = false` gated `redirect` nicht.
`tempfile` ist als Dev-Dep **und** im Lock eingetragen, ebenso `harw-fsutil`
und `harw-sandbox`. Der `&dyn Fn(&str) -> Option<String>`-Parameter ist
objektsicher und höherrangig: `&env_nonempty` (fn-Item) wie `&closure`
coercen am Argument, `no_process_env`/`bound_env` werden in
`implicit_foundry_env_key_requires_matching_process_env_endpoint` korrekt vorab
zu `&dyn Fn(..)` gebunden (beide Array-Elemente haben denselben Typ, das
Destrukturieren liefert `&ProviderToml`, nicht `&&ProviderToml`).
`SecretSources<'_>` ist `Copy` (`&dyn` ist Copy). Der `ProviderToml`-Literal in
den Tests deckt genau die zehn öffentlichen Felder ab. `EgressUrl::{parse,
as_url, is_https, port, host}`, `EgressHost::{Domain, is_loopback}` existieren
in `harw-sandbox/src/egress.rs` und sind über `lib.rs:45` reexportiert;
`matches!(url.host(), EgressHost::Domain(h) if h == official_host)` typt über
`impl PartialEq<str> for String`. `super::super::http_client()` aus
`anthropic::tests` trifft die Crate-Wurzel; `ANTHROPIC_API_HOST` ist `pub(crate)`
und aus `lib.rs` als `anthropic::ANTHROPIC_API_HOST` erreichbar.

**W1-07.** `debug!` ist importiert (`discovery.rs:51
use tracing::{debug, trace, warn};`). `DiscoveryError::{Io, Utf8}` werden nicht
mehr konstruiert, lösen aber bei einem `pub enum` kein `dead_code` aus; `io`
und `Read` bleiben durch `read_capped` benutzt. `truncate_utf8_boundary ->
Option<String>` ist der einzige Aufrufer-Vertrag und wurde an beiden Stellen
umgestellt. In `is_trusted_root_candidate` ist `if let Some(uid) = … { if … }`
bewusst verschachtelt — `clippy::collapsible_if` für `if let` ist MSRV-1.88-
gegated und feuert bei `rust-version = 1.85` nicht.
`baseline.rs`: `AgentIdentity::new(impl Into<String>, impl Into<String>)` und
`with_project_root(impl Into<String>)` passen zu allen Testaufrufen; die
Template-Überschrift `## Behavioral rules` existiert (`baseline.rs:359`), und
`format!("\\u{{{:x}}}", ch as u32)` erzeugt Kleinbuchstaben-Hex, was
`"\\u{202e}"`/`"\\u{60}"`/`"\\u{2028}"` in den Assertions entspricht. Die neuen
benannten Formatargumente `cwd = cwd` spiegeln die bereits vorhandenen
`os_name = os_name`/`tools_section = tools_section` — keine neue Lint-Klasse.

## Fix-Aufgaben nach Datei

### Eigene Dateien der Knoten
- `harw-config/src/discovery.rs`
  - **Z1-R2-04** `ResolvedConfig::validate` (Z. 66-84): `provider.validate()?;` in der Provider-Schleife ergänzen.
  - **Z1-R2-05** Test bei Z. 1277: `field_reassign_with_default` vermeiden (FRU statt Feldzuweisung).
- `harw-home/src/trust.rs`
  - **Z1-R2-06** Moduldoku Z. 40 korrigieren (`walk_beneath` läuft über den Pfad, nicht über `harw_fd`); leere Verzeichnisse im Digest bewusst dokumentieren oder aufnehmen.
- `harw-project-discovery/src/discovery.rs`
  - **Z1-R2-07** drei Tests auf `home_dir: None` festnageln.
  - **Z1-R2-08** Root-Guard im 0o000-Test.

### Fremde Dateien (nicht im Schreibbereich der Knoten)
- `harw-registry-defaults/src/embedded_agents.rs:1680-1690` — **Z1-R2-01** (Blocker).
- `harw-registry-defaults/tests/tool_admission_coverage.rs:18-32,139-142` — **Z1-R2-11** (nur Doku).
- `harw-cli/src/onboarding.rs:573` — **Z1-R2-02** (Blocker).
- `harw-cli/src/chat.rs:878-879`, `harw-cli/src/gateway.rs:408-410`, `harw-cli/src/main.rs:534-535` (+ neuer `home`-Parameter in `build_serve_provider`) — **Z1-R2-03** (Blocker).
- `harw-cli/src/mcp_auth.rs:98-110` — **Z1-R2-14**.
- `harw-provider-http/src/lib.rs:667-672` (`from_config`) — **Z1-R2-09**.
- Doku-Leichen „erste Nicht-`Allow`-Entscheidung gewinnt“: `harw-cli/src/chat.rs:123-125`, `harw-tui/src/app.rs:303-307,1444-1450,1502-1505`, `harw-tui/src/approval.rs:313-316` (harmlos, Aussage „kann nur einschränken“ stimmt jetzt).

## Kompiliert voraussichtlich

| Crate | `cargo check` | `cargo clippy -D warnings` | `cargo test` |
|---|---|---|---|
| `harw-registry-defaults` | **ja** | ja | **nein** — Z1-R2-01 |
| `harw-core` | **ja** | ja | ja |
| `harw-ops` | **ja** | ja | ja (neuer Integrationstest typt) |
| `harw-home` | **ja** | ja | ja |
| `harw-config` | **ja** | **nein** — Z1-R2-05 | ja |
| `harw-provider-http` | **ja** | ja | ja |
| `harw-project-discovery` | **ja** | ja | ja (mit Vorbehalt Z1-R2-07/08) |
| `harw-instructions` | **ja** | ja | ja |
| `harw-cli` | **ja** | ja | **nein** — Z1-R2-02 |

Hinweis: `cargo …` ohne `--locked` nötig ist nicht mehr erforderlich — `Cargo.lock`
ist für `harw-home`, `harw-provider-http` (inkl. `tempfile`) und die übrigen
`harw-fsutil`-Neuzugänge bereits aktualisiert (Ledger-Punkt W1-06b §5.3 ist
erledigt).
