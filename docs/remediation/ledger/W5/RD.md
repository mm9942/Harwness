# W5 / RD — Rollenrechte `harw-registry-defaults` (G-055, F-084, F-073, Annahme A5)

- Welle: W5 (vorgezogen), Rolle: Coding-Agent (Opus), hohe Sicherheitsstufe.
- Owned: `harw-registry-defaults/**` (src, agents/*.toml, tests; `Cargo.toml` nur path-Zeilen), dieses Ledger.
- BUILD-POLICY eingehalten: kein cargo build/check/test/clippy/run/add/fmt, kein rustc/rust-analyzer, keine
  git-Schreibbefehle. Ausgeführt: `cargo metadata --offline --no-deps --format-version 1` (vor/nach: Exit 0).
  **Nichts kompiliert, kein Test ausgeführt** — Signaturen, Imports, Borrows von Hand gegen die gelesenen APIs geprüft.
  Zeilen ≤ 100 Zeichen in allen neuen/geänderten Stellen (awk-Prüfung); `cargo fmt --check` durch Orchestrator.
- Pflichtlektüre: AGENT-BRIEF.md; Plan W5-Tabelle RD, A5, K3 (A-BRIDGE); Register G-055 (w4), F-084/F-073 (w1-w3);
  `reports/w4-registry-defaults.md` (R6, P2-Maßnahmen); Ledger W1-05 und W2A-04 (Allowlist/Zelle/kein `Default` —
  **nicht zurückgedreht**), W3/C-CFG (`NetworkSection`), W3/C-EGRESS (`EgressPolicy`), W2d2/R0 (ReadOnlyExplore ⊆? Full).

## 1. Geänderte / neue Dateien

| Datei | Änderung |
|---|---|
| `Cargo.toml` | + `harw-config`, `harw-egress`, `harw-sandbox` (nur `path`-Zeilen; Zyklusprüfung per `cargo metadata`: keine der drei hängt an `harw-registry-defaults`; Abnehmer sind nur harw-ops/-tui/-runtime/-cli/harw) |
| `src/authority.rs` (neu) | Werkzeug→Recht, `AuthorityReducer`, `reduce_to_read_{only,registry,network}`, `authority_reducer_for_role` + 9 Unit-Tests |
| `src/research_web.rs` (neu) | `researcher_web_policy`, `researcher_web_network_scope` + 5 Unit-Tests |
| `src/error.rs` | + Variante `ResearcherWebPolicy { source: EgressError }` (Display, `source()`) |
| `src/profile.rs` | `Research` = nur `web.*`; `Full` ohne Browser; `required_permissions`, `tool_names_for`; `assemble_registry_for_sandbox`; `browser_tool_provider` (Feature); Tests |
| `src/lib.rs` | `pub mod authority; pub mod research_web;`, Re-Exporte, Doku, Test ohne Browser-Erwartung |
| `agents/researcher-web.toml` | `admitted` = nur `web.*`; `forbidden` + alle `fs.*`, `deps.*`, `lens.ask`, `browser.*` |
| `tests/role_rights_matrix.rs` (neu) | Rechte-Matrix Rollen × Profile × 2⁷ Rechtesätze + TOML-Seite + echte Montage |

## 2. Öffentliche API (neu/geändert, exakt)

```rust
// harw_registry_defaults::authority   (Re-Export an der Wurzel: AuthorityReducer, authority_reducer_for_role, tool_permission)
pub const REDUCE_TO_READ_ONLY: &str = "reduce_to_read_only";
pub const REDUCE_TO_READ_REGISTRY: &str = "reduce_to_read_registry";
pub const REDUCE_TO_READ_NETWORK: &str = "reduce_to_read_network";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityReducer { ReadOnly, ReadRegistry, ReadNetwork }
impl AuthorityReducer {
    pub const ALL: &'static [AuthorityReducer];
    pub const fn id(self) -> &'static str;
    pub fn ceiling(self) -> PermissionSet;              // {RW} | {RW, ReadCargoRegistry} | {NetworkAccess}
    pub fn reduce(self, granted: &PermissionSet) -> PermissionSet;   // granted ∩ ceiling
}
pub fn reduce_to_read_only(granted: &PermissionSet) -> PermissionSet;
pub fn reduce_to_read_registry(granted: &PermissionSet) -> PermissionSet;
pub fn reduce_to_read_network(granted: &PermissionSet) -> PermissionSet;
pub fn authority_reducer_for_role(role: &str) -> Option<AuthorityReducer>;
pub fn tool_permission(tool: &str) -> Option<harw_sandbox::Permission>;

// harw_registry_defaults::research_web   (Re-Export an der Wurzel)
pub fn researcher_web_policy(network: &harw_config::NetworkSection)
    -> RegistryDefaultsResult<Arc<harw_egress::EgressPolicy>>;
pub fn researcher_web_network_scope(policy: &harw_egress::EgressPolicy) -> harw_sandbox::NetworkScope;

// harw_registry_defaults::profile   (assemble_registry_for_sandbox auch an der Wurzel)
impl RegistryProfile {
    pub fn required_permissions(self) -> PermissionSet;
    pub fn tool_names_for(self, granted: &PermissionSet) -> Vec<&'static str>;
}
pub fn assemble_registry_for_sandbox(profile: RegistryProfile, project: &ProjectContext,
    overrides: IdentityOverrides, approval_mode: ApprovalModeCell, granted: &PermissionSet)
    -> RegistryDefaultsResult<AssembledRegistry>;
#[cfg(feature = "browser")]
pub fn browser_tool_provider(grant: harw_tool_browser::BrowserOpenGrant)
    -> RegistryDefaultsResult<Arc<dyn ToolProvider>>;

// harw_registry_defaults::RegistryDefaultsError  (nicht non_exhaustive)
ResearcherWebPolicy { source: harw_egress::EgressError }
```

Unverändert in der Signatur: `assemble_registry`, `assemble_registry_for_project` (delegiert jetzt an
`assemble_registry_for_sandbox(.., &profile.required_permissions())` = volles Profil, verhaltensgleich außer den
Inhaltsänderungen §3), `assemble_default_registry`, `AUTO_APPROVED_TOOLS`, `DefaultApprovalPolicy`, `profile_for_role`.

## 3. Verhaltensänderungen

1. **`researcher-web` / Profil `Research` = nur `web.fetch`, `web.docs_rs`, `web.crates_io`.** Vorher zusätzlich
   5×`fs.*` + 5×`deps.*`. Registriert, beworben (Identity) und admittiert (TOML) sind deckungsgleich;
   `required_permissions() = {NetworkAccess}`.
   **Entscheidung über den A5-Wortlaut hinaus:** auch `deps.graph`/`deps.locked` sind entfernt. Beide lesen mit
   `ReadWorkspace` `Cargo.lock`/Workspace-Metadaten (`graph_tool.rs:195`, `locked_tool.rs:209`) — das sind
   Workspace-Daten, die über `web.fetch`-Query-Parameter hinausgetragen werden könnten (A5-Zweck). Crate/Version
   bekommt die Rolle über den Auftrag. Rücknahme wäre eine TOML-Zeile + `Research`-Arm in `profile.rs` +
   `ReadNetwork`-Obergrenze um `ReadWorkspace` — bitte bestätigen oder zurückweisen (Frage 1).
2. **Netz-Scope nur aus `[network].researcher_web_hosts`**: `EgressPolicy::new(hosts.to_vec(), false)`
   (`harw-egress/src/policy.rs:91`). `allow_hosts` fließt nicht ein; `allow_private` ist für die Rolle **immer**
   `false` (auch bei `[network].allow_private = true`). Leer ⇒ Policy ohne Ziel (C-EGRESS §7.1, fail-closed).
   Ungültiger Eintrag ⇒ `ResearcherWebPolicy`-Fehler statt verkürzter Liste. `NetworkScope` wird aus den
   **kanonisierten** Policy-Hosts gebaut (`NetworkScope::from_hosts`, `harw-sandbox/src/lib.rs:286`).
   Hinweis: Die Feld-Doku in `harw-config/src/network_toml.rs:57-60` nennt die Liste „zusätzliche Hostnamen“;
   umgesetzt ist laut Brief „nur aus researcher_web_hosts“ (Frage 2).
3. **Browser nur mit Grant (F-073).** `Full` registriert unter Feature `browser` **keine** `browser.*` mehr (vorher
   `BrowserToolSet::new(host)` ohne Grant). Einziger Weg: `browser_tool_provider(grant)` →
   `BrowserToolSet::with_open_policy(host, BrowserOpenPolicy::grant(grant))` (`harw-tool-browser/src/tool_set.rs:33`,
   `types.rs:139`, Re-Export `lib.rs:17`). **Rollen-Grant-Feld verifiziert: existiert nicht.** Die DSL kennt nur
   `[tools].admitted/forbidden` (`harw-agent-dsl/src/executable.rs:1093,1098`) und `AuthorityCeiling.capabilities`
   (`authority.rs:43-45`); `grep -i browser harw-agent-dsl/src` = 0 Treffer. Der Grant ist deshalb host-seitig
   (`[browser]`-Sektion, `BrowserSection{enabled, allowed_origins, max_actions, …}`), keine Rolle admittiert
   `browser.*` (Matrix-Test), `researcher-web` verbietet sie ausdrücklich.
   `profile_tool_providers` ist dadurch infallibel; `RegistryDefaultsError::BrowserHost` entsteht nur noch in
   `browser_tool_provider`.
4. **Registry-seitiger Reducer / R0-Frage „ReadOnlyExplore ⊆ Full (deps.*)“ — gehärtet.**
   `tool_names_for(granted)` lässt nur Werkzeuge mit gewährtem Recht (unbekanntes Recht ⇒ nie);
   `assemble_registry_for_sandbox` filtert Provider (`RestrictedToolProvider`, Executor per Namensraten
   unerreichbar) **und** Identity; leere Provider werden nicht registriert. Ergebnis: `ReadOnlyExplore` unter
   `{ReadWorkspace}` registriert `fs.read…grep, deps.graph, deps.locked`, **kein** `deps.source_*`.
   R0-Aussage „deps.* bleiben fail-closed wirkungslos“ bestätigt; jetzt zusätzlich unsichtbar — **sobald die
   Composition-Roots auf `assemble_registry_for_sandbox` umgestellt sind** (Folgearbeit §6).
5. **Reducer je Rolle** (`authority_reducer_for_role`):

   | Rolle | Profil | Reducer | Obergrenze |
   |---|---|---|---|
   | explorer, analyst, researcher-deps | ReadOnlyExplore | `reduce_to_read_registry` | {ReadWorkspace, ReadCargoRegistry} |
   | planner | Planning | `reduce_to_read_registry` | dito (Frage 3) |
   | researcher-web | Research | `reduce_to_read_network` | {NetworkAccess} |
   | security-{egress,baseline,structure,endpoint}-triage | NoTools | `reduce_to_read_only` | {ReadWorkspace} |

   Invariante (Test): `profile.required_permissions() ⊆ reducer.ceiling()` für jede Rolle.
   `reduce_to_read_network` ist hier **ohne** `ReadWorkspace` — falls A-BRIDGE (K3) die Sandbox-Variante mit
   `ReadWorkspace` baut, ist das weiter als nötig (Frage 4).

## 4. API-Belege (Datei:Zeile, gelesen)

| Item | Beleg |
|---|---|
| `Permission` (7 Varianten, `Copy+Ord+Debug`), `PermissionSet::{empty,from_policy,contains,intersection,is_subset_of,iter}` | `harw-sandbox/src/lib.rs:55-79, 86-128` |
| `NetworkScope::{from_hosts,hosts,is_empty,allows}` | `harw-sandbox/src/lib.rs:286,304,314,422` |
| `harw_tools::Permission` = `harw_sandbox::Permission` | `harw-tools/src/lib.rs:50` |
| `TOOL_NAMES`/`TOOL_PERMISSIONS` aus `tool_provider!` | `harw-tools/src/provider_macro.rs:110-123`; Deps `harw-tool-deps/src/provider.rs:38,118-121`; Lens `harw-tool-lens/src/provider.rs:48-53` |
| Rechte je Werkzeug | fs: `read.rs:97`, `list.rs:107`, `search.rs:191`, `glob.rs:114`, `grep.rs:256`, `write.rs:115`; shell `exec.rs:574`; deps `graph_tool.rs:195`, `locked_tool.rs:209`, `source_tool.rs:803,855,918`; lens `ask_tool.rs:242`; web `fetch.rs:1467`, `docs_rs.rs:302`, `crates_io.rs:261`; browser `harness_provider.rs:239` |
| `EgressPolicy::{new,allow_hosts,allow_private,check_url}`, `EgressError: Display + Error` | `harw-egress/src/policy.rs:91,103,109,152`; `error.rs:75,114`; Re-Export `lib.rs` |
| `NetworkSection{allow_hosts, allow_private, researcher_web_hosts}` (`Default`, alle `pub`), Re-Export | `harw-config/src/network_toml.rs:43-62`; `harw-config/src/lib.rs` (`pub use network_toml::NetworkSection`) |
| `BrowserToolSet::{new,with_open_policy}`, `BrowserOpenPolicy::grant`, `BrowserOpenGrant` | `harw-tool-browser/src/tool_set.rs:26,33`; `types.rs:64,139`; `lib.rs:17` |
| `FirefoxHost::new(FirefoxHostConfig) -> Result<_, AdapterError>`, `HarwnessBrowserToolProvider::new` | `harw-browser-thirtyfour/src/host.rs:71`; `harw-tool-browser/src/harness_provider.rs:44` |
| `ResolvedToolSurface::{admitted,forbidden}` | `harw-agent-dsl/src/executable.rs:1093,1098` |
| `WebToolProvider::new()` (Unit, `tool_provider!`) — **keine** Policy-Annahme (N-WEB offen) | `harw-tool-web/src/provider.rs:65-76` |

## 5. Tests (neu/geändert; nicht ausgeführt)

- `authority.rs`: `test_tool_permission_matches_deps_provider_declarations`, `…_lens_provider_declarations`
  (Tabelle gegen `TOOL_PERMISSIONS`, andere Quelle), `test_tool_permission_covers_every_tool_of_every_profile`,
  `test_reduce_never_exceeds_parent_or_ceiling`, `test_reduce_to_read_registry_keeps_registry_only_if_parent_has_it`,
  `test_reduce_to_read_network_never_reads_the_workspace`, `test_reduce_to_read_only_is_read_workspace_at_most`,
  `test_authority_reducer_id_is_distinct_and_named`, `test_authority_reducer_for_role_covers_every_role_and_bounds_its_profile`.
- `research_web.rs`: leer ⇒ kein Netz; `allow_hosts` erweitert nicht; `allow_private` wird ignoriert (Loopback
  bleibt gesperrt); ungültiger Eintrag ⇒ `ResearcherWebPolicy` mit `source()`; Scope = kanonische Policy-Hosts.
- `profile.rs`: `test_research_profile_adds_exactly_the_web_tools` → **ersetzt** durch
  `test_research_profile_registers_only_the_web_tools`; neu `test_no_profile_registers_browser_tools_without_a_grant`,
  `test_deps_tool_lists_partition_the_provider_order`, `test_required_permissions_per_profile`,
  `test_tool_names_for_hides_registry_tools_without_read_cargo_registry`,
  `test_assemble_registry_for_sandbox_registers_and_advertises_only_granted_tools` (inkl. Executor-Namensraten,
  Research ohne `NetworkAccess` ⇒ 0 Provider).
- `lib.rs`: `assemble_default_registry_still_yields_the_full_coding_tool_set` erwartet auch unter Feature
  `browser` keine `browser.*` mehr (`cfg_attr(allow(unused_mut))` entfernt).
- `tests/role_rights_matrix.rs`: Rollentabelle; Profil × 128 Rechtesätze; Rolle × 128 Elternsätze nach Reducer;
  TOML-Seite (keine Rolle admittiert `fs.write`/`shell.exec`/`browser.*`; researcher-web nur `web.*`, verbietet
  `fs.*`/`deps.*`); echte Montage 5 × 128 = Matrix.
- Bestehende Tests geprüft, bleiben grün laut Lesen: `tool_admission_coverage.rs` (beide Richtungen + „unclaimed = ∅“),
  `embedded_agents.rs` (`…forbids_write_and_shell_tools`, `…only_role_with_web_tools`), `lib.rs`-Allowlist-Tests
  (`web.*` bleibt read-only-Oberfläche via `Research`), Golden-Kontextprogramme (rendern keine Werkzeugfläche).

## 6. Folgearbeit (außerhalb Ownership, per grep)

| Stelle | Nötig | Welle |
|---|---|---|
| `harw-runtime/src/children.rs:176` `assemble_registry_for_project(profile, …)` | → `assemble_registry_for_sandbox(.., &authority_reducer_for_role(role)?.reduce(&parent_permissions))`; ohne Umstellung bleiben `deps.source_*` registriert (nur Prolog-Fehler) | W6 I-CONTRIB |
| `harw-runtime/src/assembly.rs:951` (Root) | → `assemble_registry_for_sandbox(.., sandbox.permissions())` | W6 I-CONTRIB |
| Web-Tools mit Policy registrieren | `researcher_web_policy(&resolved.network)` → Konstruktor von N-WEB; Kind-Sandbox `with_network_scope(researcher_web_network_scope(&policy))`. **N-WEB-Signatur existiert noch nicht** (`WebToolProvider` ist Unit-Struktur) | W6 I-CONTRIB (nach N-WEB) |
| Browser registrieren | `[browser].enabled` ⇒ Grant aus `BrowserSection` bauen ⇒ `browser_tool_provider(grant)` ⇒ `tool_provider(..)` | W6 I-CONTRIB / I-CLI |
| `harw-ops/src/{explore.rs:488, research.rs:375, research.rs:436}` `authority = "reduce_to_read_only"` | → `reduce_to_read_registry` (explore, research_deps), `reduce_to_read_network` (research_web); analyze prüfen | A-BRIDGE/A-OPSPLAN bzw. W6 |
| `harw-core-bridge/src/agent_tool.rs:1592` `KNOWN_AUTHORITY_REDUCERS` | Kennungen `reduce_to_read_registry`/`reduce_to_read_network` aufnehmen (K3), Obergrenzen = `AuthorityReducer::ceiling` | A-BRIDGE |
| `harw-cli/src/lifecycle.rs:289-318` | akzeptiert `base` oder `base ∪ browser`; bleibt grün, `with_browser`-Zweig ist jetzt tot → vereinfachen | I-CLI |
| `harw-runtime/tests/rights_matrix.rs` | Rollen-Matrix um Reducer/`assemble_registry_for_sandbox` erweitern | I-CONTRIB |
| `agents/context-programs/research-web.toml` (owned, bewusst **nicht** geändert) | Sektion `task.read_scope` (must-include) begründet sich mit lokalem Lesen, das die Rolle nicht mehr hat; Änderung würde Golden ändern → Entscheidung Orchestrator | RD-Nachlauf |
| Exhaustive `match` auf `RegistryDefaultsError` außerhalb | grep `RegistryDefaultsError::` außerhalb des Crates: **0 Treffer** — kein Bruch | — |
| Konsumenten von `RegistryProfile::Research`-Inhalt | grep: nur Doku `harw-cli/src/job_worker.rs:1566` — kein Bruch | — |

dep-requests: keine (nur `path`-Zeilen).

## 7. Offene Fragen an den Orchestrator

1. `deps.graph`/`deps.locked` für `researcher-web` entfernt (strenger als A5-Wortlaut) — bestätigen?
2. `researcher_web_hosts` exklusiv (Brief) vs. „zusätzlich zu `allow_hosts`“ (Feld-Doku C-CFG) — Brief gilt, Doku anpassen?
3. `planner` bekommt `reduce_to_read_registry` (sonst verletzt Planning `deps.source_*` die Obergrenzen-Invariante); Brief nennt nur explorer/analyst/researcher-deps.
4. K3-Sandbox-Reducer `reduce_to_read_network` ohne `ReadWorkspace` angleichen.

## 8. Ergebnis

```json
{"agent":"W5-RD",
 "files_created":["/home/mia/projects/harwness/harw-registry-defaults/src/authority.rs",
  "/home/mia/projects/harwness/harw-registry-defaults/src/research_web.rs",
  "/home/mia/projects/harwness/harw-registry-defaults/tests/role_rights_matrix.rs",
  "/home/mia/projects/harwness/docs/remediation/ledger/W5/RD.md"],
 "files_modified":["/home/mia/projects/harwness/harw-registry-defaults/Cargo.toml",
  "/home/mia/projects/harwness/harw-registry-defaults/src/error.rs",
  "/home/mia/projects/harwness/harw-registry-defaults/src/profile.rs",
  "/home/mia/projects/harwness/harw-registry-defaults/src/lib.rs",
  "/home/mia/projects/harwness/harw-registry-defaults/agents/researcher-web.toml"],
 "verification":{"command":"cargo metadata --offline --no-deps --format-version 1 (Build/Test durch Orchestrator)","exit_code":0,"pass":null},
 "stubbed_imports":[{"module":"harw_tool_web (Policy-Konstruktor)","reason":"N-WEB parallel; Registrierung mit Policy = W6 I-CONTRIB"}],
 "blocked":false}
```
