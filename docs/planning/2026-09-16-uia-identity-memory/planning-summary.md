# Planning Summary — UIA-Identität, Memory-Root, Pflege-Werkzeuge

**Status:** Planung abgeschlossen, backlog-ready für `scrum-master`.
**Artefakte:** `decomposition.md`, `dependency-research.md`,
`parent-tasks.md` (T1–T8), `goals.md`, `structure-plan.md`,
`test-and-error-plan.md`. Kein `database-plan.md` — keine DB betroffen.

## Kernaussagen

1. **Drei überlappende Speicher-/Identitätssysteme existieren bereits**
   (`harw-memory` HOT/WARM/COLD, `harw-knowledge::memory`
   core/topic/palace, UIA-Bundle-Dateien) — keine vierte Implementierung
   bauen. Empfehlung: bestehendes `harw_memory::FileMemoryStore`
   wiederverwenden, mit `<agent_dir>/memory/` als Root statt der globalen
   Default-Root. Das ist ohne Crate-Änderung möglich
   (`FileMemoryStore::open(path)` ist bereits pfad-agnostisch).
2. **`identity.md` schließt eine bereits dokumentierte, bekannte Lücke:**
   `agents.write_uia` nimmt heute schon `identity_md` entgegen, schreibt es
   aber (laut eigenem Docstring als "ABWEICHUNG" markiert) als Feld
   `identity = "..."` in `agent.toml`, weil kein Loader eine eigene Datei
   liest. Der Umsetzungsauftrag muss (a) einen Loader
   `load_uia_identity` ergänzen und (b) `build_agent_toml`/
   `commit_uia_bundle`/`propose_uia` korrigieren, damit `identity_md`
   tatsächlich in `identity.md` landet.
3. **`dream_reflection` ist real und verdrahtet**, aber gehört zu
   `harw-knowledge::dream::DreamReport`, nicht zu `harw-memory`. Die beiden
   Konsolidierungsmechanismen sind heute getrennt; die neue
   Projekt-/Datums-Schicht in `memory/warm/<projekt>/<datum>/` kann später
   optional an `dream_reflection` andocken (M4 laut Design-Doc), ist aber
   nicht Voraussetzung für die erste Umsetzungsstufe.
4. **Abgrenzung Identity/Personality/Memory** ist mit dem Test
   "Würde sich der Agent noch als derselbe Agent verstehen, wenn sich
   dieser Satz änderte?" (→ Identity) vs. "ändert sich nur Ton/Reaktion"
   (→ Personality) vs. "wächst automatisch aus Ereignissen und wird
   konsolidiert" (→ Memory) konkret gefasst, mit Beispielsätzen in
   `structure-plan.md` §1–2.
5. **Drei Pflege-Werkzeuge → Entscheidung: ein parametrisiertes Werkzeug**
   `uia_self.update_document {target, content, reason}` statt drei
   separater Tools, um Duplikation des identischen
   Validierungs-/Freigabe-Ablaufs zu vermeiden. Nur für Agenten mit
   `role = "user-interface"` registriert; nicht in `AUTO_APPROVED_TOOLS`
   (fail-closed, Freigabe-Prompt vor jedem Aufruf, wie
   `agents.write_uia`). Wirkt nur auf das eigene Agent-Verzeichnis.
   `memory/`-Rohnotizen laufen bewusst **nicht** über dieses Werkzeug,
   sondern über das bestehende `Memory::record(Signal)`-API.

## Offene Punkte für den Umsetzungsauftrag (bewusst nicht in dieser Planung entschieden)
- Exakter Ort der Laufzeit-Verdrahtung der Pro-Agent-Memory-Root
  (Kandidat `harw-runtime/src/assembly.rs`, nicht gelesen in dieser
  Planungsrunde — als offene Recherche markiert statt spekuliert).
- Ob `identity.md` bei fehlender Angabe einen neutralen Platzhaltertext
  bekommt (wie `USER.md` laut `uia_bootstrap.rs`) oder ganz weggelassen
  wird.
- Ob `simple_line_diff` aus `agent_definition_tools.rs` in ein gemeinsames
  Hilfsmodul verschoben wird, wenn der neue `uia_self.update_document`-
  Provider es ebenfalls braucht.

## Nebenbemerkung (nicht Teil dieses Auftrags)
Während der Recherche traf eine Nachricht einer anderen, parallel
laufenden Agenten-Session ein (Thema: DeepSeek/GLM-Modellverhalten bei
gestrandeten sgh-flow-Crate-Analysen). Das ist inhaltlich unabhängig von
diesem Planungsauftrag; es wurde nicht bearbeitet und sollte, falls
relevant, separat an die richtige Session weitergeleitet werden.

## Reporting-Kette
`worker (dieser Planungslauf, in Ermangelung eines Agent-Spawn-Werkzeugs
in dieser Umgebung direkt durch den Planning-Orchestrator selbst
durchgeführt) → planning-orchestrator → Claude (main) → Nutzer.`
