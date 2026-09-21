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
| `policy_profile` | `Option<String>` | `None` | `harness_config.rs:40-41` | ERSETZT |
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

**Gesamtzahl dokumentierter `HarnessConfig`-Felder: 88** (Blattfelder inkl.
verschachtelter Typen wie `McpPrincipalToml`, `RuleToml`,
`CargoSandboxToml`/`TmuxSandboxToml`, `InternalModelChoice`,
`OnboardingSeen`; dazu `base_dir` als 89. Tabellenzeile in Abschnitt 1.1,
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
`active_agent_definition`, `active_uia_definition`, `policy_profile` (bereits
in 1.1 gelistet).

---

## 2. Scope-Vorschlag pro Feld

Legende: 🔒 = vom Nutzer als sicherheits-/beschränkungsrelevant markiert
(Freigaben, Sandbox, Netzwerk, Secrets, Listener, Berechtigungen).

| TOML-Pfad | Vorschlag | Merge-Regel | Begründung | 🔒 |
|---|---|---|---|---|
| `config_version` | **OFFEN** | — | Kein Scope im eigentlichen Sinn: vermutlich pro *Datei* zur Migrationssteuerung gedacht, nicht pro Ebene mergebar. Optionen: (A) GLOBAL, ein kanonischer Schema-Stand; (B) pro Layer eigenständig, gar nicht Teil dieses Scope-Modells. | |
| `workspace_root` | **OFFEN** | — | Überschreibt die Arbeitswurzel direkt. Optionen: (A) GLOBAL, damit ein Admin die Wurzel diktiert (Pfad-Traversal-Risiko, wenn ein Profil sie frei setzen darf); (B) PROFIL, da jedes Profil einen eigenen Arbeitskontext braucht. | 🔒 |
| `default_provider` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | Aktive Modellwahl ist Profil-Alltag, keine Beschränkung. | |
| `default_model` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | s.o. | |
| `active_agent_definition` | PROFIL | letzter gesetzter Wert gewinnt | Auswahl einer Agenten-Definition ist Nutzerpräferenz. | |
| `active_uia_definition` | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | s.o. | |
| `uia_provider` | PROFIL | letzter gesetzter Wert gewinnt | Pinning ist Profil-spezifisch. | |
| `uia_model` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `policy_profile` | **OFFEN** | — | Bisher kein Consumer im Code gefunden (nur deklariert + per CLI setzbar, `harw-cli/src/settings.rs`); Name legt eine sicherheitsrelevante Policy-Auswahl nahe. Optionen: (A) GLOBAL, Admin erzwingt eine Policy-Familie; (B) PROFIL, jedes Profil wählt seine eigene. | 🔒 |
| `project_root_markers` | **OFFEN** | — | Reine Heuristik zur Root-Erkennung, aber beeinflusst indirekt, was als „Projekt“ (und damit als Sandbox-Grenze) gilt. Optionen: (A) PROFIL, reine Bequemlichkeit; (B) GLOBAL mit „nur verengen“, falls Root-Erkennung Sicherheitsgrenzen berührt. | |
| `logging.level` | PROFIL | letzter gesetzter Wert gewinnt | Ausgabe-Verbosität, keine Beschränkung. | |
| `logging.target_module_paths` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `logging.json` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `tui.theme` | PROFIL | letzter gesetzter Wert gewinnt | Reine UI-Präferenz. | |
| `tui.keybindings_file` | PROFIL | letzter gesetzter Wert gewinnt | s.o. | |
| `session.store_dir` | PROFIL | letzter gesetzter Wert gewinnt | Ablagepfad ist Profil-lokal. | |
| `session.journal_format` | PROFIL | letzter gesetzter Wert gewinnt | Reines Format. | |
| `session.retention_days` | **OFFEN** | — | Kann Compliance-/Audit-Anforderung sein (Admin will Mindest-Aufbewahrung) oder Datenschutz-Wunsch (Profil will kürzer löschen) — Richtung des „strengeren“ Werts ist nicht eindeutig. Optionen: (A) GLOBAL, Profil darf nur verkürzen (Datenschutz-Sicht); (B) GLOBAL, Profil darf nur verlängern (Audit-Sicht); (C) PROFIL, freie Wahl. | |
| `session.title_generation` | PROFIL | letzter gesetzter Wert gewinnt | Komfort-Feature. | |
| `session.title_model` | PROFIL | letzter gesetzter Wert gewinnt | Legacy-Modellwahl, wie `internal_models`. | |
| `policy.default_visibility_scope` | **OFFEN** | — | Sicherheitsrelevant (Standard-Sichtbarkeit neuer Sessions), aber als freier String ohne definierte Ordnung („self“ vs. andere Werte) nicht eindeutig „strenger/lockerer“ vergleichbar. Optionen: (A) GLOBAL gewinnt immer; (B) GLOBAL mit einer noch zu definierenden Rangfolge, Profil darf nur restriktiver wählen. | 🔒 |
| `policy.require_approval_for` | GLOBAL (Baseline) + Profil erweitert | **Vereinigung** | Direkt vom Nutzer vorgegebenes Beispiel; bereits identisch für den untrusted Projekt-Layer implementiert (`merge_restricted_harness:958-964`) — dasselbe Muster auf GLOBAL/PROFIL übertragen. | 🔒 |
| `mcp_listener.enabled` | GLOBAL (Baseline) + Profil darf nur verengen | **AND** (analog `browser.enabled`, `merge_restricted_browser:912-914`) | Öffnet eine lokale Netzwerk-Angriffsfläche; ein Profil darf einen global deaktivierten Listener nicht aktivieren. | 🔒 |
| `mcp_listener.listen_addr` | GLOBAL | global gewinnt | Bind-Adresse ist Angriffsfläche; Profil soll sie nicht verschieben können (Analogie zu `web.bind`, das nie vom Repo beeinflussbar ist). | 🔒 |
| `mcp_listener.path` | GLOBAL | global gewinnt | s.o. | 🔒 |
| `mcp_listener.principals` (inkl. `id`/`credential_ref`/`tenant`/`workspace`/`job_capabilities`) | **OFFEN** | — | Enthält Credential-Referenzen und Autorisierungs-Capabilities. Optionen: (A) GLOBAL, Admin verwaltet alle zugelassenen Identitäten zentral; (B) PROFIL, jedes Profil bringt eigene Principals für seine eigene Listener-Instanz mit — dann aber nur wirksam, wenn `mcp_listener.enabled` global erlaubt ist. | 🔒 |
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
| `permissions.default_mode` | **OFFEN** | — | Zentraler Freigabe-Schalter (`ask`/`auto`/`full`). Eine Rangfolge „strenger“ existiert konzeptionell (`ask` > `auto` > `full`), ist im Code aber nirgends kodiert; es gibt (anders als bei `require_approval_for`) kein Vorbild. Optionen: (A) GLOBAL gewinnt immer; (B) GLOBAL mit Ordinalskala, Profil darf nur strenger wählen. | 🔒 |
| `permissions.approval_timeout_secs` | GLOBAL (Baseline) + Profil darf nur verengen | **Minimum** | Kürzerer Timeout = konservativer (Auto-Ablehnung greift früher); analog zu `research.*_secs`. | 🔒 |
| `permissions.allow` | **OFFEN (sicherheitskritisch)** | — | `allow`-Regeln umgehen die Freigabe-Abfrage — das Gegenteil von `require_approval_for`. Eine Vereinigung wäre hier eine **Lockerung**, kein Verengen. Optionen: (A) GLOBAL exklusiv, Profil darf `allow` gar nicht setzen; (B) Profil darf nur eine **Teilmenge** der global erlaubten Regeln referenzieren (Schnittmenge statt Vereinigung). Kein bestehendes Vorbild in `discovery.rs` deckt diesen Fall ab. | 🔒 |
| `permissions.deny` | GLOBAL (Baseline) + Profil erweitert | **Vereinigung** | Gegenstück zu `allow`: mehr `deny`-Regeln bedeuten nur mehr Ablehnungen, also sicher zu vereinigen — gleiches Muster wie `require_approval_for`. | 🔒 |
| `permissions.extra_roots` | **OFFEN (sicherheitskritisch)** | — | Erweitert erlaubte Arbeitswurzeln — analog zu `allow` eine potenzielle Rechteausweitung, keine Verengung. Optionen: (A) GLOBAL exklusiv; (B) Profil darf nur eine Teilmenge der global gesetzten Wurzeln referenzieren. | 🔒 |
| `sandbox.cargo.*` | GLOBAL exklusiv | Profil darf nicht setzen/überschreiben | Vertrauensanker für die Cargo-Sandbox; Moduldoku (`harness_config.rs:70-74`) verlangt ausdrücklich, dass diese Werte nur beim Runtime-Aufbau aus der (vertrauten) Konfiguration gelesen werden. Direkte Analogie zu `browser.geckodriver_path`/`geckodriver_sha256` und `dod.proof_key_dir`, die bereits nie aus einem Repo-Layer übernommen werden (`browser_toml.rs:18-25`, `dod_toml.rs:22-31`). | 🔒 |
| `sandbox.tmux.*` | GLOBAL exklusiv | Profil darf nicht setzen/überschreiben | s.o. | 🔒 |
| `internal_models.*` (alle 9 Felder) | PROFIL | letzter gesetzter Wert gewinnt (bereits korrekt implementiert) | Modell-Routing für Hilfsaufgaben ist Nutzerpräferenz/Kostensteuerung, keine Zugriffsbeschränkung. | |
| `compaction.absolute_ceiling_tokens` | **OFFEN** | — | Reine Ressourcen-/Kostengrenze. Optionen: (A) PROFIL, freie Wahl; (B) GLOBAL mit „Profil darf nur senken“ (Minimum), falls Admin eine Kostenobergrenze erzwingen will. | |
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

## Antwort-Zusammenfassung

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
