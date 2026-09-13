# Review Z2b-R2 — statischer Compile-Review W2B-02

- Welle: W2b, Ledger `docs/remediation/ledger/W2b/W2B-02.md`
- Umfang: `harw-runtime/src/{sandbox.rs, ceiling.rs, budget.rs, trace.rs}`, `harw-runtime/Cargo.toml`
- Verfahren: READ-ONLY, kein Compilerlauf. Jeder Pfad, jede Arität, jeder
  Typ und jedes `match` wurde gegen die Quelle der jeweiligen Fremd-Crate
  geprüft (Zeilenangaben unten).
- Vertrag: `CONTRACTS.md` §runtime-spec (Reduktionstabelle), §principal.

## Kompiliert voraussichtlich

**Ja.** Es wurde **kein** blockierender Compile- oder Clippy-Fehler gefunden.
Die vom Orchestrator nachgetragene Zeile `harw-lens-types = { path =
"../harw-lens-types" }` ist genau und ausschließlich die fehlende
Voraussetzung; damit ist `ceiling.rs` konstruierbar. Ein Zyklus entsteht
nicht (`harw-lens-types/Cargo.toml` hängt nur an `harw-types` und
`harw-macros`, beide < L12).

## Befunde

| ID | Schwere | Datei:Zeile | Befund | Fix-Snippet |
|---|---|---|---|---|
| R2-01 | Mittel | `harw-runtime/Cargo.toml:41` + `sandbox.rs:310-312` | `tempfile` ist als Dev-Dependency eingetragen, wird aber in **keiner** Datei des Crates benutzt (`grep -rn tempfile harw-runtime/` trifft nur den Cargo-Eintrag und einen Kommentar). Die Sandbox-Tests binden stattdessen `std::env::temp_dir()` — also das **gesamte, geteilte** `/tmp` — als Harness-Root. Kein Datenverlust (die Tests schreiben nichts), aber die Bindung ist semantisch falsch (ein Harness-Root ist ein Projekt, kein Systemtemp) und die Negativprobe R2-02 hängt daran. | `fn existing_root() -> tempfile::TempDir { tempfile::TempDir::new().expect("temp dir") }` und an den Aufrufstellen `let root = existing_root(); let root = root.path();` |
| R2-02 | Mittel | `sandbox.rs:356` | `existing_root().join("harw-runtime-no-such-directory-w2b02")` ist ein **fester** Pfad unter dem geteilten `/tmp`. Legt irgendein Prozess (oder ein abgebrochener Vorlauf) dieses Verzeichnis an, schlägt der Test ab da dauerhaft fehl — der Test ist nicht selbst-isolierend. Mit R2-01 verschwindet das Problem, weil der Elternpfad dann je Lauf eindeutig ist. | mit R2-01: `let missing = root.join("no-such-directory");` |
| R2-03 | Niedrig | `harw-runtime/src/config.rs:137-139` | **Fremde Datei** (nicht W2B-02-Eigentum), aber durch den Cargo-Nachtrag faktisch falsch geworden: der Doc-Kommentar der handgebauten `TempDir` sagt „kein `tempfile` in den Dev-Dependencies dieses Crates". Das gilt seit dem Nachtrag nicht mehr; die 35 Zeilen Handarbeit (`config.rs:140-176`) sind jetzt ersetzbar. | An den Eigentümer von `config.rs` geben: `TempDir` durch `tempfile::TempDir` ersetzen, Kommentar streichen. |
| R2-04 | Info | `sandbox.rs:192-202` | F-045 ist für den **Web**-Einstieg heute wirkungslos: `EntryKind::Web.profile().permissions == {ReadWorkspace}` (`spec.rs:176`), und `restrict` schneidet nur. Ein `Observer` und ein `Owner` erhalten am Web-Einstieg denselben Rechtesatz `{R}`. Das ist vertragskonform (§runtime-spec: „Web: nach Tier; Spec: {R}"), aber der Befundtext im Ledger („Ein Observer hatte die Autorität eines Owner") suggeriert eine Verhaltensänderung, die erst greift, wenn das Web-Profil je aufgeweitet wird. Der Test `web_entry_narrowed_by_tier_never_exceeds_its_profile` (`sandbox.rs:407`) prüft folglich eine Tautologie. | Keine Codeänderung; ein Satz im Ledger, dass `permissions_for_tier` als *Obergrenze für die Zukunft* wirkt, nicht als heutige Verengung. |
| R2-05 | Info | `ceiling.rs:220-227` | `every_entry_gets_exactly_one_of_the_two_ceilings` ist tautologisch: `root_ceiling` hat genau zwei Zweige, mehr als zwei Ergebnisse sind nicht konstruierbar. Der Test kann nicht fehlschlagen. | Aussagekräftiger: je Einstieg die **erwartete** Politik gegen die Vertragstabelle prüfen (`Web`/`McpServe`/`JobPrompt`/Gateways → `Closed`, Rest → `LocalRoot`). |
| R2-06 | Info | `trace.rs:317` | `span_id` sind die ersten 16 Hexziffern einer UUID v4; Index 12 ist dort per RFC 4122 konstant `'4'`. Die Spanne trägt damit 60 statt 64 Bits Entropie. Für Span-IDs unkritisch (das Muster stammt aus `harw-core::new_span_id`), aber es ist kein gleichverteiltes 16-Hex-Wort. | Optional: `draw_hex32().chars().rev().take(16).collect()` (hinteres Ende trägt keine Formatbits). |
| R2-07 | Info | `trace.rs:372-383` | Zwei Tests sind probabilistisch, nicht deterministisch (`two_roots_never_share_a_trace_id`, `trace_id_and_span_id_are_independent_draws`). Kollisionswahrscheinlichkeit ≈ 2⁻⁶⁰; praktisch stabil, formal kein deterministischer Test. | Keine; als bekannt notieren. |
| R2-08 | Info | `sandbox.rs:93` | `bind_project` gibt `harw_sandbox::WorkspaceBinding` über den vollen Pfad zurück, während `WorkspaceRegistration`/`WorkspaceRegistry` importiert sind. Nur Stilbruch, kein Fehler. | `use harw_sandbox::{..., WorkspaceBinding};` und `-> RuntimeResult<WorkspaceBinding>`. |

## Was geprüft wurde und stimmt

**Pfade / Sichtbarkeit** (alle einzeln in der Quelle nachgeschlagen):

- `harw_sandbox`: `SandboxSpec::from_resolved` (`lib.rs:848`, 2 Argumente
  `WorkspaceBinding, PermissionSet`), `restrict(&PermissionSet)` (`:898`),
  `permissions()` (`:879`), `network_scope()` (`:885`), `workspace()`
  (`:874`), `ensure_child_of(&Self) -> SandboxResult<()>` (`:950`),
  `WorkspaceBinding::{tenant,workspace,canonical_root}` (`:661-671`),
  `WorkspaceRegistration{tenant,workspace,root}` (`:739`),
  `WorkspaceRegistry::build(&Path, impl IntoIterator<..>)` (`:754`),
  `resolve(&TenantId,&WorkspaceId)` (`:809`), `PermissionSet::{empty,
  from_policy,contains,is_subset_of}` (`:91-122`), `NetworkScope::is_empty`
  (`:314`). `SandboxError` hat `impl fmt::Display` (`:1002`) — die vier
  `format!("…{error}")` in `bind_project` sind gedeckt.
- `harw_types`: `TenantId::from_str`/`WorkspaceId::from_str` sind die
  **inhärenten**, infalliblen Konstruktoren (`ids.rs:58`, `-> Self`), nicht
  `FromStr::from_str`; `sandbox.rs` importiert `std::str::FromStr` nicht,
  die Auflösung ist also eindeutig. `SessionId::{new,as_str}` (`ids.rs:36,
  62`), `PermissionTier` mit `Copy` (`principal.rs:40`) — `pair[0]` in
  `ALL_TIERS.windows(2)` kopiert, kein Move-Fehler.
- `harw_context`: `ContextCeiling{sections,max_trust,budget}` (`ceiling.rs:95`,
  alle Felder `pub`, `PartialEq`), `intersect` (`:135`),
  `ContextBudgetSpec{total,per_section}` (`budget.rs:59`, `total:
  harw_lens_types::BudgetSpec`), `SectionName::try_new -> Result<_,
  ContextError>` (`fragment.rs:193`, `ContextError: Debug` → `expect` ok),
  `TrustClass::{Instruction,Data}` (`fragment.rs:76`). Alle vier am
  Crate-Root re-exportiert (`lib.rs:89-92`).
- `harw_lens_types::BudgetSpec { total: u32 }` (`rank.rs:82-88`, Feld `pub`,
  kein `#[non_exhaustive]`), am Root re-exportiert (`lib.rs:56`). Die
  Struct-Literale in `ceiling.rs:130,140` sind korrekt.
- `harw_core`: `HISTORY_TAIL_SECTION: &str` (`history_tail.rs:115`) — als
  `const` in `[&str; 4]` zulässig; `ChildLimits` mit `Copy`
  (`child_controller.rs:148`), `conservative()` (`:161`),
  `with_max_children()` (`:209`, hebt `0` auf `1` an — der Befund ist echt
  und in `budget.rs:316-319` festgehalten). Beide am Root re-exportiert
  (`lib.rs:39`).
- `harw_model_catalog::profile_for` ist am **Crate-Root** re-exportiert
  (`lib.rs:41`) — der Aufruf in `budget.rs:221` ist gültig;
  `runtime::DelegationPolicy` (`runtime.rs:125`) ebenfalls erreichbar.
  `max_child_fanout: u8` → `usize::from(..)` ist die richtige Konversion.
- `harw_observe::TraceContext` (`lib.rs:100`), `new(impl Into<String>, impl
  Into<String>) -> Result<Self, ObserveError>` (`trace.rs:80`), Felder
  `trace_id`/`span_id`/`parent_span_id` `pub` (`:42-50`).
- `harw_plan::PlanNodeKind` am Root re-exportiert (`lib.rs:90`), `Copy`
  (`types.rs:91`) — `kind` bleibt nach `plan_node_sandbox(kind, ..)` für
  `"{kind:?}"` verfügbar.
- `harw_config::ResolvedConfig` `derive(Default)` (`discovery.rs:23`),
  `harness.default_model: Option<String>`, `models: HashMap<String,
  ModelToml>` → `contains_key(model_id: &str)` über `Borrow<str>` gültig.

**Erschöpfende `match`:**

- `EntryKind` (11 Varianten laut `spec.rs:23-45`): `tenant_name`
  (`sandbox.rs:62-70`) und `RootBudget::from_config` (`budget.rs:143-160`)
  decken alle 11 ab, ohne `_`-Arm. Die drei Testtabellen `ALL_ENTRIES`
  (`sandbox.rs:270`, `ceiling.rs:152`, `trace.rs:333`) listen alle 11.
- `PermissionTier` (4): `permissions_for_tier` (`sandbox.rs:195-201`), kein
  `_`-Arm.
- `PlanNodeKind` (10): `kind_may_write` (`sandbox.rs:213-224`) nennt alle
  zehn ausdrücklich; `ALL_KINDS` (`:287`) ebenso, Länge `10` stimmt.
- `CeilingPolicy` (2): `root_ceiling` (`ceiling.rs:118-144`).

**Semantik gegen die Vertragstabelle:**

- `root_sandbox` liest die Rechte ausschließlich aus
  `EntryKind::profile().permissions`; die Tabelle in `spec.rs:135-210`
  deckt sich Zeile für Zeile mit §runtime-spec: `Tui/OneShot/LocalEcho/
  Analyze/Doctor` = `{R,W,X}`, `Web` = `{R}`, `McpServe/JobPrompt` = `{}`,
  `JobPlanNode` = `{R,W}`, beide Gateways = `{}`.
- **Netz nirgends:** `from_resolved` setzt `NetworkScope::empty()`
  (`harw-sandbox/src/lib.rs:852`); `with_network_scope` wird in keiner der
  vier Dateien aufgerufen (geprüft); `restrict` lässt den Scope unverändert,
  kann ihn also aus dem leeren Zustand nicht füllen. `NetworkAccess` kommt
  in keinem Profil und in keiner Tier-Zeile vor. Beide Achsen sind
  zusätzlich getestet (`sandbox.rs:328-341`, `:436-439`).
- `plan_node_sandbox` nimmt `EntryKind::JobPlanNode` als Obergrenze und
  schneidet nur (`restrict`); `X`/`Netz` sind in der Obergrenze nicht
  enthalten und damit auf keinem Pfad erzeugbar. Korrekt.
- `Web` im lokalen Budget-Block (`budget.rs:143-149`) ist begründet und
  vertragsneutral (§runtime-spec macht keine Budgetaussage).

**Monotonie-Aussagen — sind sie wirklich getestet?** Ja, alle vier:

- Sandbox/Tier: `web_entry_narrowed_by_tier_never_exceeds_its_profile`
  (`sandbox.rs:407`) ruft `ensure_child_of` gegen den Elternteil — das ist
  die echte Reduktionsprüfung, nicht nur ein Rechtevergleich. Zusätzlich
  `tiers_are_monotone_…` (`:381`) mit `is_subset_of` über alle
  benachbarten Tier-Paare.
- Sandbox/Plan-Knoten: `plan_node_sandbox_is_monotone_…` (`:421`) prüft
  `ensure_child_of` über **alle** 10 Knotenarten × 2 `may_write`-Werte.
- Decke: `closed_is_a_reduction_of_local_root` (`ceiling.rs:213`) über
  `intersect`. Nachgerechnet: Sektionen → ∅; `max_trust` → `Data` (niedrigerer
  `trust_rank`); `budget.total` → `min(1_000_000, 0) = 0`; `per_section`
  beidseitig leer. Die Gleichheit gilt.
- Kind-Grenzen: `child_limits_never_exceed_the_conservative_grant`
  (`budget.rs:323`) und `forbidden_delegation_yields_zero_children` (`:306`).
  `DEFAULT_PROFILE.max_child_fanout == 2` (`runtime.rs:374`) ≤ 8, also hält
  auch `an_unknown_model_falls_back_to_the_catalog_default`;
  `"deepseek-reasoner"` hat `Forbidden` + `max_child_fanout: 0`
  (`runtime.rs:653-664`), die Assertions stimmen.

**MSRV 1.85 / Edition 2024:** benutzt werden nur `[T; N]: IntoIterator`
(1.53), `collect::<Result<_,_>>()`, `Duration::from_secs` als `const`
(1.32), `const fn` mit `match` über Enums (1.46), `let`-Bindings mit
Shadowing, `impl Into<String>`. Nichts davon überschreitet 1.85. Kein
`unsafe` (Crate-Wurzel `#![forbid(unsafe_code)]`, `lib.rs:5`).

**Clippy (`-D warnings`, Workspace-Lints = nur `unsafe_code = "forbid"`,
`Cargo.toml:156-157`; `make clippy-tests` läuft `--all-targets
--all-features`):** keine Treffer der Default-Gruppen gefunden.
Insbesondere: `filter(char::is_ascii_hexdigit)` (`trace.rs:282`) passt auf
`FnMut(&char) -> bool` — korrekt, kein `redundant_closure`;
`('a'..='f').contains(&c)` (`trace.rs:351`) löst `manual_is_ascii_check`
**nicht** aus (die Lint kennt nur `a..=z`, `A..=Z`, `0..=9`);
`assert_ne!(trace.trace_id[..16], trace.span_id)` (`trace.rs:382`) ist über
`impl PartialEq<String> for str` gedeckt; die funktionale Update-Syntax
`{ .. conservative }` (`budget.rs:224-227`) liest `conservative` im
Feld-Initialisierer **vor** dem Move des Basisausdrucks — NLL-konform.
Kein `#[allow]`, kein `todo!`/`unimplemented!`, kein `unwrap()` in den vier
Dateien (geprüft).

**Doc-Tests / Intra-Doc-Links:** `harw_runtime::sandbox::permissions_for_tier`
(`sandbox.rs:182-190`), `harw_runtime::ceiling::root_ceiling`
(`ceiling.rs:109-115`) und `harw_runtime::RootBudget` (`budget.rs:97-101`)
sind alle über tatsächlich existierende `pub use`-Pfade erreichbar
(`lib.rs:19-22`). `harw_types::Principal::tier` (`principal.rs:162`),
`harw_core::HISTORY_TAIL_SECTION`, `harw_model_catalog::runtime::
DelegationPolicy`, `ContextBudgetSpec::tighten` existieren alle — kein
`broken_intra_doc_links`.

## Fix-Aufgaben nach Datei

### `harw-runtime/src/sandbox.rs` (Eigentum W2B-02)
1. **R2-01 + R2-02 zusammen:** `existing_root()` auf `tempfile::TempDir`
   umstellen und die sechs Aufrufstellen (`:317, :330, :345, :356, :408,
   :422, :451`) auf `dir.path()` ziehen. Damit ist jeder Test in einem
   eigenen, nach dem Lauf entfernten Verzeichnis, die Negativprobe bekommt
   einen garantiert nicht existierenden Pfad, und die Dev-Dependency
   `tempfile` ist nicht mehr unbenutzt.
2. **R2-08 (optional):** `WorkspaceBinding` importieren statt voll zu
   qualifizieren.

### `harw-runtime/src/ceiling.rs` (Eigentum W2B-02)
3. **R2-05 (optional):** `every_entry_gets_exactly_one_of_the_two_ceilings`
   gegen die erwartete Politik je Einstieg prüfen statt gegen „eine von
   zweien".

### `harw-runtime/src/trace.rs` (Eigentum W2B-02)
4. **R2-06/R2-07 (optional, Info):** nichts zu tun; als bekannt vermerken.

### `harw-runtime/src/budget.rs` (Eigentum W2B-02)
Keine Aufgabe.

### `harw-runtime/Cargo.toml` (Eigentum ORCH)
Keine Aufgabe. Beide Nachträge sind korrekt und nötig: `harw-lens-types`
schließt die im Ledger gemeldete Blockade, `tempfile` wird nach R2-01
tatsächlich benutzt (heute noch nicht — ohne R2-01 bliebe der Eintrag
unbenutzter Ballast, was hier aber **keine** Warnung auslöst, weil
`unused_crate_dependencies` nicht in den Workspace-Lints steht).

### `harw-runtime/src/config.rs` (fremdes Eigentum)
5. **R2-03:** Doc-Kommentar `:137-139` ist jetzt falsch; handgebaute
   `TempDir` durch `tempfile::TempDir` ersetzbar. An den Eigentümer geben.

## Blocker

Keine. Es gibt keine Fix-Aufgabe, die das Kompilieren verhindert.
