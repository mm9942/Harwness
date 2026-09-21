# Config-Geltungsbereiche: GLOBAL vs. PROFIL

Bestandsaufnahme aller `HarnessConfig`-Felder, ihres heutigen Merge-Verhaltens
über die Config-Ebenen (Root-Home → aktives Profil → Projekt) und ein
Vorschlag, welchen Geltungsbereich (GLOBAL/PROFIL) jedes Feld künftig haben
sollte. **Reine Analyse, kein Produktivcode geändert.**

Quellen: `harw-config/src/harness_config.rs`, `harw-config/src/plan_toml.rs`,
`harw-config/src/mode_toml.rs`, `harw-config/src/research_toml.rs`,
`harw-config/src/permissions_toml.rs`, `harw-config/src/internal_models.rs`,
`harw-config/src/auth_toml.rs` (nur `SecretRef`-Typ),
`harw-config/src/discovery.rs` (`discover_config_with_restricted`,
`apply_restricted_layer`, `merge_restricted_*`), `harw-home/src/scaffold.rs`.

---

## 0. Heutiges Merge-Modell (Kurzfassung)

`discover_config_with_restricted` (`discovery.rs:580-742`) iteriert über die
vertrauten Layer (`~/.harw` → `~/.harw/profiles/<name>`) in aufsteigender
Präzedenz. Für jeden Layer mit eigener `config.toml` (Schleife
`discovery.rs:593-650`):

1. `[network]`/`[browser]`/`[dod]`/`[web]` werden **unabhängig** von
   `HarnessConfig` geparst (`extract_section`, da `harness_config.rs` diese
   vier Tabellen noch nicht kennt — `NEW_SECTION_KEYS`,
   `discovery.rs:794-813`) und **nur bei Anwesenheit im Layer ganz ersetzt**;
   fehlt die Tabelle, bleibt der Wert des vorigen Layers stehen
   (`discovery.rs:607-624`, Kommentar „Home-Layer setzt“).
2. Der Rest der `config.toml` wird als `HarnessConfig` deserialisiert
   (`cfg`, `discovery.rs:626-630`). Serde füllt dabei **jedes** Feld, das
   dieser Layer nicht selbst setzt, mit dem hartcodierten Section-Default
   (`#[serde(default)]`).
3. Nur fünf Werte werden explizit aus dem bisher akkumulierten Stand
   (`resolved.harness`) in `cfg` zurückgeschrieben, bevor `cfg` übernommen
   wird (`discovery.rs:631-647`): `default_provider`, `default_model`,
   `active_uia_definition` (jeweils nur falls `cfg.<feld>.is_none()`),
   `onboarding` (nur falls der Layer die Tabelle `[onboarding]` gar nicht
   enthält) und `internal_models` (feingranular pro Modellstelle über
   `merge_internal_models`, `discovery.rs:846-866`).
4. `resolved.harness = cfg;` (`discovery.rs:649`) — **die gesamte
   `HarnessConfig` wird ersetzt.**

**Konsequenz:** Jedes der **anderen ~80 Felder** (siehe Abschnitt 4) geht
stillschweigend auf seinen Default zurück, sobald ein späterer vertrauter
Layer (typischerweise das Profil) eine `config.toml` besitzt, die dieses Feld
nicht selbst setzt — unabhängig davon, ob ein früherer Layer (Root-Home) es
explizit gesetzt hatte. `ensure_home` legt die Profil-`config.toml` **immer**
an (`harw-home/src/scaffold.rs:93-97`, Template `PROFILE_CONFIG_TEMPLATE`
mit fest codiertem `[mcp_listener]`), das Problem tritt also praktisch bei
jeder Installation auf, sobald die Home-Ebene irgendein Feld außerhalb der
fünf Ausnahmen abweichend vom Default setzt.

Zusätzlich existiert bereits eine **„restricted“-Logik** für einen nicht
vertrauten Projekt-Layer (`apply_restricted_layer`,
`discovery.rs:752-792`, aufgerufen aus `discover_config_with_restricted:728-731`
nachdem alle vertrauten Layer verarbeitet sind): Sie liest `config.toml`
eines untrusted Repo-`.harw` und wendet **nur verengende** Regeln auf den
bereits gemergten, vertrauten Stand an — für eine Teilmenge von neun Feldern
(`merge_restricted_harness`, `discovery.rs:942-1003`, plus
`merge_restricted_network/browser/dod`, `discovery.rs:883-939`). Details in
Abschnitt 3.

---

## 1. Vollständige Feldliste

Legende Spalte „Merge heute“: **ERSETZT** = Feld wird bei Fehlen im Layer auf
Section-Default zurückgesetzt (Bug); **ÜBERNOMMEN** = explizite Ausnahme
(Zeile 631–647); **RESTRICTED-VORBILD** = zusätzlich bereits Teil der
`merge_restricted_harness`-Verengung für den untrusted Projekt-Layer.

### 1.1 Top-Level-Felder (`harness_config.rs:13-103`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `config_version` | `u32` | `0` | `harness_config.rs:14-15` | ERSETZT |
| `workspace_root` | `Option<String>` | `None` | `harness_config.rs:16-17` | ERSETZT |
| `default_provider` | `Option<String>` | `None` | `harness_config.rs:18-19` | ÜBERNOMMEN (nur falls `cfg`-Wert `None`) |
| `default_model` | `Option<String>` | `None` | `harness_config.rs:20-21` | ÜBERNOMMEN (nur falls `cfg`-Wert `None`) |
| `active_agent_definition` | `Option<String>` | `None` | `harness_config.rs:22-23` | ERSETZT |
| `active_uia_definition` | `Option<String>` | `None` | `harness_config.rs:24-28` | ÜBERNOMMEN (nur falls `cfg`-Wert `None`) |
| `uia_provider` | `Option<String>` | `None` | `harness_config.rs:29-35` | ERSETZT |
| `uia_model` | `Option<String>` | `None` | `harness_config.rs:36-39` | ERSETZT |
| `uia_worker_model` | `Option<String>` | `None` | `harness_config.rs:40-48` | ERSETZT |
| `policy_profile` | `Option<String>` | `None` | `harness_config.rs:49-50` | ERSETZT |
| `project_root_markers` | `Option<Vec<String>>` | `None` | `harness_config.rs:76-80` | ERSETZT |
| `base_dir` | `Option<PathBuf>` | — | `harness_config.rs:101-102` | `#[serde(skip)]` — **kein TOML-Feld**, wird pro Layer intern auf `Some(base.clone())` gesetzt (`discovery.rs:648`), reine Buchführung |

### 1.2 `[logging]` (`harness_config.rs:262-284`, Defaults `420-422`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `logging.level` | `String` | `"info"` | `harness_config.rs:269` | ERSETZT |
| `logging.target_module_paths` | `bool` | `false` | `harness_config.rs:271` | ERSETZT |
| `logging.json` | `bool` | `false` | `harness_config.rs:273` | ERSETZT |

### 1.3 `[tui]` (`harness_config.rs:286-304`, Defaults `423-428`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `tui.theme` | `String` | `"default-dark"` | `harness_config.rs:292` | ERSETZT |
| `tui.keybindings_file` | `String` | `"keybindings.toml"` | `harness_config.rs:294` | ERSETZT |

### 1.4 `[session]` (`harness_config.rs:306-343`, Defaults `429-437`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `session.store_dir` | `String` | `"sessions"` | `harness_config.rs:311` | ERSETZT |
| `session.journal_format` | `String` | `"jsonl"` | `harness_config.rs:313` | ERSETZT |
| `session.retention_days` | `u32` | `90` | `harness_config.rs:315` | ERSETZT |
| `session.title_generation` | `bool` | `true` | `harness_config.rs:321` | ERSETZT |
| `session.title_model` | `Option<String>` | `None` | `harness_config.rs:325` | ERSETZT |

### 1.5 `[policy]` (`harness_config.rs:345-353`, Default `411-418`, `default_visibility_scope` `438-440`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `policy.default_visibility_scope` | `String` | `"self"` | `harness_config.rs:350` | ERSETZT |
| `policy.require_approval_for` | `Vec<String>` | `[]` | `harness_config.rs:352` | ERSETZT — **RESTRICTED-VORBILD**: für den untrusted Projekt-Layer bereits als Vereinigung gemerged (`merge_restricted_harness:958-964`) |

### 1.6 `[mcp_listener]` (`harness_config.rs:355-409`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `mcp_listener.enabled` | `bool` | `false` | `harness_config.rs:364` | ERSETZT |
| `mcp_listener.listen_addr` | `String` | `"127.0.0.1:1337"` | `harness_config.rs:368` | ERSETZT |
| `mcp_listener.path` | `String` | `"/mcp"` | `harness_config.rs:372` | ERSETZT |
| `mcp_listener.principals` | `Vec<McpPrincipalToml>` | `[]` | `harness_config.rs:376` | ERSETZT |
| `mcp_listener.principals[].id` | `String` | — (Pflichtfeld) | `harness_config.rs:382` | Teil von `principals`, s.o. |
| `mcp_listener.principals[].credential_ref` | `SecretRef` (`auth_toml.rs:23-40`) | — (Pflichtfeld) | `harness_config.rs:383` | Teil von `principals`, s.o. |
| `mcp_listener.principals[].tenant` | `String` | — (Pflichtfeld) | `harness_config.rs:384` | Teil von `principals`, s.o. |
| `mcp_listener.principals[].workspace` | `String` | — (Pflichtfeld) | `harness_config.rs:385` | Teil von `principals`, s.o. |
| `mcp_listener.principals[].job_capabilities` | `Vec<McpJobCapabilityToml>` (Enum `ReadOwn`/`ReadWorkspace`/`SubmitOwn`/`CancelOwn`/`CancelWorkspace`, `harness_config.rs:390-398`) | `[]` | `harness_config.rs:387` | Teil von `principals`, s.o. |

### 1.7 `[onboarding]` (`harness_config.rs:231-251`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `onboarding.seen` (Tabelle) | `OnboardingSeen` | s.u. | `harness_config.rs:238` | ÜBERNOMMEN als Ganzes, nur falls Layer die Top-Level-Tabelle `[onboarding]` **gar nicht** enthält (`discovery.rs:640-642`) — kein Per-Feld-Merge |
| `onboarding.seen.provider` | `bool` | `false` | `harness_config.rs:246` | s.o. |
| `onboarding.seen.model` | `bool` | `false` | `harness_config.rs:248` | s.o. |
| `onboarding.seen.channel` | `bool` | `false` | `harness_config.rs:250` | s.o. |

### 1.8 `[tools.plan]` (`plan_toml.rs:26-150`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `tools.plan.enabled` | `bool` | `true` | `plan_toml.rs:43` | ERSETZT |
| `tools.plan.persist` | `bool` | `true` | `plan_toml.rs:47` | ERSETZT |
| `tools.plan.require_for_complex_work` | `bool` | `false` | `plan_toml.rs:51` | ERSETZT |
| `tools.plan.validate_dependency_cycles` | `bool` | `true` | `plan_toml.rs:55` | ERSETZT — **RESTRICTED-VORBILD**: OR-Verengung (`merge_restricted_harness:990-992`) |
| `tools.plan.validate_write_conflicts` | `bool` | `true` | `plan_toml.rs:59` | ERSETZT — **RESTRICTED-VORBILD**: OR-Verengung (`merge_restricted_harness:993-995`) |
| `tools.plan.max_nodes` | `usize` | `256` | `plan_toml.rs:63` | ERSETZT — **RESTRICTED-VORBILD**: Minimum (`merge_restricted_harness:996-998`) |
| `tools.plan.require_exploration_for` | `Vec<String>` | `["coding","integration"]` | `plan_toml.rs:67` | ERSETZT |
| `tools.plan.exploration_ttl_secs` | `u64` | `86400` | `plan_toml.rs:71` | ERSETZT |
| `tools.plan.max_expand_depth` | `u32` | `3` | `plan_toml.rs:75` | ERSETZT — **RESTRICTED-VORBILD**: Minimum (`merge_restricted_harness:999-1001`) |

### 1.9 `[mode]` (`mode_toml.rs:19-38`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `mode.default` | `String` (`chat`/`plan`/`explore`/`work`/`shell`) | `"plan"` | `mode_toml.rs:25` | ERSETZT |

### 1.10 `[research]` (`research_toml.rs:16-105`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `research.network_allow_hosts` | `Vec<String>` | `["docs.rs","crates.io","doc.rust-lang.org","static.crates.io"]` | `research_toml.rs:22` | ERSETZT — **RESTRICTED-VORBILD**: Schnittmenge (`merge_restricted_harness:968-973`) |
| `research.cargo_registry_read` | `bool` | `true` | `research_toml.rs:26` | ERSETZT — **RESTRICTED-VORBILD**: AND (`merge_restricted_harness:974-976`) |
| `research.max_fetch_bytes` | `usize` | `1_048_576` | `research_toml.rs:29` | ERSETZT — **RESTRICTED-VORBILD**: Minimum (`merge_restricted_harness:977-980`) |
| `research.fetch_timeout_secs` | `u64` | `20` | `research_toml.rs:32` | ERSETZT — **RESTRICTED-VORBILD**: Minimum (`merge_restricted_harness:981-986`) |
| `research.cache_ttl_secs` | `u64` | `3600` | `research_toml.rs:36` | ERSETZT — **nicht** von der Restricted-Logik erfasst (Inkonsistenz innerhalb derselben Sektion, siehe Fund 3) |

### 1.11 `[permissions]` (`permissions_toml.rs:39-74`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `permissions.default_mode` | `Option<String>` (`ask`/`auto`/`full`) | `None` | `permissions_toml.rs:45` | ERSETZT |
| `permissions.approval_timeout_secs` | `Option<u64>` (10–86400) | `None` | `permissions_toml.rs:49` | ERSETZT |
| `permissions.allow` | `Vec<RuleToml>` | `[]` | `permissions_toml.rs:52` | ERSETZT |
| `permissions.deny` | `Vec<RuleToml>` | `[]` | `permissions_toml.rs:57` | ERSETZT |
| `permissions.extra_roots` | `Vec<PathBuf>` (≤8, absolut) | `[]` | `permissions_toml.rs:61` | ERSETZT |
| `RuleToml.tool` (Feld von `allow[]`/`deny[]`) | `String` | — (Pflichtfeld) | `permissions_toml.rs:69` | Teil der jeweiligen Liste, s.o. |
| `RuleToml.pattern` (Feld von `allow[]`/`deny[]`) | `Option<String>` | `None` | `permissions_toml.rs:73` | Teil der jeweiligen Liste, s.o. |

### 1.12 `[sandbox]` (`harness_config.rs:180-229`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `sandbox.cargo` | `Option<CargoSandboxToml>` | `None` | `harness_config.rs:184` | ERSETZT |
| `sandbox.cargo.mode` | `CargoSandboxModeToml` (`inspect`/`build_offline`/`fetch`) | — (Pflichtfeld, falls `cargo` gesetzt) | `harness_config.rs:198` | Teil von `sandbox.cargo`, s.o. |
| `sandbox.cargo.cargo_bin` | `String` (absoluter Pfad) | — (Pflichtfeld) | `harness_config.rs:199` | Teil von `sandbox.cargo`, s.o. |
| `sandbox.cargo.rustup_home` | `String` (absoluter Pfad) | — (Pflichtfeld) | `harness_config.rs:200` | Teil von `sandbox.cargo`, s.o. |
| `sandbox.cargo.cargo_home` | `String` (absoluter Pfad) | — (Pflichtfeld) | `harness_config.rs:201` | Teil von `sandbox.cargo`, s.o. |
| `sandbox.tmux` | `Option<TmuxSandboxToml>` | `None` | `harness_config.rs:186` | ERSETZT |
| `sandbox.tmux.mode` | `TmuxOperationModeToml` (`inspect`/`write`) | — (Pflichtfeld) | `harness_config.rs:220` | Teil von `sandbox.tmux`, s.o. |
| `sandbox.tmux.socket_path` | `String` (absoluter Pfad) | — (Pflichtfeld) | `harness_config.rs:221` | Teil von `sandbox.tmux`, s.o. |

### 1.13 `[internal_models]` (`internal_models.rs:139-193`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `internal_models.use_openrouter_defaults` | `bool` | `true` | `internal_models.rs:160` | ÜBERNOMMEN, feingranular (`merge_internal_models:857-859`, nur falls Layer den Schlüssel nicht selbst setzt) |
| `internal_models.session_title` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:162` | ÜBERNOMMEN, feingranular (`merge_internal_models:860-865`) |
| `internal_models.compaction_summary` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:164` | ÜBERNOMMEN, feingranular |
| `internal_models.memory_consolidation` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:166` | ÜBERNOMMEN, feingranular |
| `internal_models.dream_reflection` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:168` | ÜBERNOMMEN, feingranular |
| `internal_models.explorer` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:170` | ÜBERNOMMEN, feingranular |
| `internal_models.research` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:172` | ÜBERNOMMEN, feingranular |
| `internal_models.worker_simple` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:174` | ÜBERNOMMEN, feingranular |
| `internal_models.worker_complex` | `Option<InternalModelChoice>` | `None` | `internal_models.rs:176` | ÜBERNOMMEN, feingranular |
| `InternalModelChoice.provider`/`.model` (Feld jeder Stelle) | `Option<String>` je | `None` | `internal_models.rs:143-145` | Teil der jeweiligen Stelle, s.o. |

### 1.14 `[compaction]` (`harness_config.rs:105-117`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `compaction.absolute_ceiling_tokens` | `Option<u64>` | `None` | `harness_config.rs:116` | ERSETZT |

### 1.15 `[reasoning]` (`harness_config.rs:119-148`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `reasoning.uia` | `Option<String>` | `None` | `harness_config.rs:130` | ERSETZT |
| `reasoning.root_orchestrator` | `Option<String>` | `None` | `harness_config.rs:134` | ERSETZT |
| `reasoning.root_orchestrator_with_subs` | `Option<String>` | `None` | `harness_config.rs:138` | ERSETZT |
| `reasoning.sub_orchestrator` | `Option<String>` | `None` | `harness_config.rs:141` | ERSETZT |
| `reasoning.worker_complex` | `Option<String>` | `None` | `harness_config.rs:144` | ERSETZT |
| `reasoning.worker_simple` | `Option<String>` | `None` | `harness_config.rs:147` | ERSETZT |

### 1.16 `[guards]` (`harness_config.rs:150-175`)

| TOML-Pfad | Typ | Default | Datei:Zeile | Merge heute |
|---|---|---|---|---|
| `guards.enabled` | `Option<bool>` | `None` → Laufzeit-Default `true` | `harness_config.rs:159` | ERSETZT |
| `guards.repeated_failure_warn` | `Option<u32>` | `None` → `2` | `harness_config.rs:162` | ERSETZT |
| `guards.repeated_failure_abort` | `Option<u32>` | `None` → `3` | `harness_config.rs:165` | ERSETZT |
| `guards.no_progress_rounds_warn` | `Option<u32>` | `None` → `4` | `harness_config.rs:168` | ERSETZT |
| `guards.no_progress_rounds_abort` | `Option<u32>` | `None` → `8` | `harness_config.rs:171` | ERSETZT |
| `guards.plan_stale_rounds` | `Option<u32>` | `None` → `6` | `harness_config.rs:174` | ERSETZT |

**Gesamtzahl dokumentierter `HarnessConfig`-Felder: 89** (Blattfelder inkl.
verschachtelter Typen wie `McpPrincipalToml`, `RuleToml`,
`CargoSandboxToml`/`TmuxSandboxToml`, `InternalModelChoice`,
`OnboardingSeen`; dazu `base_dir` als 90. Tabellenzeile in Abschnitt 1.1,
aber **kein TOML-Feld** — `#[serde(skip)]`, daher nicht mitgezählt).

Außerhalb der `HarnessConfig` selbst, aber im selben `config.toml` und mit
derselben Layer-Mechanik geparst: `[network]` (3 Felder), `[browser]`
(5 Felder), `[dod]` (4 Felder), `[web]` (3 Felder) — siehe Abschnitt 3, sowie
Randnotiz unten. Diese vier Sektionen sind **keine** `HarnessConfig`-Felder
(`harness_config.rs` referenziert sie nicht), werden aber pro Layer aus
derselben Datei extrahiert (`extract_section`, `discovery.rs:823-835`) und
haben ein **anderes, bereits stichhaltigeres** Merge-Verhalten (Abschnitt 0,
Punkt 1: sticky statt reset). Provider-/Modell-/Secret-Kataloge
(`providers/*.toml`, `models/*.toml`, `auth.toml`) sind ebenfalls **keine**
`HarnessConfig`-Felder, sondern eigene `ResolvedConfig`-Karten mit
datei-basiertem Merge (`discover_flat_dir`, `discovery.rs:1268-1292`: letzte
Layer-Datei mit demselben Namen gewinnt vollständig; `auth.toml` wird pro
Layer ganz ersetzt, `discovery.rs:679-686`). Die einzigen tatsächlichen
„Provider-/Modell-Felder“ *innerhalb* `HarnessConfig` sind
`default_provider`, `default_model`, `uia_provider`, `uia_model`,
`uia_worker_model`, `active_agent_definition`, `active_uia_definition`,
`policy_profile` (bereits in 1.1 gelistet).

---

## 2. Scope-Vorschlag pro Feld

Legende: 🔒 = vom Nutzer als sicherheits-/beschränkungsrelevant markiert
(Freigaben, Sandbox, Netzwerk, Secrets, Listener, Berechtigungen).

**Status: Tabelle vom Nutzer bestätigt (2026-09-21).** Alle vormals
**OFFEN** markierten Zeilen sind unten durch die Nutzerentscheidung ersetzt
und mit „(entschieden 2026-09-21)" gekennzeichnet. Die Spalte „Merge-Regel"
verweist ab jetzt auf die exakten `MergeRule`-Varianten aus Abschnitt 6; die
vollständige, feldgenaue Zuordnung aller 89 Felder steht in Abschnitt 6.3 (die
Gruppierung hier fasst strukturgleiche Unterfelder weiterhin zusammen).

| TOML-Pfad | Vorschlag | Merge-Regel | Begründung | 🔒 |
|---|---|---|---|---|
| `config_version` | Kein Scope — **pro Datei geprüft, nicht gemergt** (entschieden 2026-09-21) | `PerFileValidated` | Jede Layer-`config.toml` deklariert ihren eigenen `config_version`-Wert zur Migrations-/Kompatibilitätssteuerung; er wird nicht über Layer hinweg zusammengeführt, sondern pro Datei gegen die unterstützte(n) Schema-Version(en) geprüft. `resolved.harness.config_version` übernimmt danach schlicht den Wert des zuletzt geladenen vertrauten Layers (heutiges `cfg`-Zuweisungsverhalten bleibt hier unverändert), ist aber kein „gemergter" Wert im Sinne dieses Modells. | |
| `workspace_root` | **PROFIL** (entschieden 2026-09-21) | `ProfileReplaces` | Jedes Profil braucht einen eigenen Arbeitskontext. Das Pfad-Traversal-Risiko ist keine Merge-Scope-Frage, sondern wird von der bestehenden Sandbox-/Root-Validierung beim Runtime-Aufbau abgefangen; dieses Feld beschreibt nur die *bevorzugte* Wurzel, keine eigenständige Berechtigungsgrenze. | 🔒 |
| `default_provider` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | Aktive Modellwahl ist Profil-Alltag, keine Beschränkung. | |
| `default_model` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | s.o. | |
| `active_agent_definition` | PROFIL | letzter gesetzter Wert gewinnt | Auswahl einer Agenten-Definition ist Nutzerpräferenz. | |
| `active_uia_definition` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | s.o. | |
| `uia_provider` | PROFIL | letzter gesetzter Wert gewinnt | Pinning ist Profil-spezifisch. | |
| `uia_model` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `uia_worker_model` | PROFIL | letzter gesetzter Wert gewinnt | Pinning der uia-worker-Rollenfamilie ist Profil-spezifisch, wie `uia_model`. | |
| `policy_profile` | **GLOBAL** (entschieden 2026-09-21) | `GlobalOnly` | Admin erzwingt eine Policy-Familie zentral; ein Profil kann sie nicht umgehen. Bisher kein Consumer im Code gefunden (nur deklariert + per CLI setzbar, `harw-cli/src/settings.rs`) — die Entscheidung gilt vorsorglich für den Moment, in dem ein Consumer entsteht. | 🔒 |
| `project_root_markers` | **PROFIL** (entschieden 2026-09-21) | `ProfileReplaces` | Reine Heuristik zur Root-Erkennung ohne eigene Sicherheitswirkung — die eigentliche Sandbox-Grenze ziehen `sandbox.*` (GLOBAL exklusiv) und `permissions.extra_roots` (GLOBAL-Obergrenze), nicht die Root-*Erkennung*. Ein Profil darf seine eigene Projekterkennung frei anpassen. | |
| `logging.level` | PROFIL | letzter gesetzter Wert gewinnt | Ausgabe-Verbosität, keine Beschränkung. | |
| `logging.target_module_paths` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `logging.json` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `tui.theme` | PROFIL | letzter gesetzter Wert gewinnt | Reine UI-Präferenz. | |
| `tui.keybindings_file` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `session.store_dir` | PROFIL | letzter gesetzter Wert gewinnt | Ablagepfad ist Profil-lokal. | |
| `session.journal_format` | PROFIL | letzter gesetzter Wert gewinnt | Reines Format. | |
| `session.retention_days` | **GLOBAL als Obergrenze (Minimum)** (entschieden 2026-09-21) | `MinBound` | Admin erzwingt eine Höchst-Aufbewahrung als Compliance-Leitplanke; das Profil darf nur kürzer aufbewahren, nie länger als global erlaubt. | |
| `session.title_generation` | PROFIL | letzter gesetzter Wert gewinnt | Komfort-Feature. | |
| `session.title_model` | PROFIL | letzter gesetzter Wert gewinnt | Legacy-Modellwahl, wie `internal_models`. | |
| `policy.default_visibility_scope` | **GLOBAL, nur ein engerer Wert gilt** (entschieden 2026-09-21) | `StricterOf(ordering)` — Minimalordnung `"self"` < `"everyone"` (Abschnitt 6.2); jeder dritte/unbekannte String wird abgelehnt statt eingeordnet (entschieden 2026-09-21, Risiko R1 Abschnitt 8; Absicherungstest Abschnitt 7h #24) | Sicherheitsrelevant (Standard-Sichtbarkeit neuer Sessions), aber als freier String ohne `validate()`-Einschränkung im Code nur unvollständig ordbar — nur die beiden im Code belegten Werte (`harness_config.rs:438-440`, `discovery.rs:1428`) sind eindeutig ordbar. | 🔒 |
| `policy.require_approval_for` | GLOBAL (Baseline) + Profil erweitert | **Vereinigung** | Direkt vom Nutzer vorgegebenes Beispiel; bereits identisch für den untrusted Projekt-Layer implementiert (`merge_restricted_harness:958-964`) — dasselbe Muster auf GLOBAL/PROFIL übertragen. | 🔒 |
| `mcp_listener.enabled` | GLOBAL (Baseline) + Profil darf nur verengen | **AND** (analog `browser.enabled`, `merge_restricted_browser:912-914`) | Öffnet eine lokale Netzwerk-Angriffsfläche; ein Profil darf einen global deaktivierten Listener nicht aktivieren. | 🔒 |
| `mcp_listener.listen_addr` | GLOBAL | global gewinnt | Bind-Adresse ist Angriffsfläche; Profil soll sie nicht verschieben können (Analogie zu `web.bind`, das nie vom Repo beeinflussbar ist). | 🔒 |
| `mcp_listener.path` | GLOBAL | global gewinnt | s.o. | 🔒 |
| `mcp_listener.principals` (inkl. `id`/`credential_ref`/`tenant`/`workspace`/`job_capabilities`) | **GLOBAL, Profil darf nur entfernen** (entschieden 2026-09-21) | `Intersection` nach Vergleichsschlüssel `id` (entschieden 2026-09-21, Risiko R2 Abschnitt 8) — Unterfelder `id`/`credential_ref`/`tenant`/`workspace`/`job_capabilities` sind `CompositeMember` (Abschnitt 6.3) | Enthält Credential-Referenzen und Autorisierungs-Capabilities; ein Profil darf einzelne Principals per Weglassen ihrer `id` aus der Liste entfernen, aber niemals eine neue `id` hinzufügen oder Felder eines bestehenden Principal-Eintrags ändern (z. B. `job_capabilities` erweitern) — nur Principals, deren `id` exakt in der globalen Liste vorkommt, dürfen im Profil-Ergebnis auftauchen, und ihre Felder stammen dabei ausschließlich aus der globalen Fassung (kein Feld-Merge innerhalb eines Principal-Eintrags). | 🔒 |
| `onboarding.seen.*` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert, nur ganze Sektion statt Feld) | Reiner First-Run-Fortschritt pro Profil. | |
| `tools.plan.enabled` | PROFIL | letzter gesetzter Wert gewinnt | Feature-Umschalter, kein Zugriffsschutz. | |
| `tools.plan.persist` | PROFIL | letzter gesetzter Wert gewinnt | Speicherverhalten. | |
| `tools.plan.require_for_complex_work` | PROFIL | letzter gesetzter Wert gewinnt | Workflow-Vorgabe, kein Zugriffsschutz. | |
| `tools.plan.validate_dependency_cycles` | GLOBAL (Baseline) + Profil darf nur verschärfen | **OR** (bereits Vorbild: `merge_restricted_harness:990-992`) | Sicherheits-/Korrektheits-Prüfung; Abschalten wäre eine Lockerung. | |
| `tools.plan.validate_write_conflicts` | GLOBAL (Baseline) + Profil darf nur verschärfen | **OR** (Vorbild: `merge_restricted_harness:993-995`) | s.o. | |
| `tools.plan.max_nodes` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** (Vorbild: `merge_restricted_harness:996-998`) | Ressourcen-/Ausufer-Grenze. | |
| `tools.plan.require_exploration_for` | PROFIL | letzter gesetzter Wert gewinnt | Workflow-Feinabstimmung. | |
| `tools.plan.exploration_ttl_secs` | PROFIL | letzter gesetzter Wert gewinnt | Workflow-Feinabstimmung. | |
| `tools.plan.max_expand_depth` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** (Vorbild: `merge_restricted_harness:999-1001`) | Ressourcen-/Ausufer-Grenze. | |
| `mode.default` | PROFIL | letzter gesetzter Wert gewinnt | Interaktionsmodus ist reine Präferenz. | |
| `research.network_allow_hosts` | GLOBAL (Baseline) + Profil darf nur verengen | **Schnittmenge** (Vorbild: `merge_restricted_harness:968-973`) | Netzwerk-Egress-Allowlist. | 🔒 |
| `research.cargo_registry_read` | GLOBAL (Baseline) + Profil darf nur verengen | **AND** (Vorbild: `merge_restricted_harness:974-976`) | Dateisystem-Lesezugriff auf Cargo-Cache. | 🔒 |
| `research.max_fetch_bytes` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** (Vorbild: `merge_restricted_harness:977-980`) | Ressourcengrenze für Netzwerk-Fetches. | 🔒 |
| `research.fetch_timeout_secs` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** (Vorbild: `merge_restricted_harness:981-986`) | s.o. | 🔒 |
| `research.cache_ttl_secs` | PROFIL | letzter gesetzter Wert gewinnt | Reine Cache-Lebensdauer, kein Zugriffsschutz (im Unterschied zu den übrigen `[research]`-Feldern heute inkonsistent unbehandelt, s. Fund 3). | |
| `permissions.default_mode` | **GLOBAL als Obergrenze** (entschieden 2026-09-21) | `StricterOf(ordering)` — `ask` > `auto` > `full` (Abschnitt 6.2) | `ask` fragt immer nach (am sichersten), `full` nie (am offensten). Die Rangfolge war im Code bisher nirgends kodiert (`ALLOWED_MODES`, `permissions_toml.rs:17`, ist nur eine ungeordnete Werteliste); dieses Dokument legt sie erstmals fest. Ein Profil darf nur Richtung `ask` verschärfen, nie Richtung `full` lockern. | 🔒 |
| `permissions.approval_timeout_secs` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | Kürzerer Timeout = konservativer (Auto-Ablehnung greift früher); analog zu `research.*_secs`. | 🔒 |
| `permissions.allow` | **GLOBAL, Schnittmenge** (entschieden 2026-09-21) | `Intersection` | `allow`-Regeln umgehen die Freigabe-Abfrage — das Gegenteil von `require_approval_for`. Eine Vereinigung wäre eine **Lockerung**; korrekt ist die Schnittmenge: ein Profil kann nichts Neues öffnen, nur global erlaubte Regeln nicht referenzieren (effektiv entfernen). | 🔒 |
| `permissions.deny` | GLOBAL (Baseline) + Profil erweitert | **Vereinigung** | Gegenstück zu `allow`: mehr `deny`-Regeln bedeuten nur mehr Ablehnungen, also sicher zu vereinigen — gleiches Muster wie `require_approval_for`. | 🔒 |
| `permissions.extra_roots` | **GLOBAL, Schnittmenge** (entschieden 2026-09-21) | `Intersection` | Erweitert erlaubte Arbeitswurzeln — analog zu `permissions.allow` eine potenzielle Rechteausweitung; dieselbe Schnittmengen-Logik: ein Profil kann nur eine Teilmenge der global gesetzten Wurzeln referenzieren, nichts Neues öffnen. | 🔒 |
| `sandbox.cargo.*` | GLOBAL exklusiv | Profil darf nicht setzen/überschreiben | Vertrauensanker für die Cargo-Sandbox; Moduldoku (`harness_config.rs:70-74`) verlangt ausdrücklich, dass diese Werte nur beim Runtime-Aufbau aus der (vertrauten) Konfiguration gelesen werden. Direkte Analogie zu `browser.geckodriver_path`/`geckodriver_sha256` und `dod.proof_key_dir`, die bereits nie aus einem Repo-Layer übernommen werden (`browser_toml.rs:18-25`, `dod_toml.rs:22-31`). | 🔒 |
| `sandbox.tmux.*` | GLOBAL exklusiv | Profil darf nicht setzen/überschreiben | s.o. | 🔒 |
| `internal_models.*` (alle 9 Felder) | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | Modell-Routing für Hilfsaufgaben ist Nutzerpräferenz/Kostensteuerung, keine Zugriffsbeschränkung. | |
| `compaction.absolute_ceiling_tokens` | **GLOBAL als Obergrenze (Minimum)** (entschieden 2026-09-21) | `MinBound` | Admin erzwingt eine Kostenobergrenze; das Profil darf nur senken, nie über die globale Grenze hinausgehen. | |
| `reasoning.*` (alle 6 Felder) | PROFIL | letzter gesetzter Wert gewinnt | Reasoning-Effort ist ein Kosten-/Geschwindigkeits-Kompromiss, keine Zugriffsbeschränkung. | |
| `guards.enabled` | GLOBAL (Baseline) + Profil darf nur verschärfen | **OR** (Wert `true` gewinnt) | Sicherheits-/Stabilitäts-Wächter; Abschalten wäre eine Lockerung, analog `tools.plan.validate_*`. | |
| `guards.repeated_failure_warn` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | Niedrigere Schwelle = empfindlicherer (strengerer) Wächter. | |
| `guards.repeated_failure_abort` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | s.o. | |
| `guards.no_progress_rounds_warn` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | s.o. | |
| `guards.no_progress_rounds_abort` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | s.o. | |
| `guards.plan_stale_rounds` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | s.o. | |

---

## 3. Projekt-Ebene

**Heute:** Eine dritte, **nicht vertraute** Ebene existiert bereits —
`restricted_repo` (`harw_home::LayerReport::untrusted_repo`, typischerweise
ein `.harw/` im geklonten Repository außerhalb der Nutzer-Kontrolle). Sie
wird **nach** allen vertrauten Layern verarbeitet
(`discover_config_with_restricted:728-731`) und läuft durch einen komplett
anderen Codepfad (`apply_restricted_layer`) als Home/Profil:

- Nur `config.toml` wird gelesen, ohne Symlinks zu folgen und mit
  1 MiB-Obergrenze (`read_restricted_file`, `discovery.rs:1025 ff.`,
  `MAX_RESTRICTED_CONFIG_BYTES`).
- Es werden **niemals** `providers/`, `models/`, `auth.toml`, `.env`,
  `mcps/`, `plugins/`, `skills/`, `agents/`, `channels/`, `[mcp_listener]`,
  `default_provider`/`default_model`, `active_agent_definition`,
  `[policy].default_visibility_scope`, `[session]`, `[tui]`, `[logging]`,
  `[mode]` oder `[web]` übernommen (Doku-Tabelle `discovery.rs:542-558`).
- Für die verbleibenden Felder gilt strikt **monoton verengend**: Vereinigung
  bei „mehr Pflicht“-Listen (`require_approval_for`), Schnittmenge bei
  Allowlists, Minimum bei positiven Zahlen-Obergrenzen (`min_positive`,
  `discovery.rs:1008-1014`, `0` aus dem Repo-Layer wird nie übernommen), AND
  bei „mehr Erlaubnis nötig“-Booleans, OR bei „mehr Prüfung“-Booleans.
- Nur neun `HarnessConfig`-Felder sind überhaupt abgedeckt (Abschnitt 1: die
  mit **RESTRICTED-VORBILD** markierten Zeilen), plus je drei/vier Felder in
  `[network]`/`[browser]`/`[dod]`. `[mcp_listener]`, `[sandbox]`,
  `[permissions]`, `[guards]`, `[compaction]`, `[reasoning]` sind **überhaupt
  nicht** erfasst — auch nicht gegen den nicht vertrauten Projekt-Layer.

**Einordnung im neuen GLOBAL/PROFIL-Modell (Vorschlag):** Die Projekt-Ebene
sollte konzeptionell **unterhalb** von PROFIL stehen und pauschal denselben
„darf nur verengen“-Vertrag erhalten, den GLOBAL gegenüber PROFIL bekommt
(Abschnitt 2) — mit der zusätzlichen Verschärfung, dass die Projekt-Ebene
(nicht vertraut) niemals irgendein `OFFEN (sicherheitskritisch)`-Feld
(`permissions.allow`, `permissions.extra_roots`, `sandbox.*`,
`mcp_listener.*`) beeinflussen darf, während PROFIL (vertraut) dort
zumindest eine Teilmenge referenzieren könnte, je nach Nutzerentscheidung zu
den OFFEN-Punkten. Die bestehende `apply_restricted_layer`-Maschinerie ist
ein direktes Vorbild für die Umsetzung (Abschnitt 5), deckt aber aktuell nur
9 von 89 Feldern ab — eine Erweiterung auf alle als „GLOBAL, Profil darf nur
verschärfen“ vorgeschlagenen Felder wäre konsequent, wurde aber bisher nicht
umgesetzt.

**Nachtrag (2026-09-21):** Die oben als „je nach Nutzerentscheidung zu den
OFFEN-Punkten“ offen gelassene Frage ist entschieden — siehe Abschnitt 2
(alle elf Zeilen aufgelöst) und Abschnitt 7d für die konkrete Einordnung der
Projekt-Ebene unterhalb PROFIL.

---

## 4. Heute verlorene Einstellungen

Jedes Feld aus Abschnitt 1 **außer** den fünf explizit behandelten
(`default_provider`, `default_model`, `active_uia_definition`, `onboarding`,
`internal_models`) geht verloren, sobald ein späterer vertrauter Layer eine
`config.toml` schreibt, die dieses Feld nicht selbst enthält — der Wert
fällt auf den Section-Default zurück statt auf den Wert des vorigen Layers.
Da `ensure_home` die Profil-`config.toml` **immer** anlegt (mit fest
codiertem `[mcp_listener]`-Block, `harw-home/src/scaffold.rs:213-225`),
betrifft dies praktisch jede Installation, sobald die Home-Ebene
(`~/.harw/config.toml`) irgendeines der folgenden Felder abweichend vom
Default setzt:

- **Gesamte Top-Level-Skalare:** `config_version`, `workspace_root`,
  `active_agent_definition`, `uia_provider`, `uia_model`, `policy_profile`,
  `project_root_markers`.
- **`[logging]`, `[tui]`, `[session]`:** komplett (z. B. ein global gesetztes
  `logging.level = "debug"` verschwindet wieder, sobald das Profil ein
  `[mcp_listener]` schreibt, ohne `[logging]` zu wiederholen — exakt der vom
  Nutzer beschriebene Fall, nur am Beispiel `logging` statt `policy`).
- **`[policy]`:** `default_visibility_scope` **und** — das vom Nutzer
  genannte Beispiel — `require_approval_for`. Eine global gesetzte
  Freigabepflicht (z. B. `require_approval_for = ["shell.exec"]`) fällt auf
  `[]` zurück, sobald die Profil-Ebene `[policy]` nicht erneut schreibt.
- **`[mcp_listener]`:** komplett, inkl. `principals` (Credential-Referenzen!)
  — praktisch irrelevant hier, da die Profil-Vorlage die Sektion ohnehin
  immer selbst setzt, aber ein Admin, der auf Home-Ebene zusätzliche
  `principals` einträgt, verliert sie sofort, sobald das Profil sein eigenes
  `[mcp_listener]` (ohne diese Principals) schreibt.
- **`[tools.plan]`, `[mode]`, `[research]`, `[permissions]`, `[sandbox]`:**
  komplett — inklusive sicherheitskritischer Felder wie
  `permissions.allow`/`deny`/`extra_roots` und `sandbox.cargo`/`sandbox.tmux`
  (Vertrauensanker für die Sandbox).
- **`[compaction]`, `[reasoning]`, `[guards]`:** komplett.

Nicht betroffen (weil ganz oder teilweise gegen genau dieses Problem
abgesichert): `default_provider`, `default_model`, `active_uia_definition`,
`onboarding.*` (als Ganzes), `internal_models.*` (pro Modellstelle) sowie
— strukturell anders, weil außerhalb von `HarnessConfig` geparst —
`[network]`, `[browser]`, `[dod]`, `[web]` (diese bleiben "sticky": ein
Layer, der die Sektion nicht schreibt, überschreibt den vorigen Wert nicht,
`discovery.rs:607-624`).

---

## 5. Umsetzungsskizze

1. Geltungsbereich pro Feld deklarieren, nicht als Rust-Attribut (verstreut,
   schwer auditierbar), sondern als **eine zentrale Tabelle**
   (`FieldScope { path: &str, scope: Global|Profile, merge: LastWins|Union|
   Intersection|Min|Max|And|Or|GlobalOnly }`) in `harw-config`, analog zu
   `NEW_SECTION_KEYS`/`ALLOWED_MODES`-Konstanten — ein Review-Ort für jede
   künftige Feld-Ergänzung.
2. `discover_config_with_restricted` ersetzt Schritt 3 (Zeile 649,
   `resolved.harness = cfg`) durch ein generisches
   `merge_layer_into(&mut resolved.harness, cfg, &fields, layer_kind)`, das
   pro Tabelleneintrag die passende Regel anwendet statt der heutigen
   Fünf-Feld-Sonderliste — Struktur bereits vorhanden in
   `merge_restricted_harness` (`discovery.rs:942-1003`), nur generalisiert
   und für **jeden** vertrauten Layer angewendet, nicht nur für den
   untrusted Projekt-Layer.
3. `apply_restricted_layer` wird zu einem Spezialfall derselben Funktion mit
   `layer_kind = Project` und zusätzlich schärferer Policy für die
   `OFFEN (sicherheitskritisch)`-Felder (Abschnitt 3).
4. Beim Laden warnen (`tracing::warn!`), wenn ein PROFIL- oder
   Projekt-Layer versucht, ein `GlobalOnly`/„nur verschärfen“-Feld zu
   lockern (Wert nach Merge-Regel abgelehnt statt übernommen) — Diagnose
   analog zu [`ConfigDiagnostic`] (`discovery.rs:34-63`), nicht fatal, aber
   sichtbar in `resolved.diagnostics`.

---

## 6. MergeRule-Taxonomie

### 6.1 Das `MergeRule`-Enum

Zehn Varianten decken alle 89 Felder exakt ab (Herleitung und Feldzahl je
Variante: Abschnitt 6.3). Keine weitere Variante (z. B. ein separates
`MaxBound`) wird gebraucht — jeder Fall „profil darf nur *mehr* erlauben“
ist bereits `Union`/`OrBool`, jeder Fall „profil darf nur *weniger*
erlauben“ bereits `Intersection`/`MinBound`/`AndBool`.

```rust
/// Legt fest, wie ein einzelnes `HarnessConfig`-Blattfeld über die
/// vertrauten Layer (Home → aktives Profil) hinweg zusammengeführt wird,
/// und — mit denselben Varianten, aber eingeschränkter Anwendung (siehe
/// Abschnitt 7c) — gegen einen nicht vertrauten Projekt-Layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRule {
    /// Der zuletzt **explizit gesetzte** Wert gewinnt. Ein Layer, der das
    /// Feld nicht setzt, lässt den Wert des vorigen Layers unverändert
    /// stehen (kein Reset auf den Section-Default — behebt die
    /// „ERSETZT“-Regression aus Abschnitt 4 für alle so eingestuften
    /// Felder). Nie vom nicht vertrauten Projekt-Layer angewendet.
    ProfileReplaces,
    /// Nur der Wert des vertrauten Home-Layers gilt je. Ein späterer Layer
    /// (Profil **oder** nicht vertrautes Projekt), der einen *anderen*
    /// Wert setzt, wird ignoriert und löst eine [`ScopeDiagnostic`]-Warnung
    /// aus; denselben Wert erneut zu setzen ist ein stiller No-op.
    GlobalOnly,
    /// Vereinigung aller Layer, die das Feld setzen (einschränkende
    /// Listen: mehr Einträge sind immer sicher — z. B. mehr
    /// Freigabepflichten).
    Union,
    /// Schnittmenge aller Layer, die das Feld setzen, mit dem Home-Wert als
    /// Startmenge (rechte-erweiternde Listen: ein späterer Layer kann nur
    /// Einträge entfernen, niemals welche hinzufügen, die im Home-Layer
    /// fehlen). Der Vergleichsschlüssel für „ist derselbe Eintrag" ist pro
    /// Feld in `FIELD_TABLE`/der jeweiligen Merge-Doku festgelegt: für
    /// Listen von Primitivwerten (`String`, `PathBuf`) ist er implizit die
    /// volle Wertgleichheit; für Listen von Structs kann er entweder die
    /// volle Struct-Gleichheit sein (z. B. `RuleToml` als vollständiges
    /// `(tool, pattern)`-Tupel, Abschnitt 6.3/1.11 — kein Teilabgleich nur
    /// über `tool`) oder ein einzelnes Identitätsfeld (z. B.
    /// `McpPrincipalToml` über `id` allein, Abschnitt 6.3/1.6, entschieden
    /// 2026-09-21/R2). Bei einem engeren Identitätsfeld gewinnt für die
    /// überlebenden Einträge immer die vollständige Home-Fassung des
    /// Elements — die übrigen Felder eines übereinstimmenden Eintrags
    /// werden nie aus einem späteren Layer übernommen, selbst wenn dieser
    /// Layer denselben Schlüssel mit abweichenden Werten erneut setzt (kein
    /// Feld-Merge innerhalb eines Listenelements). Keine separate
    /// `MergeRule`-Variante nötig — dieselbe `Intersection` deckt beide
    /// Spielarten ab, nur der Vergleichsschlüssel unterscheidet sich.
    Intersection,
    /// Numerische Obergrenze: effektiver Wert = Minimum aller Layer, die
    /// das Feld setzen (bestehende `min_positive`-Konvention aus
    /// `discovery.rs:1008-1014` wiederverwendet: `0`/der jeweilige
    /// Unset-Sentinel-Wert eines späteren Layers senkt die Obergrenze nie
    /// weiter).
    MinBound,
    /// Bool-Feld, bei dem `true` der **lockere/erlaubende** Wert ist:
    /// effektiv = UND-Verknüpfung aller Layer, die das Feld setzen. Ein
    /// späterer Layer darf nur abschalten (verschärfen), nie einschalten.
    AndBool,
    /// Bool-Feld, bei dem `true` der **strenge/sichere** Wert ist: effektiv
    /// = ODER-Verknüpfung aller Layer, die das Feld setzen. Ein späterer
    /// Layer darf nur einschalten (verschärfen), nie abschalten.
    OrBool,
    /// Ordinalwert mit expliziter, in `FieldScope::ordering` hinterlegter
    /// Strenge-Reihenfolge (strengster Wert zuerst). Effektiv = der
    /// strengste unter allen Layern, die das Feld setzen gesetzte Wert; ein
    /// Versuch, einen lockereren Wert zu wählen, wird ignoriert + gewarnt.
    StricterOf,
    /// Kein eigenständiges Merge: Dieses Feld reist nur als Teil eines
    /// umschließenden atomaren Werts (Listenelement oder Punkt-Struct), der
    /// selbst unter der Regel eines anderen Feldes gemergt wird. Existiert,
    /// damit der Exhaustivitäts-Test (Abschnitt 7a) auch solche Felder
    /// nachweislich erfasst.
    CompositeMember,
    /// Wird über Layer hinweg **nie** zusammengeführt: Jeder Layer prüft
    /// seinen eigenen Wert unabhängig gegen die unterstützte(n)
    /// Schema-Version(en) beim Laden dieser einen Datei. Nur für
    /// `config_version` verwendet.
    PerFileValidated,
}

/// Wo ein Feld herkommen darf, bevor `MergeRule` bestimmt, *wie* mehrere
/// Layer-Werte kombiniert werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Der Home-Layer (`~/.harw`) legt die Baseline fest; das Profil ist
    /// ihr untergeordnet (Details je nach `MergeRule`).
    Global,
    /// Betrifft nur das aktive Profil; die Profil-Ebene ersetzt dort den
    /// globalen Wert (`MergeRule::ProfileReplaces`).
    Profile,
    /// Kein Scope im GLOBAL/PROFIL-Sinn — aktuell nur `config_version`
    /// (`MergeRule::PerFileValidated`).
    NotScoped,
}

/// Ein Eintrag der zentralen Deklarationstabelle (Abschnitt 7a).
pub struct FieldScope {
    /// Gepunkteter `HarnessConfig`-Pfad, exakt wie in Abschnitt 1/6.3,
    /// z. B. `"mcp_listener.enabled"` oder
    /// `"internal_models.session_title"`.
    pub path: &'static str,
    pub scope: Scope,
    pub merge: MergeRule,
    /// Strenge-Reihenfolge für `MergeRule::StricterOf`, strengster Wert
    /// zuerst. `None` für jede andere Regel.
    pub ordering: Option<&'static [&'static str]>,
}
```

### 6.2 Explizite Strenge-Ordnungen für `StricterOf`

Nur zwei Felder nutzen `StricterOf`; beide Ordnungen sind bewusst **neu
festgelegt**, da sie im heutigen Code an keiner Stelle kodiert sind:

- **`permissions.default_mode`**: `["ask", "auto", "full"]` (Index 0 =
  strengster Wert). `ask` fragt bei jeder Aktion nach — keine automatische
  Freigabe möglich, also die sicherste Einstellung. `full` fragt nie nach —
  die offenste Einstellung. `auto` liegt dazwischen (fragt nur, wo
  `policy.require_approval_for`/`permissions.deny` es verlangen). Quelle der
  Wertemenge: `ALLOWED_MODES` (`permissions_toml.rs:17`); die Reihenfolge
  dort ist rein deklarativ und trägt heute keine Bedeutung — dieses Dokument
  legt die Strenge-Ordnung hiermit erstmals normativ fest.
- **`policy.default_visibility_scope`**: `["self", "everyone"]` (Index 0 =
  strengster Wert), aber **nur eine Teilordnung**: Dies sind die einzigen
  beiden im Code belegten Werte (`default_visibility_scope()` liefert
  `"self"`, `harness_config.rs:438-440`; `"everyone"` taucht als einziger
  Alternativwert in einem Testfixture auf, `discovery.rs:1428`). Es gibt
  **keine** `validate()`-Einschränkung auf eine feste Wertemenge (anders als
  bei `permissions.default_mode`), also auch keine vollständige Ordnung für
  einen beliebigen dritten String. **Entschieden (2026-09-21, Risiko R1,
  Abschnitt 8):** Die Ordnung bleibt exakt diese Zweier-Teilordnung; für
  jeden Wert außerhalb von `{"self", "everyone"}` fällt `stricter_of` auf
  `GlobalOnly`-Verhalten zurück (kein Vergleichsversuch — der abweichende
  Layer-Wert wird ignoriert und löst eine `ScopeDiagnostic` aus), damit ein
  dritter Wert nie stillschweigend in die Ordnung einsortiert wird;
  abgesichert durch Test #24 (Abschnitt 7h).

### 6.3 Vollständige Feld-für-Feld-Zuordnung (alle 89 Felder)

Eine Zeile je Blattfeld aus Abschnitt 1, in derselben Reihenfolge und mit
denselben Unterabschnittsnummern, damit die Tabelle 1:1 gegen Abschnitt 1
geprüft werden kann. „Scope" = `Scope`-Variante, „Regel" = `MergeRule`-
Variante.

**1.1 Top-Level** (11 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `config_version` | NotScoped | `PerFileValidated` |
| `workspace_root` | Profile | `ProfileReplaces` |
| `default_provider` | Profile | `ProfileReplaces` |
| `default_model` | Profile | `ProfileReplaces` |
| `active_agent_definition` | Profile | `ProfileReplaces` |
| `active_uia_definition` | Profile | `ProfileReplaces` |
| `uia_provider` | Profile | `ProfileReplaces` |
| `uia_model` | Profile | `ProfileReplaces` |
| `uia_worker_model` | Profile | `ProfileReplaces` |
| `policy_profile` | Global | `GlobalOnly` |
| `project_root_markers` | Profile | `ProfileReplaces` |

**1.2 `[logging]`** (3): `logging.level`, `logging.target_module_paths`,
`logging.json` — alle Profile / `ProfileReplaces`.

**1.3 `[tui]`** (2): `tui.theme`, `tui.keybindings_file` — alle Profile /
`ProfileReplaces`.

**1.4 `[session]`** (5 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `session.store_dir` | Profile | `ProfileReplaces` |
| `session.journal_format` | Profile | `ProfileReplaces` |
| `session.retention_days` | Global | `MinBound` |
| `session.title_generation` | Profile | `ProfileReplaces` |
| `session.title_model` | Profile | `ProfileReplaces` |

**1.5 `[policy]`** (2 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `policy.default_visibility_scope` | Global | `StricterOf` (`["self","everyone"]`, Teilordnung, s. 6.2) |
| `policy.require_approval_for` | Global | `Union` |

**1.6 `[mcp_listener]`** (9 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `mcp_listener.enabled` | Global | `AndBool` (`true` = Listener aktiv = lockerer Wert) |
| `mcp_listener.listen_addr` | Global | `GlobalOnly` |
| `mcp_listener.path` | Global | `GlobalOnly` |
| `mcp_listener.principals` | Global | `Intersection` (Vergleichsschlüssel: `id` allein, nicht der volle Struct — entschieden 2026-09-21, Risiko R2; bis dahin galt hier `GlobalOnly`) |
| `mcp_listener.principals[].id` | Global | `CompositeMember` (Identitätsfeld des `Intersection`-Vergleichs von `mcp_listener.principals`, s. o.) |
| `mcp_listener.principals[].credential_ref` | Global | `CompositeMember` (reist mit dem Element; bei übereinstimmender `id` gewinnt immer die globale Fassung, kein Feld-Merge) |
| `mcp_listener.principals[].tenant` | Global | `CompositeMember` (s. o.) |
| `mcp_listener.principals[].workspace` | Global | `CompositeMember` (s. o.) |
| `mcp_listener.principals[].job_capabilities` | Global | `CompositeMember` (s. o.) |

**1.7 `[onboarding]`** (4 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `onboarding.seen` (Tabelle) | Profile | `ProfileReplaces` — **auf Section-Ebene atomar**, wie heute (`discovery.rs:640-642`): nicht pro Flag, sondern die ganze `OnboardingSeen`-Struktur wird gemeinsam ersetzt, wenn der Layer `[onboarding]` überhaupt enthält |
| `onboarding.seen.provider` | Profile | `CompositeMember` (Teil der atomaren `onboarding.seen`-Struktur, s. o.) |
| `onboarding.seen.model` | Profile | `CompositeMember` |
| `onboarding.seen.channel` | Profile | `CompositeMember` |

**1.8 `[tools.plan]`** (9 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `tools.plan.enabled` | Profile | `ProfileReplaces` |
| `tools.plan.persist` | Profile | `ProfileReplaces` |
| `tools.plan.require_for_complex_work` | Profile | `ProfileReplaces` |
| `tools.plan.validate_dependency_cycles` | Global | `OrBool` (`true` = Prüfung aktiv = strenger Wert) |
| `tools.plan.validate_write_conflicts` | Global | `OrBool` |
| `tools.plan.max_nodes` | Global | `MinBound` |
| `tools.plan.require_exploration_for` | Profile | `ProfileReplaces` |
| `tools.plan.exploration_ttl_secs` | Profile | `ProfileReplaces` |
| `tools.plan.max_expand_depth` | Global | `MinBound` |

**1.9 `[mode]`** (1): `mode.default` — Profile / `ProfileReplaces`.

**1.10 `[research]`** (5 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `research.network_allow_hosts` | Global | `Intersection` |
| `research.cargo_registry_read` | Global | `AndBool` (`true` = Lesezugriff erlaubt = lockerer Wert) |
| `research.max_fetch_bytes` | Global | `MinBound` |
| `research.fetch_timeout_secs` | Global | `MinBound` |
| `research.cache_ttl_secs` | Profile | `ProfileReplaces` |

**1.11 `[permissions]`** (7 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `permissions.default_mode` | Global | `StricterOf` (`["ask","auto","full"]`, s. 6.2) |
| `permissions.approval_timeout_secs` | Global | `MinBound` |
| `permissions.allow` | Global | `Intersection` |
| `permissions.deny` | Global | `Union` |
| `permissions.extra_roots` | Global | `Intersection` |
| `RuleToml.tool` (Feld von `allow[]`/`deny[]`) | — | `CompositeMember` (reist als Teil des `RuleToml`-Listenelements unter der Regel der jeweiligen Liste — `Intersection` bei `allow`, `Union` bei `deny`; Vergleichsschlüssel ist das vollständige `(tool, pattern)`-Tupel) |
| `RuleToml.pattern` | — | `CompositeMember` |

**1.12 `[sandbox]`** (8 Felder) — alle Global / `GlobalOnly`:
`sandbox.cargo`, `sandbox.cargo.mode`, `sandbox.cargo.cargo_bin`,
`sandbox.cargo.rustup_home`, `sandbox.cargo.cargo_home`, `sandbox.tmux`,
`sandbox.tmux.mode`, `sandbox.tmux.socket_path`.

**1.13 `[internal_models]`** (10 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `internal_models.use_openrouter_defaults` | Profile | `ProfileReplaces` |
| `internal_models.session_title` | Profile | `ProfileReplaces` (atomar pro Stelle, wie heute `merge_internal_models`) |
| `internal_models.compaction_summary` | Profile | `ProfileReplaces` |
| `internal_models.memory_consolidation` | Profile | `ProfileReplaces` |
| `internal_models.dream_reflection` | Profile | `ProfileReplaces` |
| `internal_models.explorer` | Profile | `ProfileReplaces` |
| `internal_models.research` | Profile | `ProfileReplaces` |
| `internal_models.worker_simple` | Profile | `ProfileReplaces` |
| `internal_models.worker_complex` | Profile | `ProfileReplaces` |
| `InternalModelChoice.provider`/`.model` (Feld jeder Stelle) | — | `CompositeMember` (reist als Teil der atomaren `Option<InternalModelChoice>` der jeweiligen Stelle, s. o.; nie einzeln gemergt) |

**1.14 `[compaction]`** (1): `compaction.absolute_ceiling_tokens` — Global /
`MinBound`.

**1.15 `[reasoning]`** (6): `reasoning.uia`, `.root_orchestrator`,
`.root_orchestrator_with_subs`, `.sub_orchestrator`, `.worker_complex`,
`.worker_simple` — alle Profile / `ProfileReplaces`.

**1.16 `[guards]`** (6 Felder)

| Pfad | Scope | Regel |
|---|---|---|
| `guards.enabled` | Global | `OrBool` (`true` = Wächter aktiv = strenger Wert) |
| `guards.repeated_failure_warn` | Global | `MinBound` |
| `guards.repeated_failure_abort` | Global | `MinBound` |
| `guards.no_progress_rounds_warn` | Global | `MinBound` |
| `guards.no_progress_rounds_abort` | Global | `MinBound` |
| `guards.plan_stale_rounds` | Global | `MinBound` |

**Verteilung (Kontrollsumme = 89, Stand 2026-09-21 nach R1/R2-Entscheidung
plus Ergänzung `uia_worker_model`):**
`ProfileReplaces` 41 · `GlobalOnly` 11 · `MinBound` 12 · `CompositeMember` 11 ·
`Intersection` 4 · `OrBool` 3 · `Union` 2 · `AndBool` 2 · `StricterOf` 2 ·
`PerFileValidated` 1. (Vor der R2-Entscheidung: `GlobalOnly` 17 ·
`CompositeMember` 6 · `Intersection` 3 — die Ummappung von
`mcp_listener.principals` auf `Intersection` zieht zwingend auch dessen fünf
Unterfelder von `GlobalOnly` auf `CompositeMember`, da sie laut
`CompositeMember`-Definition (Abschnitt 6.1) nicht eigenständig gemergt
werden dürfen, sondern nur als Teil des durch `Intersection` gemergten
Listenelements reisen — netto `GlobalOnly` −6, `CompositeMember` +5,
`Intersection` +1, Summe unverändert 88 zu diesem Zeitpunkt. Die spätere
Ergänzung von `uia_worker_model` als `ProfileReplaces` erhöht die Summe auf
89 und `ProfileReplaces` von 40 auf 41, ohne die übrigen Varianten zu
berühren.)

---

## 7. Umsetzungsspezifikation (verbindlich)

Operationalisiert die Skizze aus Abschnitt 5 vollständig; wo sich beide
widersprechen, gilt dieser Abschnitt.

### 7a. Wo der Geltungsbereich deklariert wird

**Eine zentrale, öffentliche Tabelle** `FIELD_TABLE: &[FieldScope]` in
`harw-config/src/scope.rs` (Typen: Abschnitt 6.1) — kein verstreutes
Rust-Attribut pro Feld, damit ein Review-Ort für jede künftige
Feld-Ergänzung existiert.

**Exhaustivitäts-Garantie:** Eine neue `HarnessConfig`- oder
Section-Struct-Feld ohne `FIELD_TABLE`-Eintrag löst einen **Compile-Fehler**
aus (E0027 „pattern does not mention field"), nicht nur einen
Test-Fehlschlag zur Laufzeit. Mechanismus: für `HarnessConfig` selbst sowie
für **jede** Section-Struct mit benannten Feldern (`LoggingSection`,
`TuiSection`, `SessionSection`, `PolicySection`, `McpListenerSection`,
`McpPrincipalToml`, `OnboardingSection`, `OnboardingSeen`, `ToolsSection`
(bzw. `PlanToml`), `ModeSection`, `ResearchSection`, `PermissionsSection`,
`RuleToml`, `SandboxSection`, `CargoSandboxToml`, `TmuxSandboxToml`,
`InternalModelsToml`, je Modellstelle `InternalModelChoice`,
`CompactionToml`, `ReasoningWeightsToml`, `GuardsToml`) gibt es einen Test
der Form:

```rust
#[test]
fn test_field_table_exhaustive_harness_config() {
    let HarnessConfig {
        config_version,
        workspace_root,
        default_provider,
        default_model,
        active_agent_definition,
        active_uia_definition,
        uia_provider,
        uia_model,
        uia_worker_model,
        policy_profile,
        logging: _,
        tui: _,
        session: _,
        policy: _,
        mcp_listener: _,
        onboarding: _,
        tools: _,
        mode: _,
        research: _,
        permissions: _,
        sandbox: _,
        project_root_markers,
        internal_models: _,
        compaction: _,
        reasoning: _,
        guards: _,
        base_dir: _, // #[serde(skip)], kein TOML-Feld, keine FIELD_TABLE-Zeile
    } = HarnessConfig::default();
    // Kein `..` — ein neues Feld auf HarnessConfig, das hier nicht
    // aufgeführt wird, ist ein Compile-Fehler (E0027), keine Laufzeitprobe.
    let _ = (
        config_version, workspace_root, default_provider, default_model,
        active_agent_definition, active_uia_definition, uia_provider,
        uia_model, uia_worker_model, policy_profile, project_root_markers,
    ); // unused-Warnungen vermeiden
    for path in [
        "config_version", "workspace_root", "default_provider",
        "default_model", "active_agent_definition", "active_uia_definition",
        "uia_provider", "uia_model", "uia_worker_model", "policy_profile",
        "project_root_markers",
    ] {
        assert!(
            FIELD_TABLE.iter().any(|f| f.path == path),
            "FIELD_TABLE fehlt Eintrag für {path}"
        );
    }
}
```

Dasselbe Muster (Struct-Destructuring ohne `..` + Pfad-Assert-Schleife) für
jede der oben gelisteten Section-Structs, mit dem jeweils vollständig
gepunkteten Pfad (z. B. `"sandbox.cargo.mode"`, `"internal_models.session_title"`,
`"mcp_listener.principals"` — für `Vec`-/`Option`-Felder wird nur der
Container-Pfad destrukturiert, nicht seine Elemente; Elemente wie
`RuleToml`/`McpPrincipalToml`/`InternalModelChoice` bekommen ihre eigene,
separate Destructuring-Test-Funktion). Alle diese Tests leben in **Paket C**
(Abschnitt 7g), nicht verteilt über die Produktivdateien.

### 7b. Neue Dateien, Module, Typen, Funktionssignaturen

**Neu:** `harw-config/src/scope.rs` — `MergeRule`, `Scope`, `FieldScope`,
`FIELD_TABLE` (Abschnitt 6.1/6.3), rein deklarativ, keine Merge-Logik.

**Neu:** `harw-config/src/merge.rs` — die generische Merge-Engine:

```rust
/// Grober Vertrauens-/Ebenen-Kontext dieses Merge-Aufrufs (bestimmt, welche
/// `MergeRule`-Varianten überhaupt wirken — Tabelle in Abschnitt 7c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    /// Der erste vertraute Layer (`~/.harw`, `layer_index == 0`). Jede
    /// Regel verhält sich hier identisch zu `ProfileReplaces` — es gibt
    /// noch keinen GLOBAL-Vorzustand, gegen den verengt werden könnte.
    Baseline,
    /// Jeder weitere vertraute Layer (aktives Profil, `layer_index >= 1`).
    Refinement,
    /// Der nicht vertraute Projekt-Layer aus `apply_restricted_layer`.
    UntrustedProject,
}

/// Modelliert auf [`ConfigDiagnostic`] (nicht-fatal, sichtbar aber
/// blockiert den Start nicht), trägt aber die für eine
/// Scope-Verletzung nötigen Zusatzfelder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDiagnostic {
    /// Gepunkteter Feldpfad, z. B. `"mcp_listener.enabled"`.
    pub field: String,
    /// `config.toml`, die den Lockerungsversuch enthielt.
    pub file: String,
    /// Der verworfene Wert, `Debug`-formatiert. Nie ein Secret: jedes
    /// Feld, das eine `ScopeDiagnostic` auslösen kann, ist ein
    /// nicht-geheimer Skalar/Enum/Pfad/Regel-Eintrag — `credential_ref`
    /// nimmt nie teil, da `mcp_listener.principals` `GlobalOnly` ist.
    pub rejected_value: String,
}

impl std::fmt::Display for ScopeDiagnostic { /* "{field} in {file}: …" */ }

/// Wendet `incoming` (bereits vollständig deserialisierte
/// `HarnessConfig` dieses Layers, inkl. dessen eigener Section-Defaults für
/// alles, was der Layer nicht selbst setzt) gemäß `FIELD_TABLE` und `role`
/// auf `trusted` (der bisher akkumulierte Stand) an. `raw` ist das
/// **rohe**, noch nicht `strip_new_sections`-bereinigte `toml::Value`
/// dieses Layers und entscheidet je Feld per Präsenzprüfung (Muster
/// `field_present`, `discovery.rs:872-878`, wiederverwendet), ob dieser
/// Layer das Feld überhaupt selbst gesetzt hat.
///
/// Ersetzt `resolved.harness = cfg` (`discovery.rs:649`) für vertraute
/// Layer und `merge_restricted_harness` (`discovery.rs:942-1003`) für den
/// nicht vertrauten Projekt-Layer — beide Aufrufer unterscheiden sich nur
/// im übergebenen `role`.
///
/// # Returns
/// Jede `ScopeDiagnostic`, die durch einen abgelehnten
/// Lockerungs-/Erweiterungsversuch entstanden ist (Abschnitt 7c/7e). Leer,
/// wenn kein Layer-Wert verworfen wurde. `tracing::warn!` wird zusätzlich
/// synchron für jeden Eintrag ausgelöst — der Rückgabewert ist für
/// `ResolvedConfig::scope_warnings` und Tests gedacht, nicht der einzige
/// Sichtbarkeitskanal.
pub fn merge_layer_into(
    trusted: &mut HarnessConfig,
    incoming: HarnessConfig,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) -> Vec<ScopeDiagnostic>;
```

Intern ist `merge_layer_into` **kein** reflektierendes Generikum — Rust hat
keine Laufzeit-Reflection über heterogene Struct-Felder. Es ist eine
Sequenz handgeschriebener Sektions-Helfer (`merge_top_level`,
`merge_logging`, `merge_tui`, `merge_session`, `merge_policy`,
`merge_mcp_listener`, `merge_onboarding`, `merge_tools_plan`, `merge_mode`,
`merge_research`, `merge_permissions`, `merge_sandbox`, `merge_internal_models`
(erweitert die bestehende Funktion, s. u.), `merge_compaction`,
`merge_reasoning`, `merge_guards`), jeweils
`fn(trusted: &mut X, incoming: X, raw: &toml::Value, role: LayerRole) -> Vec<ScopeDiagnostic>`,
die für jedes ihrer Felder die passende **generische Regel-Anwendung**
aufrufen — einmal in `merge.rs` definiert, von allen Sektions-Helfern
wiederverwendet:

```rust
fn profile_replaces<T: Clone>(trusted: &mut T, incoming: T, present: bool, role: LayerRole);
fn global_only<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut T, incoming: T, present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
fn union_list<T: Clone + PartialEq>(trusted: &mut Vec<T>, incoming: &[T], present: bool, role: LayerRole);
fn intersection_list<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut Vec<T>, incoming: &[T], present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
fn min_bound<T: Ord + Default + Copy + std::fmt::Debug>(
    trusted: &mut T, incoming: T, present: bool, role: LayerRole,
    field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
); // nutzt intern das bestehende `min_positive` (discovery.rs:1008-1014)
fn and_bool(trusted: &mut bool, incoming: bool, present: bool, role: LayerRole, field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>);
fn or_bool(trusted: &mut bool, incoming: bool, present: bool, role: LayerRole, field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>);
fn stricter_of(
    trusted: &mut String, incoming: String, present: bool, role: LayerRole,
    ordering: &[&str], field: &str, layer_path: &Path, out: &mut Vec<ScopeDiagnostic>,
);
```

Jeder Helfer bekommt `present` bereits von seinem Aufrufer über
`field_present(raw, &[...])` berechnet (kein Helfer liest `raw` selbst).
Bei `role == LayerRole::Baseline` geben **alle** Helfer sofort
`profile_replaces`-Verhalten zurück (Abschnitt 7c) — implementiert als
früher `if role == LayerRole::Baseline { ...; return; }` am Kopf jedes
Helfers außer `profile_replaces` selbst (das ist für `Baseline` ohnehin
identisch).

**Ersetzung von `discovery.rs:649`:**

```rust
// vorher (discovery.rs:648-649):
// cfg.base_dir = Some(base.clone());
// resolved.harness = cfg;

// nachher:
cfg.base_dir = Some(base.clone());
let role = if layer_index == 0 { LayerRole::Baseline } else { LayerRole::Refinement };
let mut warnings = merge_layer_into(&mut resolved.harness, cfg, &fields, role, base);
resolved.scope_warnings.append(&mut warnings);
```

Die fünf Sonderfall-`if`-Blöcke (Zeilen 631-647) und `merge_internal_models`
entfallen an dieser Stelle **nicht ersatzlos** — sie werden zu den
konkreten `ProfileReplaces`-Implementierungen für genau diese fünf Felder
innerhalb von `merge_top_level`/`merge_onboarding`/`merge_internal_models`
(Details: Abschnitt 7f). `merge_internal_models` selbst bleibt als
Funktion bestehen (Signatur unverändert) und wird von `merge.rs`s
`merge_internal_models`-Sektionshelfer aufgerufen statt inline in der
Discovery-Schleife.

`ResolvedConfig` (Typdefinition in `discovery.rs`) bekommt ein neues
öffentliches Feld:

```rust
/// Abgelehnte Scope-Lockerungsversuche aus `merge_layer_into`
/// (Abschnitt 7e), gesammelt über alle Layer inkl. des nicht vertrauten
/// Projekt-Layers. Getrennt von `diagnostics` (hängende Modell-/
/// Provider-Referenzen) gehalten, da semantisch verschieden — beide sind
/// nicht-fatal.
pub scope_warnings: Vec<ScopeDiagnostic>,
```

### 7c. Verhältnis zu `merge_restricted_*`

**`merge_restricted_network`/`_browser`/`_dod`** (`discovery.rs:883-939`)
bleiben **unverändert bestehen** — sie behandeln `[network]`/`[browser]`/
`[dod]`, die außerhalb von `HarnessConfig` liegen (Abschnitt 1, Randnotiz)
und damit außerhalb des Geltungsbereichs dieser Spezifikation. Eine
künftige Vereinheitlichung unter derselben `MergeRule`-Engine wäre
konsequent, ist aber **nicht** Teil dieser Arbeitspakete (siehe Risiko R3,
Abschnitt 8).

**`merge_restricted_harness`** (`discovery.rs:942-1003`) wird **entfernt**
und durch denselben `merge_layer_into`-Aufruf ersetzt, den auch die
vertrauten Layer nutzen — `apply_restricted_layer` ruft ihn mit
`role = LayerRole::UntrustedProject` auf, statt eine eigene 62-Zeilen-Funktion
zu pflegen. `min_positive` wird aus `merge.rs` heraus weiterhin von dort
importiert (nicht dupliziert).

Damit eine bestehende Schutzwirkung **nicht** verloren geht, gilt je
`MergeRule`-Variante für `LayerRole::UntrustedProject` exakt dieselbe
Anwendung wie für `LayerRole::Refinement` (trusted Profil), **mit genau
zwei Ausnahmen**, die beide bereits identisch für Refinement/Baseline
gelten:

| `MergeRule` | `Refinement` (Profil) | `UntrustedProject` |
|---|---|---|
| `ProfileReplaces` | Wert übernehmen, wenn gesetzt, sonst `trusted` behalten | **nie angewendet** — Feld bleibt unberührt (entspricht der heutigen Ausschlussliste `discovery.rs:542-558`: `default_provider`/`default_model`, `active_agent_definition`, `[session]`, `[tui]`, `[logging]`, `[mode]` etc.) |
| `GlobalOnly` | abweichender Wert ignoriert + gewarnt | identisch: abweichender Wert ignoriert + gewarnt |
| `Union`/`Intersection`/`MinBound`/`AndBool`/`OrBool`/`StricterOf` | wendet die Regel monoton verengend an (per Konstruktion niemals lockernd) | **identisch** — dieselbe monotone Regel; da jede dieser sechs Regeln per Konstruktion nur verengen kann, ist die Anwendung durch einen nicht vertrauten Layer ebenso sicher wie durch ein vertrautes Profil |
| `CompositeMember` | reist mit dem Elternwert | reist mit dem Elternwert |
| `PerFileValidated` | pro Datei geprüft | pro Datei geprüft (auch der Projekt-Layer darf keinen nicht unterstützten `config_version`-Wert einschleusen) |

**Konsequenz (bewusste, dokumentierte Erweiterung gegenüber heute):** Die
heutige Abdeckung von neun Feldern für den nicht vertrauten Layer
(`policy.require_approval_for`, 4× `research.*`, 4× `tools.plan.*`) wächst
auf **alle 25 Felder** mit `Union`/`Intersection`/`MinBound`/`AndBool`/
`OrBool`/`StricterOf` (Abschnitt 6.3-Kontrollsumme: 2+4+12+2+3+2 = 25, minus
die bereits neun abgedeckten = 16 neu geschützte Felder, u. a.
`permissions.deny`, `guards.*`, `session.retention_days`,
`compaction.absolute_ceiling_tokens`, `permissions.approval_timeout_secs`).
Das ist eine reine Erweiterung des Schutzes (nie eine Lockerung) und damit
mit der Vorgabe „keine bestehende Schutzwirkung darf verloren gehen"
vereinbar — sollte aber im Review explizit als beabsichtigte
Verhaltensänderung markiert werden, nicht als Nebeneffekt.

### 7d. Einordnung der vertrauten Projekt-Ebene

Es gibt in diesem Modell **keine eigene** „vertraute Projekt-Ebene" (anders
als der Wortlaut der Aufgabenstellung suggeriert) — die heutige
Implementierung kennt nur (1) vertraute Layer (`~/.harw` → aktives Profil,
iteriert als `layers: &[PathBuf]`) und (2) den einen nicht vertrauten
Projekt-Layer (`restricted_repo`). Eine „vertraute Projekt-Ebene" im
Sinne einer dritten, zusätzlichen `config.toml` (etwa ein vom Nutzer
selbst — nicht vom Repo — verwaltetes Projekt-Overlay) existiert im
heutigen Code nicht als eigener Begriff; **falls** eine solche Ebene
künftig eingeführt wird, ist der im Auftrag vorgeschlagene Ansatz
(„wie Profil, also unter global") direkt umsetzbar: Sie würde als
zusätzlicher Eintrag in `layers` mit `role = LayerRole::Refinement`
behandelt, genau wie eine zweite Profil-Ebene — die Engine unterscheidet
ohnehin nicht zwischen „Profil" und „vertrautes Projekt", beide sind
`Refinement` in Präzedenz-Reihenfolge. Diese Aussage ist eine
Bereitschaftserklärung für eine mögliche künftige Ebene, keine Umsetzung in
den vier Arbeitspaketen.

### 7e. Warnungsformat

`ScopeDiagnostic` (Abschnitt 7b) — modelliert auf dem bestehenden
`ConfigDiagnostic`-Muster (`discovery.rs:34-63`: nicht-fataler Fund mit
`Display`), aber mit den drei geforderten Feldern `field`, `file`,
`rejected_value` statt `site`/`kind`/`reference`.

**Auslöser:** jede `MergeRule`-Anwendung außer `ProfileReplaces` /
`CompositeMember` / `PerFileValidated`, bei der (a) der aktuelle Layer das
Feld laut `field_present` **explizit gesetzt** hat, **und** (b) der
resultierende effektive Wert **nicht** dem entspricht, was dieser Layer
gesetzt hat (die Regel hat seinen Versuch also verworfen oder begrenzt —
sei es eine `GlobalOnly`-Abweichung, ein `Intersection`/`Union`-Eintrag
außerhalb der erlaubten Richtung, eine `MinBound`-Überschreitung, ein
`AndBool`/`OrBool` in falscher Richtung, oder ein `StricterOf`-Wert
lockerer als der bisherige).

**Sofortige Sichtbarkeit:** jeder Fund wird **synchron** per
`tracing::warn!` mit strukturierten Feldern ausgelöst (Projekt-Konvention,
siehe `CLAUDE.md`-Tracing-Standards):

```rust
tracing::warn!(
    field = %diagnostic.field,
    file = %diagnostic.file,
    rejected_value = %diagnostic.rejected_value,
    "scope loosening attempt ignored"
);
```

**Gesammelte Sichtbarkeit:** zusätzlich landet jeder Fund in
`ResolvedConfig::scope_warnings` (Abschnitt 7b), unabhängig vom
Tracing-Log auswertbar (z. B. für eine künftige `harw doctor`-artige
Diagnose-Ausgabe oder für Tests).

### 7f. Die fünf heute schon übernommenen Sonderfälle

`default_provider`, `default_model`, `active_uia_definition`, `onboarding`,
`internal_models` sind alle als `ProfileReplaces` eingestuft (Abschnitt
6.3) — ihr heutiges Verhalten darf sich **nicht** ändern. Konkret:

- `default_provider`/`default_model`/`active_uia_definition`: Die
  `profile_replaces`-Implementierung für genau diese drei Top-Level-Felder
  muss das bestehende Muster (`if cfg.<feld>.is_none() { cfg.<feld> = resolved.harness.<feld>.clone() }`,
  `discovery.rs:631-639`) **wortgleich reproduzieren** — nicht durch eine
  neue, unabhängig geschriebene generische Implementierung ersetzen, deren
  Verhalten erst durch einen Test bewiesen werden müsste. Am einfachsten:
  `merge_top_level` ruft für diese drei Felder exakt diesen bestehenden
  Code auf (als kleine private Hilfsfunktion extrahiert, nicht neu
  geschrieben).
- `onboarding`: `merge_onboarding` reproduziert exakt
  `discovery.rs:640-642` (`if fields.get("onboarding").is_none() { cfg.onboarding = resolved.harness.onboarding.clone(); }`)
  — Section-Ebene, nicht Flag-Ebene (Abschnitt 6.3, 1.7).
- `internal_models`: `merge_internal_models` (bestehende Funktion,
  `discovery.rs:846-866`) wird **unverändert wiederverwendet**, nur der
  Aufrufort wandert von der Discovery-Schleife in den
  `merge_internal_models`-Sektionshelfer von `merge.rs`.

Alle **anderen** ~35 `ProfileReplaces`-Felder bekommen dagegen eine **neu
geschriebene** generische `profile_replaces`-Implementierung — für sie gab
es bisher **keinen** korrekten Code (sie waren als „ERSETZT" markiert,
Abschnitt 4); das ist die eigentliche Bugfix-Wirkung dieser Arbeit.

### 7g. Arbeitspakete (3, disjunkte Dateimengen)

| Paket | Dateien | Inhalt | Abhängigkeit |
|---|---|---|---|
| **A — Scope & Merge-Engine** | `harw-config/src/scope.rs` (neu), `harw-config/src/merge.rs` (neu), `harw-config/src/lib.rs` (2 Zeilen: `pub mod scope; pub mod merge;` + Re-Exports von `MergeRule`, `Scope`, `FieldScope`, `FIELD_TABLE`, `LayerRole`, `ScopeDiagnostic`, `merge_layer_into`) | `MergeRule`/`Scope`/`FieldScope`/`FIELD_TABLE` (Abschnitt 6.1/6.3) + `LayerRole`/`ScopeDiagnostic`/`merge_layer_into` + alle Sektions-Helfer + generischen Regel-Helfer (Abschnitt 7b) | keine |
| **B — Discovery-Integration** | `harw-config/src/discovery.rs` (nur diese Datei: `use`-Zeilen für `crate::scope::*`/`crate::merge::*` ergänzen; Ersetzung von Zeile 649, Abschnitt 7b; `apply_restricted_layer` auf `merge_layer_into(..., LayerRole::UntrustedProject, ...)` umstellen; `merge_restricted_harness` entfernen; `ResolvedConfig::scope_warnings`-Feld + Befüllung ergänzen, Abschnitt 7b) | Verdrahtung der Engine in den bestehenden Discovery-Ablauf | A |
| **C — Tests** | `harw-config/tests/config_scope_merge.rs` (neu; bei Bedarf zusätzlich `harw-config/tests/config_scope_exhaustive.rs`) | Alle Tests aus Abschnitt 7a (Exhaustivität) und 7h (Merge-Verhalten, Regression), ausschließlich über die öffentliche API (`FIELD_TABLE`, `merge_layer_into`, `discover_config_with_restricted`) — keine Änderung an den Dateien aus A/B nötig, da alle beteiligten Felder `pub` sind | A, B |

Reihenfolge zwingend A → B → C (jedes Paket braucht die fertige API des
vorigen). Kein Paket überschneidet sich mit einem anderen in der
Dateimenge.

### 7h. Testliste

**Pro `MergeRule`-Variante mindestens ein Test** (in `config_scope_merge.rs`,
über `merge_layer_into` direkt oder über zwei synthetische Layer):

1. `ProfileReplaces`: Profil setzt das Feld nicht → Home-Wert bleibt
   erhalten (nicht Default) — der eigentliche Kernbeweis für den Bugfix.
2. `ProfileReplaces` (Spezialfall `internal_models`): Profil setzt nur
   `session_title` → `compaction_summary` aus Home bleibt erhalten (Beweis,
   dass `merge_internal_models` unverändert eingebunden ist).
3. `GlobalOnly`: Profil versucht `sandbox.cargo.cargo_bin` zu ändern →
   Home-Wert bleibt effektiv, `ScopeDiagnostic` mit
   `field == "sandbox.cargo.cargo_bin"` erzeugt.
4. `Union`: Home setzt `require_approval_for = ["shell.exec"]`, Profil
   setzt `["fs.write"]` → effektiv beide Einträge vorhanden.
5. `Intersection`: Home setzt `permissions.allow = [A, B]`, Profil setzt
   `[A, C]` → effektiv nur `[A]` (C wird verworfen + `ScopeDiagnostic`).
6. `MinBound`: Home setzt `guards.repeated_failure_warn = 2`, Profil setzt
   `5` → effektiv `2`, `ScopeDiagnostic` erzeugt; Profil setzt `1` →
   effektiv `1`, keine Diagnostic.
7. `AndBool`: Home setzt `mcp_listener.enabled = false`, Profil setzt
   `true` → effektiv `false`, `ScopeDiagnostic` erzeugt.
8. `OrBool`: Home setzt `guards.enabled = true`, Profil setzt `false` →
   effektiv `true`, `ScopeDiagnostic` erzeugt.
9. `StricterOf`: Home setzt `permissions.default_mode = "auto"`, Profil
   setzt `"full"` → effektiv `"auto"`, `ScopeDiagnostic` erzeugt; Profil
   setzt `"ask"` → effektiv `"ask"`, keine Diagnostic.
10. `CompositeMember`: `RuleToml`-Element wird nur als Ganzes verglichen —
    zwei `allow`-Regeln mit gleichem `tool`, aber unterschiedlichem
    `pattern` gelten als **unterschiedliche** Einträge in der
    `Intersection` (kein teilweiser Abgleich nur über `tool`).
11. `PerFileValidated`: `config_version` eines Layers wird nicht vom
    vorigen Layer „geerbt" — zwei aufeinanderfolgende Layer mit
    unterschiedlichem `config_version` behalten je ihren eigenen Wert bis
    zur Prüfung (kein Merge-Effekt).

**Ein Test pro sicherheitskritischem (🔒) Feld — Lockerung wird ignoriert
und gewarnt** (mindestens folgende, je nach `MergeRule`-Form; für die
übrigen 🔒-Felder aus Abschnitt 2/6.3 gilt dasselbe Muster analog):

12. `mcp_listener.enabled`: Home `false` → Profil `true` verworfen (s. 7).
13. `mcp_listener.listen_addr`: Home `"127.0.0.1:1337"` → Profil
    `"0.0.0.0:1337"` verworfen + `ScopeDiagnostic`.
14. `mcp_listener.principals`: Profil versucht einen zusätzlichen Principal
    einzutragen → ignoriert + `ScopeDiagnostic`, Home-Liste unverändert
    (seit R2, entschieden 2026-09-21: Regel ist `Intersection` nach `id`,
    nicht mehr `GlobalOnly` — Entfernen- und Überschreiben-Fälle sind
    gesondert in Test 25–27 abgedeckt).
15. `permissions.allow`: s. 5.
16. `permissions.extra_roots`: Home `["/a"]`, Profil `["/a", "/b"]` →
    effektiv `["/a"]`, `ScopeDiagnostic` für `/b`.
17. `permissions.default_mode`: s. 9.
18. `sandbox.cargo.*`/`sandbox.tmux.*`: s. 3.
19. `research.network_allow_hosts`: Home `["docs.rs"]`, Profil
    `["docs.rs", "evil.example"]` → effektiv `["docs.rs"]`,
    `ScopeDiagnostic` für `evil.example`.

**Regressionstest** (in `config_scope_merge.rs`, über
`discover_config_with_restricted` mit zwei echten Temp-Layer-Verzeichnissen):

20. „globales `require_approval_for` überlebt eine Profil-`config.toml`":
    Home-`config.toml` setzt `[policy] require_approval_for = ["shell.exec"]`;
    Profil-`config.toml` setzt `[mcp_listener]` (o. ä.), aber **kein**
    `[policy]` → `resolved.harness.policy.require_approval_for` enthält
    weiterhin `"shell.exec"` (der exakte, vom Nutzer beschriebene Fall aus
    Abschnitt 0/4).

**Schutzwirkung nicht vertrauter Projekt-Layer bleibt erhalten:**

21. Alle neun heute schon per `merge_restricted_harness` abgedeckten Fälle
    (`policy.require_approval_for` Union, 4× `research.*`, 4×
    `tools.plan.*` — Abschnitt 3/6.3) werden 1:1 als Tests gegen
    `discover_config_with_restricted(..., Some(&restricted_repo))`
    reproduziert und müssen weiterhin bestehen (Nicht-Regression bei der
    Umstellung von `merge_restricted_harness` auf `merge_layer_into`).
22. Ein Feld, das laut Abschnitt 7c **neu** für den Projekt-Layer geschützt
    wird (z. B. `permissions.deny`, `guards.repeated_failure_warn`): Ein
    Projekt-`.harw/config.toml` mit einem lockernden Versuch wird
    verworfen — Beweis der bewussten Schutz-**Erweiterung**.
23. `[mcp_listener]`/`sandbox.*` bleiben für den Projekt-Layer weiterhin
    komplett unerreichbar (identisch zu heute, `discovery.rs:542-558`).

**Neu durch die R1/R2-Entscheidung vom 2026-09-21 nötig gewordene Tests:**

24. `StricterOf`-Fallback für `policy.default_visibility_scope` (R1): Home
    setzt `default_visibility_scope = "self"`, Profil setzt einen
    dritten/unbekannten Wert (z. B. `"team"`) → effektiv bleibt `"self"`
    (Home-Wert); der Profil-Wert wird **nicht** in die Ordnung einsortiert
    (weder als strenger noch als lockerer behandelt als `"self"`/
    `"everyone"`), sondern wie eine `GlobalOnly`-Abweichung verworfen —
    `ScopeDiagnostic` mit `field == "policy.default_visibility_scope"` und
    `rejected_value == "team"` wird erzeugt. Der Test muss fehlschlagen,
    falls eine künftige Implementierung den unbekannten Wert stattdessen
    stillschweigend als „lockerer" oder „strenger" einsortiert (z. B. durch
    einen String-Vergleich anstelle eines expliziten `ordering`-Lookups).
25. `mcp_listener.principals` — Entfernen (R2): Home setzt
    `principals = [P1, P2]` (verschiedene `id`), Profil setzt
    `principals = [P1]` → effektiv `[P1]`; `P2` fehlt im Ergebnis, **keine**
    `ScopeDiagnostic` (reines Entfernen ist erlaubt und kein
    Lockerungsversuch).
26. `mcp_listener.principals` — Hinzufügen verworfen (R2): Home setzt
    `principals = [P1]`, Profil setzt `principals = [P1, P3]` (`P3` mit
    einer `id`, die in Home nicht vorkommt) → effektiv `[P1]`; `P3` wird
    verworfen und löst eine `ScopeDiagnostic` (`field ==
    "mcp_listener.principals"`, `rejected_value` nennt die verworfene `id`)
    aus.
27. `mcp_listener.principals` — Rechteausweitung verworfen (R2): Home setzt
    `P1` mit `job_capabilities = ["ReadOwn"]`, Profil setzt einen Eintrag
    mit derselben `id` `P1`, aber erweiterten `job_capabilities =
    ["ReadOwn", "CancelWorkspace"]` (bzw. abweichendem `credential_ref`/
    `tenant`/`workspace`) → effektiv gewinnt die vollständige Home-Fassung
    von `P1` unverändert (kein Feld-Merge innerhalb des Principal-Eintrags);
    eine `ScopeDiagnostic` wird erzeugt, da der vom Profil gesetzte Wert für
    `P1` vom effektiven Ergebnis abweicht.

---

## 8. Risiken und offene Punkte

**R1 — `policy.default_visibility_scope`: unvollständige Ordnung — entschieden
2026-09-21.** Die in Abschnitt 6.2 festgelegte Ordnung `["self", "everyone"]`
deckt nur die beiden im Code belegten Werte ab. `PolicySection.default_visibility_scope`
hat kein `validate()`, das die Wertemenge einschränkt (anders als
`permissions.default_mode`/`ALLOWED_MODES`) — ein Layer könnte
theoretisch jeden beliebigen String setzen. **Entscheidung:** Die feste
Reihenfolge `"self" < "everyone"` wird festgeschrieben und bleibt
`MergeRule::StricterOf` mit genau dieser Ordnung (Abschnitt 6.2/6.3) — das
entspricht Weg (b) der ursprünglichen Analyse: Für jeden Wert außerhalb
`{"self", "everyone"}` fällt `stricter_of` auf `GlobalOnly`-Verhalten
zurück (kein Vergleich wird versucht; der abweichende Profil-/Projekt-Wert
wird ignoriert und löst eine `ScopeDiagnostic` aus), statt ihn
stillschweigend in die Ordnung einzusortieren. Zusätzlich zur reinen
Verhaltensfestlegung verlangt die Nutzerentscheidung einen expliziten
**Absicherungstest** (Abschnitt 7h, Test #24), der genau diesen Fallback
beweist und rot werden muss, falls eine künftige Implementierung einen
dritten Wert stattdessen fälschlich als „lockerer" oder „strenger"
behandelt. Weg (a) — `default_visibility_scope` vor dieser Arbeit per
`validate()` auf eine feste Wertemenge einzuschränken — wurde nicht
gewählt; die Absicherung erfolgt ausschließlich über den Merge-Fallback
plus Test.

**R2 — `mcp_listener.principals`: Entfernen-Vorschlag bestätigt — entschieden
2026-09-21.** Die ursprüngliche Analyse delegierte die Frage „darf ein
Profil Principals per Schnittmenge entfernen" mit einem markierten
Vorschlag an diese Spezifikation. **Entscheidung:** Ja — die Regel wechselt
von `GlobalOnly` auf `Intersection` nach dem Vergleichsschlüssel `id`
(Abschnitt 6.1/6.3, Zeile zu `mcp_listener.principals`). Ein Profil darf
`principals` auf eine Teilmenge der von Home gesetzten IDs einschränken
(reines Entfernen), aber **nie** eine neue `id` hinzufügen und **nie**
`credential_ref`/`tenant`/`workspace`/`job_capabilities` zu einer
bestehenden `id` verändern (z. B. Rechte erweitern) — nur Principals, deren
`id` exakt in der globalen Liste vorkommt, dürfen im Profil-Ergebnis
auftauchen, und für sie gewinnt immer die vollständige globale Fassung
(kein Feld-Merge innerhalb eines Principal-Eintrags). Damit ist Abschnitt
6.3/7 vollständig auf `Intersection` umgestellt (nicht mehr `GlobalOnly`);
die fünf Principal-Unterfelder sind entsprechend `CompositeMember`
(Kontrollsummen-Update in Abschnitt 6.3). Absicherungstests: Abschnitt 7h,
Tests #25 (Entfernen), #26 (Hinzufügen verworfen), #27 (Rechteausweitung
verworfen).

**R3 — `[network]`/`[browser]`/`[dod]`/`[web]` bleiben außen vor.** Diese
vier Sektionen liegen außerhalb von `HarnessConfig` (Abschnitt 1, Randnotiz)
und damit außerhalb des mit „89 `HarnessConfig`-Felder" abgesteckten
Umfangs dieser Aufgabe. Ihr heutiges Merge-Verhalten (sticky
Ganze-Sektion-Ersetzung bei Anwesenheit, `discovery.rs:607-624`) bleibt
unangetastet; `merge_restricted_network/_browser/_dod` bleiben als
separate, nicht generalisierte Funktionen bestehen. Eine Vereinheitlichung
unter derselben `MergeRule`-Engine wäre folgerichtig, ist aber nicht
Bestandteil der Arbeitspakete in Abschnitt 7g und müsste gesondert
beauftragt werden.

**R4 — „Vertraute Projekt-Ebene" ist ein Begriff aus dem Auftrag, nicht aus
dem Code.** Abschnitt 7d erklärt, dass der heutige Code nur „vertraute
Layer" (Home + Profil, ununterschieden) und den einen nicht vertrauten
Projekt-Layer kennt. Falls mit „vertraute Projekt-Ebene" etwas anderes
gemeint war (z. B. eine dritte, vom Nutzer aber nicht vom Repo verwaltete
Konfigurationsdatei, die heute noch gar nicht existiert), konnte ich das
im Code nicht verifizieren und rate hier bewusst nicht weiter — Abschnitt
7d beschreibt nur, wie sich eine solche Ebene *falls sie eingeführt wird*
in dieses Modell einfügen würde.

**R5 — `session.retention_days`/`compaction.absolute_ceiling_tokens` als
`MinBound`: Datenschutz- vs. Audit-Zielkonflikt bleibt bestehen.** Die
Nutzerentscheidung wählt „GLOBAL als Obergrenze (Minimum)" für beide Felder
explizit aus den zwei ursprünglich zur Wahl gestellten Optionen — das
Spannungsfeld selbst (ein Admin könnte eine *Mindest*-Aufbewahrung aus
Audit-Gründen wollen, was mit `MinBound` nicht ausdrückbar ist) ist damit
zwar entschieden, aber nicht aufgelöst; sollte ein Audit-Mindesthaltezeitraum
künftig gebraucht werden, braucht es ein zusätzliches, hier nicht
spezifiziertes Feld (z. B. `session.min_retention_days` mit `MaxBound`),
kein Umwidmen von `retention_days` selbst.

---

## Antwort-Zusammenfassung

> **Hinweis (2026-09-21):** Dieser Abschnitt ist die Zusammenfassung der
> **ursprünglichen Analyse** (Bestandsaufnahme + 11 OFFEN-Punkte, vor der
> Nutzerentscheidung). Er wird bewusst nicht überschrieben, um den
> Analysestand nachvollziehbar zu halten. Der **aktuelle, entschiedene**
> Stand steht in Abschnitt 2 (Tabelle, alle 11 Zeilen aufgelöst), Abschnitt
> 6 (vollständige `MergeRule`-Taxonomie für alle 88 Felder) und Abschnitt 7
> (verbindliche Umsetzungsspezifikation).

- **Dokument:** `/home/mia/Harwness-neu/docs/design/config-scopes.md`
- **Felder gesamt (dokumentiert):** 88 `HarnessConfig`-Blattfelder (Abschnitt
  1; `base_dir` als 89. Zeile separat vermerkt, kein TOML-Feld)
- **Entscheidungs-Zeilen in Abschnitt 2:** 58 (manche Zeilen fassen mehrere
  strukturgleiche Unterfelder mit identischer Scope-Begründung zu einer
  Zeile zusammen, z. B. `internal_models.*` für 9 Modellstellen-Felder,
  `reasoning.*` für 6 Effort-Felder, `mcp_listener.principals` für dessen 5
  Unterfelder, `onboarding.seen.*` für 3 Flags, `sandbox.cargo.*`/
  `sandbox.tmux.*` für je 4/2 Unterfelder) — zusammen decken sie alle 88
  Blattfelder ab.
- **GLOBAL vorgeschlagen:** 22 Zeilen — `policy.require_approval_for`,
  `mcp_listener.enabled`/`listen_addr`/`path`,
  `tools.plan.validate_dependency_cycles`/`validate_write_conflicts`/
  `max_nodes`/`max_expand_depth`, `research.network_allow_hosts`/
  `cargo_registry_read`/`max_fetch_bytes`/`fetch_timeout_secs`,
  `permissions.approval_timeout_secs`/`deny`, `sandbox.cargo.*`,
  `sandbox.tmux.*` (je exklusiv), `guards.enabled`/`repeated_failure_warn`/
  `repeated_failure_abort`/`no_progress_rounds_warn`/
  `no_progress_rounds_abort`/`plan_stale_rounds`.
- **PROFIL vorgeschlagen:** 25 Zeilen — `default_provider`/`default_model`/
  `active_agent_definition`/`active_uia_definition`/`uia_provider`/
  `uia_model`, `logging.*` (3), `tui.*` (2), `session.*` außer
  `retention_days` (4), `onboarding.seen.*`, `tools.plan.enabled`/`persist`/
  `require_for_complex_work`/`require_exploration_for`/
  `exploration_ttl_secs`, `mode.default`, `research.cache_ttl_secs`,
  `internal_models.*`, `reasoning.*`.
- **OFFEN (Nutzer entscheidet):** 11 Zeilen — `config_version`,
  `workspace_root`, `policy_profile`, `project_root_markers`,
  `session.retention_days`, `policy.default_visibility_scope`,
  `mcp_listener.principals` (inkl. `id`/`credential_ref`/`tenant`/
  `workspace`/`job_capabilities`), `permissions.default_mode`,
  `permissions.allow` (sicherheitskritisch), `permissions.extra_roots`
  (sicherheitskritisch), `compaction.absolute_ceiling_tokens` — siehe
  Abschnitt 2 für die je zwei bis drei zur Wahl gestellten Optionen pro
  Feld.

**Die 3 wichtigsten Funde:**

1. **Die eigentliche Ursache ist strukturell, nicht feldspezifisch:**
   `resolved.harness = cfg` (`discovery.rs:649`) ersetzt bei jedem
   vertrauten Layer die komplette `HarnessConfig`. Nur 5 von 26
   Top-Level-Feldern (`default_provider`, `default_model`,
   `active_uia_definition`, `onboarding`, `internal_models`) werden
   explizit gegen dieses Verhalten abgesichert — **alle anderen ~80 Felder**
   (inkl. `policy.require_approval_for`, dem vom Nutzer genannten Beispiel,
   aber auch `permissions.*`, `sandbox.*`, `mcp_listener.*`, `guards.*`)
   fallen still auf ihren Compile-Zeit-Default zurück, sobald ein späterer
   Layer sie nicht wiederholt. Da `ensure_home` die Profil-`config.toml`
   immer erzeugt, tritt der Fall bei jeder Installation auf, sobald die
   Home-Ebene irgendein betroffenes Feld vom Default abweichend setzt.
2. **Es gibt bereits eine funktionierende, aber sehr eng begrenzte
   Verengungs-Logik** (`apply_restricted_layer` +
   `merge_restricted_harness`/`_network`/`_browser`/`_dod`,
   `discovery.rs:752-1003`) für einen nicht vertrauten Projekt-`.harw`: sie
   implementiert exakt die vom Nutzer gewünschte „Profil darf nur
   verschärfen“-Semantik (Vereinigung/Schnittmenge/Minimum/AND/OR je nach
   Feldsemantik) — aber nur für 9 von 89 `HarnessConfig`-Feldern
   (`policy.require_approval_for`, 4× `research.*`, 4× `tools.plan.*`).
   Sicherheitskritische Bereiche wie `permissions.allow`/`extra_roots`,
   `sandbox.*` und `mcp_listener.*` sind **überhaupt nicht** erfasst — auch
   nicht gegen den nicht vertrauten Projekt-Layer, geschweige denn im
   GLOBAL/PROFIL-Modell. Dieses Vorbild lässt sich direkt auf die neue
   GLOBAL/PROFIL-Trennung übertragen (Abschnitt 5).
3. **Zwei Feldgruppen sind mit einer naiven „Union verengt“-Regel
   gefährlich, nicht harmlos:** `permissions.allow` und
   `permissions.extra_roots` **erweitern** Rechte (sie umgehen
   Freigabe-Abfragen bzw. öffnen zusätzliche Arbeitswurzeln) — anders als
   `require_approval_for` oder `deny`, wo mehr Einträge automatisch
   restriktiver sind. Eine Profil-Ebene, die diese Listen per Vereinigung
   „erweitern“ darf, würde damit genau das Gegenteil der vom Nutzer
   gewünschten Garantie erreichen (Admin beschränkt Rechte global, Profil
   kann sie nicht wieder öffnen). Für diese beiden Felder braucht es vor der
   Umsetzung eine explizite Entscheidung (Abschnitt 2, `OFFEN
   (sicherheitskritisch)`), keine automatische Ableitung aus dem
   `require_approval_for`-Muster.
