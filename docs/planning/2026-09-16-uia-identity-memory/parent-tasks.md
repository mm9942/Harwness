# Parent Tasks (Backlog-Grundlage für scrum-master)

Jede Aufgabe ist so geschnitten, dass sie von `development-orchestrator`
unabhängig (aber in dieser Reihenfolge) angenommen werden kann.

## T1 — `identity.md`: Loader + Abgrenzungs-Dokumentation
- Neue Funktion `harw_config::loader::load_uia_identity(agent_dir: &Path)
  -> ConfigResult<String>` analog zu `load_system_prompt`
  (`configured_file_path` + `read_optional_file`, Dateiname `"identity.md"`).
- `load_uia_personalization` (oder ein neuer Sammel-Aufruf) um ein drittes,
  eigenständig gekennzeichnetes Fragment "# UIA-Identität" erweitern, das
  **vor** Personality/User eingehängt wird (Reihenfolge: Identität → Ton →
  Nutzerkontext).
- Datei: `harw-config/src/loader.rs`.
- Abnahmekriterium: bestehende Tests für `load_uia_user_name`/
  `load_uia_personalization` bleiben grün; neue Tests für
  `load_uia_identity` (Datei fehlt → leer, Datei vorhanden → Inhalt,
  Pfad-Traversal → `ConfigError::Invalid`).

## T2 — `agents.write_uia` korrigieren: `identity_md` in eigene Datei statt `agent.toml`
- `build_agent_toml` in `harw-registry-defaults/src/agent_definition_tools.rs`
  darf `identity_md` **nicht** mehr als `identity = "..."`-Feld in
  `agent.toml` schreiben (bekannte, dokumentierte Abweichung, siehe
  `dependency-research.md` §1).
- `commit_uia_bundle` und `propose_uia` um eine fünfte Datei
  `identity.md` erweitern (Dateiliste `[(PathBuf, &str); 5]`).
- `WriteUiaArgs.identity_md` bleibt optional; ohne Angabe wird `identity.md`
  nicht angelegt (kein leeres Pflichtfeld, anders als `user_md`, das laut
  Docstring einen Platzhalter bekommt — hier abwägen: Platzhalter oder
  Weglassen, siehe `structure-plan.md`).
- Datei: `harw-registry-defaults/src/agent_definition_tools.rs`.

## T3 — `uia_bootstrap.rs`: interaktive Ersteinrichtung um `identity.md` erweitern
- `write_generated_uia` und der interaktive Fragenfluss
  (`harw-cli/src/uia_bootstrap.rs`) um eine Identitätsfrage/-datei
  erweitern, damit neu angelegte UIA-Bundles von Anfang an ein `identity.md`
  bekommen (auch wenn zunächst nur ein neutraler Platzhaltertext).
- Bestehende Tests (Zeile ~620, ~637: Datei-Set-Assertion) müssen um
  `identity.md` ergänzt werden.

## T4 — Pro-Agent-`memory/`-Root verdrahten
- Am Ort, an dem eine UIA-Session heute `Arc<dyn Memory>` in die
  `ServiceMap` einhängt (Konsolidierungspunkt: `harw-runtime/src/assembly.rs`
  oder Äquivalent — muss im Umsetzungsauftrag lokalisiert werden, da dieser
  Planungsauftrag keine Laufzeit-Verdrahtung untersucht hat), den
  Default-Root **pro UIA** auf `<agent_dir>/memory/` statt der globalen
  `<HARWNESS_HOME>/memory/`-Root umstellen, wenn der aktive Agent eine UIA
  ist. Root-Orchestratoren/Worker behalten die globale Root (oder eine
  projektbezogene, das ist außerhalb dieses Auftrags).
- Kein Crate-Change in `harw-memory` nötig (siehe Research §5) —
  `FileMemoryStore::open(path)` nimmt jeden Pfad.

## T5 — Projekt-/Datums-Organisationsschicht über `FileMemoryStore`
- Neue dünne Schicht (Vorschlag: neues Modul `harw-memory/src/agent_root.rs`
  oder ein Helfer in `harw-config`/`harw-cli`, TBD im Umsetzungsauftrag), die
  pro Projekt und Tag eine Rohnotiz-Datei anlegt:
  `memory/<projekt-slug>/<YYYY-MM-DD>/<laufende-nr>-<slug>.md`.
  Diese Rohnotizen sind **zusätzlich** zu, nicht statt der bestehenden
  `signals/*.jsonl` — sie sind für Menschen lesbarer Rohtext, den
  `dream_reflection`/Konsolidierung später einliest.
- Konsolidierung (regelbasiert wie `harw-memory::consolidation`, optional
  später LLM-gestützt über `dream_reflection`) verdichtet
  `memory/<projekt>/<datum>/*.md` schrittweise nach oben zu
  `memory/Memory.md` (Root-Zusammenfassung, an `HOT.md`-Limit angelehnt).
- Abgrenzung zu `harw-memory`s HOT/WARM/COLD: `Memory.md` **ist** die neue
  Bezeichnung für das, was heute `HOT.md` heißt, wenn der Root
  agenten-eigen ist — kein zweites Tier-System, siehe
  `structure-plan.md` §Empfehlung.

## T6 — Drei Pflege-Werkzeuge
- `identity.update` / `user.update` / `personality.update` (oder ein
  parametrisiertes `uia_self.update{target, content}` — Entscheidung siehe
  `structure-plan.md` §4) als neue `ToolProvider`-Implementierung, analog
  zum Muster in `agent_definition_tools.rs`, aber **ohne** Rechte-Delta-
  Prüfung (reine Inhaltsdateien).
- Rollenbindung: nur für Agenten mit `role = "user-interface"` registriert
  (Vorbild: `profile_for_role`/Rollen-Gating in
  `harw-registry-defaults/src/profile.rs`).
- Freigabe: fail-closed über `DefaultApprovalPolicy` (nicht in
  `AUTO_APPROVED_TOOLS` aufnehmen).
- Ziel-Datei bleibt im eigenen Agent-Verzeichnis (Pfad-Traversal-Schutz wie
  `configured_file_path`).

## T7 — Migrationspfad für bestehende UIA-Verzeichnisse
- Bestehende Bundles (z. B. `emily-ui`) ohne `identity.md`/`memory/`:
  Loader müssen mit fehlender Datei/fehlendem Verzeichnis robust umgehen
  (bereits das Verhalten von `read_optional_file`/`FileMemoryStore::open`,
  das Verzeichnisse bei Bedarf anlegt) — keine Migrationsskripte nötig,
  aber ein Hinweis im Umsetzungsauftrag, dass "fehlt" ≠ "Fehler" gilt.

## T8 — Tests & Doku (siehe `test-and-error-plan.md`)
