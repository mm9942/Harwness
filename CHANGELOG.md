# Changelog

All notable changes to this workspace are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/) and this project uses
Semantic Versioning within the 0.x pre-release range.

## [Unreleased]

**Neuer Befehlsbaum der Kommandozeile (`harw`)**
- Befehle nach Aufgaben geordnet: `chat`, `exec`, `analyze`, `session`,
  `config`, `provider`, `model`, `auth`, `project`, `agent`, `knowledge`,
  `jobs`, `gateway`, `serve`, `web`, `service`, `mcp`, `channel` sowie die
  System-Befehle. Vollständige Referenz mit Zuordnung alt → neu in
  [`docs/cli.md`](docs/cli.md).
- Neu: `harw exec PROMPT…` für eine echte einmalige Anfrage ohne Oberfläche;
  `harw session list [--all] | show ID | resume ID`; `harw provider
  list|add|remove|enable|disable|scan`; `harw model catalog [--refresh]`;
  `harw agent uia-new|skills|plugins`; `harw knowledge index|memory|proposals`;
  `harw jobs list|show|approve [--note]|deny [--reason]|cancel|retry`;
  `harw channel connect telegram [--pair CODE]`; `harw debug echo|classify`.
  Skills, Plugins, Gedächtnis, Kontext-Vorschläge und Aufträge sind damit
  auch außerhalb des Chats erreichbar.
- Neue globale Flags `--profile NAME`, `-C/--cwd DIR` und `--json`; `-v` als
  Kurzform von `--verbose`. Befehle ohne JSON-Form brechen bei `--json` mit
  einer Fehlermeldung ab, statt das Flag still zu ignorieren.
- Neue Sitzungs-Flags `--approval ask|auto|full` und `--model ID`. Sitzungs-
  Flags (`--mode`, `--approval`, `--model`, `--goal`, `--add-dir`) wirken nur
  bei `chat`, `exec` und `analyze`; bei anderen Befehlen meldet `harw` einen
  Fehler, statt sie still zu ignorieren.
- `harw analyze --order bottom-up|top-down` ersetzt `--bottom-up`/`--top-down`
  (beide weiterhin versteckt gültig, aber nicht miteinander oder mit
  `--order` kombinierbar).
- Umbenannt: `settings` → `config`, `models` → `model` (alte Namen bleiben
  Aliase), `models delete` → `model remove` (Alias `delete`). Ältere
  Schreibweisen `connect`, `lens`, `uia`, `catalog`, `run` und `classify`
  funktionieren weiter, sind aber versteckt und verweisen per Hinweis auf den
  neuen Befehl.

### Added

- **`harw project` subcommand**: `harw project trust [DIR]`, `harw project untrust [DIR]`,
  and `harw project status [DIR]` manage a project's trust record explicitly (`DIR`
  defaults to the current directory). A project's repo-local `.harw` configuration
  layer is now loaded only for projects that have been explicitly trusted this way.

**Shell-Completions (`harw completions`)**
- Neuer Befehl `harw completions [SHELL] [--install|--uninstall] [--dry-run]`
  (bash, zsh, fish, elvish, powershell; ohne `SHELL` wird die Shell aus
  `$SHELL` erkannt). Ohne Flag wird das Skript wie bisher auf stdout
  ausgegeben. `--install` schreibt es an den kanonischen Ort (zsh:
  `$ZSH_CUSTOM/completions/_harw` bei oh-my-zsh, sonst
  `~/.local/share/zsh/site-functions/_harw` plus verwalteter `fpath`-Block in
  `.zshrc` vor `compinit`; bash:
  `~/.local/share/bash-completion/completions/harw`; fish:
  `~/.config/fish/completions/harw.fish`) und ersetzt dabei ältere
  harw-verwaltete Installationen samt veralteter Pfade, rc-Blöcke und
  `.zcompdump*`-Caches, statt sie zu stapeln. `--uninstall` entfernt all das,
  `--dry-run` zeigt nur an, was geschehen würde. `--all-binaries` ruft
  zusätzlich `completions` für jedes auf `$PATH` gefundene DoD-Binary auf.
  `harw completion` bleibt als verstecktes Alias erhalten.
- Neue Crate `harw-completions`: Skripterzeugung mit Verwaltungsmarker,
  Ortsauflösung pro Shell, Installation/Deinstallation sowie die
  wiederverwendbaren clap-Argumente (`CompletionsArgs`,
  `CompletionsSubcommand`, Feature `clap-args`).
- Die DoD-Binaries `harw-sentinel`, `harw-warden`, `harw-probe-fs` und
  `harw-probe-bpf` haben einen Unterbefehl `completions` mit denselben
  Optionen erhalten; er läuft vor jedem Sensor-, Socket- oder
  Landlock-Start.

**Freigaben, Regeln und Lebensdauern (Scopes)**
- Neuer `SettingScope`-Typ (`harw-config/src/scope.rs`) für die drei Lebensdauern
  einer Einstellung: `Session` (nur im Speicher), `Project` (dauerhaft pro Projekt,
  autoritätsgewährend außerhalb des Repos unter
  `~/.harw/profiles/<profil>/projects/<schlüssel>/`) und `Global` (dauerhaft für
  den User, `~/.harw/config.toml`). Bei einem Modus-Konflikt gewinnt die höhere
  Präzedenz (`Session > Project > Global`).
- `PermissionsSection`/`RuleToml` (`harw-config/src/permissions_toml.rs`) und ein
  neuer `ConfigWriter` (`harw-config/src/writer.rs`, `toml_edit`, atomares
  Schreiben, `.bak.<n>`-Backup-Rotation) zum dauerhaften Setzen von
  Freigabemodus, Freigabe-Timeout, Allow-/Deny-Regeln und zusätzlichen
  Arbeitswurzeln, ohne bestehende Kommentare oder Formatierung der Datei zu
  verlieren. `ConfigWriter::save` validiert vor dem Schreiben und lässt die
  Datei bei einem ungültigen Wert unangetastet.
- `AllowRuleSet`/`ApprovalRule` (`harw-extension-api/src/allow_rules.rs`):
  Allow-/Deny-Regeln je Werkzeug mit `RuleScope` (Session/Project/Global). Eine
  passende Deny-Regel gewinnt scope-übergreifend immer über jede Allow-Regel.
  Regeln für `shell.exec` vergleichen ganze Befehls-Tokens als Präfix und
  greifen als Allow-Regel nie bei einem zusammengesetzten Befehl (`;`, `&&`,
  `||`, `|`, Backtick, `$(`, Umleitung, Zeilenumbruch, Hintergrundjob); Regeln
  für `fs.*`-Werkzeuge werten ein Pfad-Glob aus und greifen als Allow-Regel nie
  bei einer `..`-Pfadkomponente. `derive_shell_rule` leitet aus einem
  tatsächlich ausgeführten Befehl einen konservativen Regel-Vorschlag ab und
  verweigert das für eine feste Liste breiter Interpreter/Wrapper (`bash`,
  `python3`, `sudo`, `xargs`, `eval` u. a.).
- Diese Regeln sind in die tatsächliche Freigabeentscheidung verdrahtet:
  `DefaultApprovalPolicy::review` (`harw-registry-defaults/src/lib.rs`) befragt
  die geteilte `AllowRuleSet` vor der Modus-Logik; eine Deny-Regel fragt immer
  nach, auch im `full`-Modus. `RuntimeAssembly` sät Modus, Allow-/Deny-Regeln
  und zusätzliche Arbeitswurzeln beim Start aus der Global- und der
  Projekt-Konfiguration (`harw-runtime/src/assembly.rs`).
- `/permissions` (`harw-ops/src/permissions.rs`) mit den Unterbefehlen `show`
  (Default), `mode`/`set <ask|auto|full>`, `allow`/`deny <tool> [muster]` und
  `remove <nr>`, jeweils wahlweise mit `--session`, `--project` oder
  `--global`. Jede Änderung wirkt sofort auf die laufende Session und wird
  zusätzlich in der jeweiligen Konfigurationsebene persistiert.
- `/add-workdir` (`harw-ops/src/add_workdir.rs`) sowie `ExtraRootsCell`
  (`harw-sandbox/src/extra_roots.rs`): zusätzliche, sitzungsweite
  Arbeitsverzeichnis-Wurzeln, mit `--save` dauerhaft im Projekt gemerkt.
  Kandidaten werden symlink-frei kanonisiert; abgelehnt werden das
  Wurzelverzeichnis `/`, das Home-Verzeichnis des Nutzers, jeder Vorfahre der
  primären Arbeitswurzel sowie mehr als 8 zusätzliche Wurzeln.

**Projekt-Erkennung und Projekt-Home**
- `harw-home/src/project.rs`: `discover_project` erkennt den Projekt-Root
  anhand konfigurierbarer Marker (Default `.git`, `project_root_markers` in
  der Konfiguration), unterscheidet gewöhnliche Git-Repositories,
  Git-Worktrees (der Trust-Anker zeigt dabei auf das Haupt-Repository) und
  markerlose Verzeichnisse. `project_key` liefert einen stabilen,
  dateisystemsicheren Schlüssel je Projekt-Root. `ProjectHome` legt
  `<root>/.harw/{memories,plans,goals,state}` mit Rechten `0700` an, schreibt
  ein `.gitignore` für `state/` und verweigert dies für `/` und `$HOME`.
  `RuntimeAssembly` legt dieses Projekt-Home bei jedem Start an.

**Session-Metadaten und Auswahl**
- `SessionMeta`-Sidecar (`harw-session-store/src/meta.rs`,
  `<session-id>.meta.json`): Titel samt Herkunft (`model`/`manual`/`fallback`/
  `none`), Erstellungs- und letzter-Öffnen-Zeitpunkt, Arbeitsverzeichnis,
  Projekt-Zuordnung, gekürzte erste Nutzernachricht und Turn-Zahl. Fehlt der
  Sidecar (ältere Sessions), wird er beim ersten Zugriff aus dem Transcript
  abgeleitet und danach persistiert.
- Neue TUI-Bausteine: `harw-tui/src/session_picker.rs` (Navigation per
  Pfeiltasten/PageUp/PageDown/Home/End, Tippfilter auf Titel/Projekt,
  Umschalten aller Projekte) und `harw-tui/src/relative_time.rs`
  (`gerade eben`, `vor 5 min`, `vor 3 h`, `vor 2 d`, sonst Datum).

**Freigabe-Dialog und Bedienbausteine (TUI)**
- `harw-tui/src/approval_dialog.rs`: Freigabe-Dialog-Widget mit vier Optionen
  (Ja / Ja und nicht mehr fragen für diesen Befehl / Ja und in den
  Auto-Modus wechseln / Nein mit optionaler Freitext-Begründung).
  Tastendrücke wirken erst nach einer Arming-Verzögerung, Esc gilt als Nein,
  ein Countdown wechselt unter einer Minute die Warnfarbe.
- `harw-tui/src/choice_dialog.rs`: generisches Auswahl-Dialog-Widget (u. a.
  für `/export`: Zwischenablage kopieren / als Datei speichern / abbrechen).
- `harw-tui/src/clipboard.rs`: Kopieren in die Zwischenablage über
  `wl-copy`/`xclip`/`xsel`/`pbcopy` je nach erkannter Umgebung, mit
  OSC-52-Fallback für Terminals ohne lokalen Zugriff.
- `harw-tui/src/export.rs`: rendert einen Chat-Verlauf als Markdown
  (Metadaten, Nutzer-/Assistenz-/Systemzeilen) und schreibt ihn atomar in
  eine Datei, kollisionssicher und ohne bestehende Dateien zu überschreiben.
- `harw-tui/src/history_cell.rs`: neue `ToolCell`/`ToolGroupCell`-Zelltypen
  mit einstellbarer Ausführlichkeit (`ToolVerbosity`), die die kompakte
  Darstellung von Werkzeugaufrufen tragen sollen.
- Neuer Befehl `/export` (`harw-ops/src/export.rs`) und erweitertes `/memory`
  (`harw-ops/src/memory.rs`) um `recall <stichwort>`,
  `record <text> [--project|--global]` und `forget <name>`.

**Langzeitgedächtnis v3 (Fakten)**
- `harw-memory/src/facts.rs`: adressierbare Fakten als einzelne
  Markdown-Dateien (`facts/<name>.md`) mit Frontmatter (`type`, `scope`,
  `confidence`, `sources`, `tags`) und `FactStore` zum Lesen, Schreiben,
  Löschen und Durchsuchen. Ein generierter Index (`MEMORY.md`), Nutzungszähler
  (`usage.json`) und ein Verfallsmechanismus (`decay`: unbenutzte Fakten
  verlieren nach einer konfigurierbaren Frist die Hälfte ihrer `confidence`)
  gehören dazu. Vor jedem Schreiben werden gängige Geheimnis-Muster
  (API-Schlüssel, GitHub-/AWS-/Slack-Tokens, PEM-Blöcke, `Bearer`-Header,
  `key=`/`token=`/`secret=`-Werte) redigiert.

### Fixed

- **Kontextbudget verdrängt die auslösende Nutzernachricht nicht mehr.** Eine
  lange Werkzeug-Runde (viele `fs.read`/`shell.exec`-Ergebnisse im selben
  Turn) konnte zuvor die zuletzt gesendete Nutzernachricht aus dem an das
  Modell geschickten Verlauf verdrängen, weil die Byte-Budget-Auswahl allein
  nach Alter der Gruppen entschied. `TurnHistory::tail_preserving_current_turn`
  (`harw-core/src/history.rs`) hält die auslösende Nutzernachricht jetzt in
  jedem Fall im Budget: übergroße Einzelergebnisse werden zuerst gekappt,
  danach werden ältere Tool-Ergebnispaare des offenen Turns gekürzt oder
  ausgelassen, bevor die Nutzernachricht selbst gefährdet wäre.

### Changed

- **CLI-Werte werden beim Parsen geprüft.** Ein ungültiger `--log`-Filter oder
  ein unbekannter `--mode` ist jetzt ein Parse-Fehler (vorher stillschweigend
  akzeptiert). Feste Wertemengen sind als Enums typisiert und erscheinen in
  `--help` und in den Shell-Completions: `connect --channel`,
  `uninstall --scope`, `auth login|token --provider`, `auth import --source`,
  `mcp setup|check --server`, `lens build --source`,
  `settings provider add --api`, `settings permissions set-mode` und
  `models internal openrouter-defaults`. Pfad-, URL- und Freitext-Argumente
  tragen passende Value-Hints.

- **Auto-Compact löst bei 500 000 Input-Tokens aus** (vorher 120 000):
  `DEFAULT_ABSOLUTE_CEILING_TOKENS` (`harw-core/src/auto_compact.rs`) steuert
  über den absoluten Deckel der `AutoCompactPolicy`, wie `maybe_compact`
  (`turn_loop.rs`) aus `last_round_usage.input_tokens` entscheidet.

- `harw web` no longer accepts `--config-dir`; the root space is resolved exclusively
  from `--home`/`HARW_HOME`, which is now required. The web surface's permission
  ceiling remains `ReadWorkspace` for every caller tier — no tier can gain write
  access through the web API.
- `harw serve`: job submission is now restricted to configured submitter principals,
  and both prompt jobs and plan-node jobs require a resolvable HARW home; a job
  submitted without one now completes as `Blocked` instead of running with an
  implicit, looser context.
- One-shot prompts (non-interactive chat) now apply the resolved interaction mode
  and the configured approval policy; a tool call that would need an interactive
  approval prompt is now rejected outright instead of being left pending.
- `harw run` (local echo) now executes under a real local principal (`uid:<n>`,
  operator tier) instead of a fixed placeholder identity.
- `harw doctor` now reports the actual assembled runtime's permissions, tool count,
  and approval chain alongside the existing configuration summary.
- `harw analyze` without `--dry-run` now requires a fully configured model provider
  up front, instead of failing only once the operation actually needed one.

### Removed

- `harw web --config-dir` flag.
- `harw-channel-browser`.
- The embedded MCP client in the core runtime library.

### Security

- `harw serve` now refuses to start if the configuration lists the same MCP
  principal ID more than once, instead of silently disabling the duplicate and
  continuing.
- TUI `/tools`: runtime overrides (`on`, `reset`, `profile`) can never enable a tool
  beyond the intersection of the base tool set and the active mode's tool set;
  previously a runtime toggle could re-enable a tool the active mode had disabled.
- TUI: `!`-prefixed shell commands are rejected outright — the TUI surface does not
  grant shell-execute capability.
- TUI: child roles configured under the `[agents]` config section cannot currently
  be spawned from the TUI (temporary regression; tracked for a follow-up wave).
- Gateway: the echo-model fallback has been removed, and sealed `secrets:`-provider
  credentials are now resolved when mounting the gateway, instead of the mount
  silently proceeding without a real provider.
- Plan-node jobs: the session sandbox is now bound exactly to the derived workspace
  root; a workspace nested under a parent directory that carries a project marker
  no longer inherits that parent's broader filesystem access.
- Prompt jobs run without any project documents injected into context.
- `harw-agent-dsl::roles::can_spawn` (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.
- **G11 — `deny_unknown_fields` audit**: Added `#[serde(deny_unknown_fields)]`
  to 17 container-level structs across `harw-config` and `harw-protocol` that
  deserialize from untrusted TOML configs or wire payloads. Unknown fields are
  now rejected at deserialization, preventing silent authority injection via
  surplus keys. Identified via bottom-up codebase review inspired by
  `hardening-suggestive-inspiration.md` §11.
- **G12 — Remove redundant `unsafe impl Send/Sync`**: `ShortTermMemory` in
  `harw-memory/src/short_term.rs` had manual `unsafe impl Send` and `unsafe
  impl Sync` blocks that were redundant — `RwLock<Inner>` with all-`Send`
  fields is automatically `Send + Sync`. Removed both blocks, eliminating a
  soundness risk surface.
- **Hardening gap analysis**: `docs/design/hardening-gap-analysis.md` —
  comprehensive 14-gap analysis from bottom-up codebase review against
  `hardening-suggestive-inspiration.md` (32 sections). (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.

### TUI Hardening

- **Grapheme cluster cursor movement**: InputEditor cursor movement
  (move_left, move_right, backspace, delete, word-jump) now operates on
  Unicode extended grapheme cluster boundaries via unicode_segmentation,
  not char boundaries. This fixes cursor corruption with combining diacritics
  (e.g. German umlauts encoded as base + combining mark, ZWJ emoji sequences).
  Backspace and forward-delete now drain the entire grapheme cluster range,
  not a single byte. Inspired by codex-rs TextArea grapheme-aware movement.

- **Display-width-aware wrapping**: visible_lines and cursor_position
  now use unicode_width::UnicodeWidthStr for column math instead of char
  count. CJK/wide glyphs (display width 2) are correctly accounted for in
  soft-wrap breakpoints and cursor column calculation. Fixes misalignment
  with wide-character text.

- **History recall boundary gate**: Up/Down keys now only trigger history
  recall when the buffer is empty or the cursor is at position 0/len AND the
  current text matches the last-recalled entry. A last_recalled field tracks
  the most recently loaded history entry. This prevents accidental history
  navigation when the cursor is mid-text in a single-line draft.

- **Paste normalization**: TuiEvent::Paste handler now normalizes CRLF
  to LF and CR to LF before inserting. Prevents stray carriage returns
  from corrupting multi-line pasted text.

- **Dynamic composer height**: The input box height now grows with the actual
  wrapped line count (visible_lines(width)) instead of counting only hard
  newlines. Max height raised from 6 to 10 rows.

- **Semantic border color**: The input box border now uses
  style::border_color(theme), applying the semantic palette (RGB values
  for dark/light themes) to the block border. Fixes the dead_code warning
  for the previously unused border_color function.

## [0.2.0] — 2026-07-16

### Added

**Operation Registry & Command Surface**
- `OperationMeta.aliases` flow through `CommandRegistry::from_operation_registry`;
  `/m`, `/p`, `/reasoning` now dispatch to the same handlers as `/model`,
  `/provider`, `/effort`.
- `OperationRegistry::try_register` returns `Err(RegistryError::DuplicateName |
  AliasCollision | SelfCollision)` on name/alias conflicts at registration time.
- Multi-segment `Surface::Command { path }` values are accepted (previously
  silently skipped).
- `FromRawArgs` derive gained strict compile-time checks: field type must be
  `Option<String>` for positional attrs, conflicting `#[raw(...)]` attrs on one
  field are an error, missing `#[raw(...)]` on a named field is an error, tuple
  and unit structs are rejected, multiple `#[raw(required)]` fields are rejected,
  unknown raw keys are rejected, `nth = 0` is rejected with a friendly message.
  Seven trybuild compile-fail cases enforce all diagnostics.

**Long-lived Session Controller**
- `ChatApp` holds `Arc<TuiSessionController>` as a persistent field.
- `command_exec::build_services()` accepts the controller Arc as a parameter.
- `apply_pending_controller_state` hook fires at the safe turn boundary (before
  `run_turn_streaming` starts), flushing queued mutations onto `AgentSession`.
- `TuiSessionController::apply_to_session` propagates `reasoning_effort`,
  `active_model`, and `active_provider` onto the `AgentSession`.

**Real /model, /provider, /effort routing**
- `AgentSession` gained `active_model: Option<ModelId>` and
  `active_provider: Option<ProviderId>` fields with accessors and setters,
  mirroring `reasoning_effort`.
- `ModelRequest` gained `model_id` and `provider_id` fields; `turn_loop.rs`
  plumbs both from the session into the next request.
- `/provider switch <id>` validates provider existence, credential resolvability,
  and active-model compatibility before mutating. Atomic fail-close on any failure.
- `/model switch <id>` validates via `harw_model_catalog::resolve` and
  cross-checks against the active provider. Atomic fail-close on incompatibility.
- `/provider show` reports the actual runtime provider; `/provider list` marks
  active, auth-ok, auth-missing states.
- `/model show` reports the actual runtime model; `/model list` filters by active
  provider and marks the current selection.
- `ReasoningEffort::Minimal` maps to `None` on the Anthropic adapter (omits
  `output_config.effort`; reasoning stays adaptive).

**HARW SDK Facade**
- New crate `harw` at the workspace root re-exports canonical types from nine
  crates under modules: `extension`, `ops`, `agent`, `provider`, `model`, `core`,
  `defaults`, `types`. `harw::prelude` re-exports the load-bearing types.
- `harw/examples/minimal.rs` provides a runnable example.

**Agent DSL / IR**
- `ExecutableAgentIr` gained `snapshot_id: SnapshotId` computed via BLAKE3 over a
  stable byte stream; deterministic across processes, excludes `trace` (which
  carries timestamps).
- `DslError` variants `MissingBase`, `MissingMixin`, `AuthorityElevation`,
  `IllegalRoleForMixin` carry `DiagLocation { layer, field_path }`. Display now
  includes the field path (e.g. `"authority.capabilities"` on `AuthorityElevation`).

**Type Consolidation**
- `harw-types` newtypes (`ProviderId`, `ModelId`, `ProviderName`, `ModelName`,
  `AgentName`, `CustomerId`) gained `Deref<Target = str>`, `AsRef<str>`,
  `Borrow<str>`, bidirectional `PartialEq<str/&str/String>`, `PartialOrd`, `Ord`.

**Testing Infrastructure**
- `harw-core::testing::RecordingModelProvider` records every `ModelRequest` for
  integration tests.
- New integration tests: `harw-agent-dsl/tests/toml_to_ir_e2e.rs` (5 tests),
  `harw/tests/sdk_example.rs` (1 test), plus expanded unit tests in all touched
  modules.
- E2E suite §7 slices 1–4, 12, 13 shipped; slices 5–11 (real-turn recording) are
  in flight and not gated for this pre-release.

### Changed

- `CommandRegistry::from_operation_registry` now returns
  `Result<Self, TuiRegistryError>`; call sites must propagate or handle the error.
- The dispatch-adapter list in `harw-tui/src/command_exec.rs` no longer maintains
  a parallel alias truth source; the executor consults `OperationMeta::aliases`
  directly.
- `register_all_second_pass_rejects_duplicates` replaces the old
  `register_all_is_additive` test; additive registration is no longer permitted.
- `/model` and `/provider` unknown subcommands now return `InvalidArguments`
  listing the supported set instead of silently falling back to `show`.
- `harw-model-catalog` migrated from `pub type ProviderId = String` to
  `pub use harw_types::{ProviderId, ModelId}` (199 mechanical substitutions across
  12 files; no runtime behaviour change).
- `AgentArgs / SkillsArgs / PluginsArgs`: the single `cmd: Option<String>` field
  is replaced by `action / target / value` fields.

### Fixed

- `AgentArgs / SkillsArgs / PluginsArgs` argument-tokenization bug that discarded
  the second token in `/agent stop <id>`.
- `FromRawArgs` permissive derive accepted invalid field configurations silently;
  now a compile-time error.
- Fresh-per-command `TuiSessionController` construction caused state resets on
  every command; the controller is now long-lived on `ChatApp`.
- Silent snapshot writes for `/model` and `/provider` when no actual switch
  occurred.
- `run_loop` in `harw-tui` no longer hard-crashes the process when a chat
  turn fails (e.g. an invalid/expired provider credential returning HTTP
  401): `TuiError::Core` is now caught, shown as a readable system line in
  the chat transcript, and the session stays alive; only `TuiError::Io`
  (fatal terminal failures) still exits and ends the process.
- `harw-ops::provider::auth_status_label` had an unreachable wildcard match
  arm that only surfaced under the `harw-provider` crate's default (non
  `chatgpt-oauth`) feature set, breaking `cargo clippy -- -D warnings` on
  ordinary builds; replaced with an explicit `#[cfg(feature =
  "chatgpt-oauth")]`-gated arm, and `harw-ops` now forwards a matching
  `chatgpt-oauth` feature to `harw-provider`.
- `harw` SDK facade example test extended (`sdk_example_toml_to_executable_runtime_context`)
  to prove the full pipeline — TOML parse → `resolve_definition` → `lower`
  into `ExecutableAgentIr` → `assemble_default_registry` →
  `AgentSession::new` — is expressible using only the public `harw::`
  facade, closing the previously-partial coverage of E2E test item #12
  (SDK example must build a registry, compile an agent definition, AND
  produce an executable runtime context).

### Deprecated

Nothing formally deprecated in this 0.x pre-release cycle.

### Known Unstable

The following areas are intentionally out of scope for this 0.2.0 milestone and carry no
stability guarantee:

- `Harness::builder()` fluent API — planned for a later 0.2.x pre-release.
- Typed Parent-to-Child return pipeline (Agent-as-Tool) — child returns a string
  today; fachliche typed return is deferred.
- Full IR-to-Runtime consumption — `ExecutableAgentIr` exists but the runtime
  still consumes `AgentRole` from `harw-types` directly, not the IR.
- `GoalGraph` / `FinalObjective` / `GoalEvaluator` — architecture only, not
  implemented runtime.
- `DurableJobRunner` has no CLI runtime caller yet.
- Memory system Cognition Loop is not fully wired end-to-end.

### Migration

See [docs/migration/0.1.0-to-0.2.0.md](docs/migration/0.1.0-to-0.2.0.md).
