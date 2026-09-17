# Research — bestehende Speicher-/Identitäts-Infrastruktur

**Ziel:** Klären, was bereits existiert, bevor irgendetwas als "neu" geplant
wird (explizite Vorgabe des Auftrags).

## 1. UIA-Bundle heute (`harw-config/src/loader.rs`, `harw-cli/src/uia_bootstrap.rs`,
   `harw-registry-defaults/src/agent_definition_tools.rs`)

- Ein UIA-Verzeichnis (`~/.harw/agents/<name>/`) enthält aktuell genau vier
  Dateien: `definition.toml`, `agent.toml`, `Personality.md`, `USER.md`.
  Belegt durch `uia_bootstrap::write_generated_uia` (Zeilen ~302–370) und
  `agent_definition_tools::commit_uia_bundle` (identisches Vier-Datei-Set).
- `harw_config::loader::load_uia_personalization(agent_dir)` liest **nur**
  `Personality.md` und `USER.md` und reicht sie als getrennt gekennzeichnete
  Kontext-Fragmente weiter ("# UIA-Persönlichkeit und Antwortverhalten" /
  "# Nutzerkontext (USER.md)"). Es gibt **keinen** Loader-Pfad für eine
  eigenständige `identity.md`/`Identity.md`.
- `USER.md`-Parsing ist absichtlich minimal: nur eine Zeile im Format
  `Name: X` oder `- **Name:** X` wird als Anzeigename erkannt
  (`load_uia_user_name`/`user_name_from_line`). Der Rest von `USER.md` ist
  freier, ungeparster Fließtext, der 1:1 in den Kontext einfließt.
- **Bereits vorhandener, aber ungenutzter `identity`-Ansatz:** In
  `agent_definition_tools.rs::build_agent_toml` gibt es schon ein
  `identity_md: Option<&str>`-Argument für `agents.write_uia`. Der
  Docstring dort sagt es explizit: *"Der bestehende Loader
  (`harw_config::load_uia_personalization`) liest keine eigene
  `Identity.md`-Datei (Contract Nachtrag K: 'sonst weglassen') —
  `identity_md` landet deshalb, wenn angegeben, als zusätzliches Feld in
  `agent.toml` statt in einer eigenen, ungelesenen Datei (ABWEICHUNG, siehe
  Bericht)."* Das ist eine bekannte, dokumentierte Lücke genau an der
  Stelle, die dieser Auftrag schließen soll: `identity_md` wird als String
  entgegengenommen, aber landet als TOML-Feld `identity = "..."` in
  `agent.toml`, nicht als eigene Datei, weil niemand sie liest. Ein
  Umsetzungsauftrag muss (a) den Loader um `identity.md` erweitern und
  (b) `agents.write_uia`/`commit_uia_bundle` so ändern, dass `identity_md`
  tatsächlich in eine `identity.md`-Datei geschrieben wird statt in
  `agent.toml`.
- Freigabe-Modell: `agents.write_uia` erzeugt **immer** einen Vorschlag
  unter `<profile_agents_dir>/.proposals/<id>/` mit `review_level =
  "user_required"` (UIAs aktivieren sich nie selbst automatisch, Nachtrag K)
  und wird erst über `agents.commit_proposal` (mit `user_confirmed=true`,
  strukturell nicht vom Modell erzwingbar, siehe unten) wirksam. Kein
  Werkzeug in dieser Datei ist in `AUTO_APPROVED_TOOLS` gelistet
  (`harw-registry-defaults/src/lib.rs`) — `DefaultApprovalPolicy` fragt
  daher für jeden Aufruf fail-closed beim Menschen nach, *bevor* der Code
  überhaupt läuft.

## 2. `harw-memory`-Crate (Design: `docs/design/harw-memory.md`, Code:
   `harw-memory/src/*.rs`)

- **Zweck:** generische, tokensparsame Session-Memory mit drei Tiers
  (HOT ≤100 Zeilen, WARM ≤200 Zeilen/Namespace, COLD unbegrenzt),
  Signal-getrieben (`Correction`, `Reflection`, `PatternHint`),
  append-only + separater `maintain()`-Pass (Promotion/Demotion, idempotent
  über eine `workflow.json`-State-Machine).
- **Root ist bereits parametrisierbar, nicht fest verdrahtet:**
  `FileMemoryStore::open(root: impl AsRef<Path>)` nimmt jeden Pfad entgegen;
  nur der *Default* in der Design-Doc ist global (`<HARWNESS_HOME>/memory/`
  bzw. `~/.harwness/memory/`). Der Crate-Code selbst erzwingt **keinen**
  globalen Pfad. Das ist die wichtigste Erkenntnis für die
  Architektur-Entscheidung unten: eine Pro-Agent-Root ist mit dem
  bestehenden Trait/Store **ohne Crate-Änderung** möglich, indem der Aufrufer
  `FileMemoryStore::open(agent_dir.join("memory"))` statt der globalen
  Default-Root übergibt.
- Layout pro Root: `HOT.md`, `INDEX.md`, `warm/{namespace}.md`,
  `cold/{namespace}.md`, `signals/*.jsonl`, `state.json`, `workflow.json`.
  Es gibt **kein** datumsbasiertes Unterverzeichnis-Layout in diesem Crate
  — Namespaces sind frei (`project/harwness`, `domain/rust`), aber nicht an
  ein Datum gebunden.
- `capture.rs` (`ProjectMemoryCapture`, `consolidate_project_memories`) ist
  bereits eine **projektbezogene** Erfassungsschicht: beobachtet
  Tool-Ergebnisse während einer Session, leitet Dateiwissen
  (`FileKnowledgeIndex`), Recherche-Fakten (`FactStore`/`IncomingStore`) und
  Lektionen (Fehler→Erfolg-Muster) ab, redigiert Inhalte
  (`facts::redact`) und konsolidiert über
  `consolidation::{plan_consolidation, apply_plan, ConsolidationLock}`.
  Das ist strukturell bereits sehr nah an dem, was der Nutzer für
  "projektbezogen, konsolidierend nach oben" beschreibt — aber es ist heute
  an eine **globale** Root gebunden, nicht an einen UIA-Agenten.
- `dream_reflection` (`harw-config/src/internal_models.rs`) ist eine reale,
  verdrahtete interne Modellstelle (`InternalModelPoint::DreamReflection`,
  8 Modellstellen insgesamt) und wird in `harw-cli/src/gateway.rs`
  (~Zeile 1539–1627) tatsächlich für einen "budgetierten Reflexions-Turn →
  review-gated `DreamReport` schreiben" genutzt — **aber** `DreamReport`
  kommt nicht aus `harw-memory`, sondern aus **`harw-knowledge::dream`**
  (siehe unten). `harw-memory::consolidation` ist ein separater,
  regelbasierter Promotion/Demotion-Mechanismus **ohne** LLM-Aufruf (M4 in
  der Design-Doc, "Opt-in Consolidation-Agent", ist laut Doc noch nicht
  gebaut). Es gibt also **keine** Verbindung zwischen `dream_reflection`
  und `harw-memory::consolidation` heute — beide sind reale, aber getrennte
  Mechanismen.

## 3. `harw-knowledge`-Crate (Design: `docs/design/knowledge-surfaces.md`,
   Code: `harw-knowledge/src/*.rs`)

- Eigenständiges Crate, **nicht** dasselbe wie `harw-memory`. Besitzt
  "Artefakte" (kuratiert, promotion-gated, dauerhaft) im Gegensatz zu den
  append-only Transkripten von `harw-session-store`.
- Module: `memory` (core/topic/palace/recall — eigenes, drittes
  Memory-Konzept!), `diary` (append-only Tagebuch — **kein**
  datumsbasiertes Ordner-Layout gefunden, sondern vermutlich
  Frontmatter-Felder auf einzelnen Artefakten), `dream` (Job-Payload/Output
  — `DreamReport { work_id, summary, proposed_topic_updates,
  proposed_palace_promotions, follow_ups }`, `has_proposals()`),
  `workbench` (Scratch je Session/Projekt), `kanban`, `security`,
  `visibility` (`VisibilityScope`, einheitlich erzwungen).
- **Wichtige Klarstellung für den Bericht:** `DreamReport` existiert real
  und ist verdrahtet (`gateway.rs` Zeile ~1760: "Rendert einen
  `DreamReport` als review-gated Markdown-Dokument"). Der vom Nutzer
  erwähnte "gebrochene Intra-Doc-Link" bezog sich vermutlich auf eine
  Doku-Referenz, die `DreamReport` fälschlich in `harw-memory` statt
  `harw-knowledge` vermutete — es ist kein totes Feature, nur in einem
  anderen Crate als vom lib.rs-Kommentar in `harw-memory` suggeriert.
- Damit gibt es **drei** überlappende "Gedächtnis"-Konzepte im Workspace:
  (a) `harw-memory` HOT/WARM/COLD (generisch, Signal-getrieben, global),
  (b) `harw-knowledge::memory` (core/topic/palace, artefakt-basiert,
  promotion-gated, projektbezogen über `VisibilityScope`), (c) die
  vorgeschlagene UIA-eigene `memory/`. Eine vierte, komplett neue
  Implementierung würde die Überlappung auf vier erhöhen — vermieden
  werden soll das laut Auftrag ausdrücklich.

## 4. Steward-Werkzeug-Muster (`agent_definition_tools.rs`) als Vorbild

- Bereits etabliertes Muster für "Werkzeuge, die Agent-Definitionsdateien
  schreiben dürfen": Validierung zuerst (`agents.validate`), dann
  **immer** ein Vorschlag statt Direktschreiben (`agents.write_uia`,
  `agents.write_definition` außer `scope="run"` mit leeren Rechte-Deltas),
  dann getrennte Freigabe (`agents.commit_proposal`, nur mit gesetzter
  `DefinitionAuthorCeiling` und `user_confirmed=true` — strukturell durch
  die vorgelagerte `DefaultApprovalPolicy`-Freigabe abgesichert, nicht durch
  das Modell-Argument selbst).
- Rechte-Algebra (`RightsDelta`, Urheber-Decke vs. Basisrolle) ist für
  *Definitionsdateien* (die Rechte verleihen) gebaut — für reine
  Inhaltsdateien (`identity.md`, `USER.md`, `Personality.md`, `memory/*.md`)
  ist sie **nicht** einschlägig, weil diese Dateien keine Rechte verleihen.
  Die neuen Pflege-Werkzeuge brauchen daher kein Pendant zur
  Rechte-Delta-Prüfung — nur Pfad-Validierung (kein Traversal, Ziel bleibt
  im eigenen Agent-Verzeichnis), Geheimnis-Scan (wie `SECRET_PATTERNS`) und
  Freigabe über die normale `DefaultApprovalPolicy` (fail-closed, nicht in
  `AUTO_APPROVED_TOOLS`).

## 5. Fazit der Recherche

- **Wiederverwendbar ohne Crate-Änderung:** `harw_memory::FileMemoryStore`
  als Speicher-Engine, wenn man ihr eine Pro-Agent-Root übergibt
  (`<agent_dir>/memory/`) statt der globalen Default-Root.
- **Wiederverwendbar mit kleiner Erweiterung:** `harw_config::loader`
  braucht eine neue Funktion `load_uia_identity(agent_dir) ->
  ConfigResult<String>` analog zu `load_system_prompt`, die
  `identity.md` liest; `load_uia_personalization` sollte um ein drittes
  Fragment ("# UIA-Identität") erweitert werden, wenn `identity.md`
  vorhanden ist.
- **Nicht wiederverwendbar, weil andere Ebene:** `harw-knowledge` bleibt
  projekt-/palace-bezogenes, geteiltes Artefaktwissen — bewusst **nicht**
  UIA-privat. Es wird nicht angefasst.
- **Neu zu bauen:** die datums-/projektbezogene Ordnerstruktur *innerhalb*
  einer Pro-Agent-`memory/`-Root (`memory/<projekt-slug>/<datum>/*.md` →
  Konsolidierung → `memory/Memory.md`) — das ist eine dünne
  Organisationsschicht *über* `FileMemoryStore`, kein Konkurrenzsystem
  dazu. Siehe `structure-plan.md` für den Vorschlag.
