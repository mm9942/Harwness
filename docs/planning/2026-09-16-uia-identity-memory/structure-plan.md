# Structure Plan

## 1. Abgrenzung `identity.md` vs. `Personality.md` (mit Beispielen)

| Datei | Beantwortet | Beispiel-Inhalt |
|---|---|---|
| `identity.md` | *Wer/was ist der Agent selbst?* Name, Herkunft/Backstory, Selbstverständnis, Rolle im Leben des Nutzers, was ihn von einer anderen UIA unterscheidet. Stabil, ändert sich selten. | "Ich bin Emily. Ich wurde als persönliche UIA für [Nutzer] eingerichtet, um Coding-Aufträge zu koordinieren. Ich verstehe mich als eigenständige Assistenz, nicht als Rollenspiel-Figur. Mein Name kommt von [...]. Ich existiere seit [Datum der Einrichtung]." |
| `Personality.md` | *Wie verhält sich der Agent, wie klingt er?* Ton, Umgangsformen, Antwortstil, Grenzen im Verhalten (z. B. "keine Emojis", "direkt, nicht ausschweifend"). Kann sich häufiger ändern (Feinjustierung des Tons), ohne dass sich die Identität ändert. | "Du bist warmherzig, aber sachlich. Keine Emojis. Kurze Sätze. Wenn du unsicher bist, sag es direkt, statt zu spekulieren." |

Test der Abgrenzung: *"Würde sich der Agent noch als derselbe Agent
verstehen, wenn dieser Satz sich änderte?"* — Wenn ja → Identity. *"Würde
sich nur der Ton/die Reaktion ändern, aber der Agent bliebe 'derselbe'?"*
— Wenn ja → Personality. Ein Namenswechsel ist Identity; ein
Tonwechsel von "warmherzig" zu "knapp-technisch" ist Personality.

## 2. Abgrenzung Memory vs. Identity/Personality

- **Identity/Personality** = kuratiert, stabil, klein, vom Menschen (oder
  vom Agenten mit Bestätigung) bewusst geschrieben. Kein Lernen aus
  Ereignissen.
- **Memory** = akkumuliert, projekt- und zeitbezogen, wächst automatisch
  aus Ereignissen (Korrekturen, Reflexionen, Projektarbeit), wird
  regelmäßig konsolidiert ("Dreaming"), damit es nicht unbegrenzt wächst.

## 3. Empfehlung: `harw-memory` erweitern statt neu bauen

**Entscheidung:** `harw_memory::FileMemoryStore` als Speicher-Engine
wiederverwenden, mit Root `<agent_dir>/memory/`. Kein neues Crate, kein
Parallelsystem.

**Begründung:**
- `FileMemoryStore::open(path)` ist bereits pfad-agnostisch (siehe
  `dependency-research.md` §2) — eine Pro-Agent-Root ist ohne
  Crate-Änderung möglich.
- Tier-Semantik (HOT/WARM/COLD) passt konzeptionell auf "aktuell relevant /
  projektbezogen abrufbar / archiviert" — muss nur um eine
  Projekt-/Datums-Ebene *innerhalb* von WARM ergänzt werden, kein
  Widerspruch zum bestehenden Design.
- Vermeidet ein viertes Memory-System (siehe Research §5: es gibt bereits
  drei überlappende Konzepte — `harw-memory`, `harw-knowledge::memory`,
  UIA-Bundle-Dateien).

**Konkrete Erweiterung der Verzeichnisstruktur** (innerhalb der neuen
Pro-Agent-Root, zusätzlich zum bestehenden `FileMemoryStore`-Layout):

```
~/.harw/agents/<uia-name>/
├── definition.toml
├── agent.toml
├── identity.md          # NEU (T1–T3)
├── Personality.md
├── USER.md
└── memory/              # NEU (T4–T5) — Root = FileMemoryStore::open(hier)
    ├── Memory.md         # entspricht HOT.md; vom Nutzer gefordertes
    │                     # "Memory-Root" — technisch dieselbe Rolle wie
    │                     # HOT.md, hier nur umbenannt, damit der
    │                     # Agenten-eigene Name nicht mit dem globalen
    │                     # HOT-Konzept verwechselt wird.
    ├── INDEX.md          # unverändert aus FileMemoryStore
    ├── warm/
    │   ├── INDEX.md
    │   └── <projekt-slug>/
    │       └── <YYYY-MM-DD>/
    │           └── NN-<slug>.md   # NEU: Rohnotizen, tagesbezogen,
    │                              #      Vorstufe zur Konsolidierung
    ├── cold/
    ├── signals/
    │   ├── corrections.jsonl
    │   ├── reflections.jsonl
    │   └── patterns.jsonl
    ├── state.json
    └── workflow.json
```

**Konsolidierungsfluss:** Rohnotizen in
`warm/<projekt>/<datum>/*.md` werden durch einen `maintain()`-Lauf (bereits
vorhandener Mechanismus in `FileMemoryStore`, ggf. um eine
projekt-/datums-bewusste Zusammenfassungs-Regel erweitert) schrittweise zu
`Memory.md` (dem Root, ≤100-Zeilen-Limit wie heute `HOT.md`) verdichtet.
Ein optionaler späterer Ausbau kann `dream_reflection`
(`InternalModelPoint::DreamReflection`) als LLM-gestützten
Konsolidierungs-Agenten einhängen — das ist laut Design-Doc (M4,
"Opt-in Consolidation-Agent") bereits vorgesehen und **nicht** Voraussetzung
für M1.

**Verworfene Alternative:** komplett neues Memory-Crate/-Format nur für
UIAs. Verworfen, weil es die Anzahl konkurrierender Speicherformate auf
vier erhöhen würde, ohne einen Vorteil zu bieten, den eine Pro-Agent-Root
von `FileMemoryStore` nicht auch böte.

## 4. Drei Pflege-Werkzeuge: Spezifikation

**Entscheidung: ein parametrisiertes Werkzeug, nicht drei separate.**

Begründung: Alle drei Ziele (`identity.md`, `USER.md`, `Personality.md`)
teilen exakt denselben Ablauf — Pfad-Validierung im eigenen
Agent-Verzeichnis, Geheimnis-Scan, Diff-Anzeige, Freigabe über
`DefaultApprovalPolicy`. Drei separate Werkzeuge würden diesen Ablauf
dreifach duplizieren (Contract-Pflicht "kein Silent Inference", aber keine
Pflicht zu redundanten Tool-Definitionen). Ein `enum target`-Parameter
macht das Freigabe-Prompt für den Menschen zudem einheitlich lesbar
("UIA möchte `identity.md` ändern: ...").

```
uia_self.update_document {
  target: "identity" | "user" | "personality",   // → identity.md / USER.md / Personality.md
  content: string,                                 // vollständiger neuer Dateiinhalt
  reason: string,                                  // kurze Begründung, erscheint im Freigabe-Prompt
}
```

- **Rolle:** nur registriert, wenn der aufrufende Agent `role =
  "user-interface"` ist (Rollen-Gating wie bestehende
  `profile_for_role`-Logik). Root-Orchestratoren/Worker bekommen dieses
  Werkzeug **nicht** — sie haben kein eigenes `identity.md`/`USER.md`.
  Begründung: diese Dateien sind laut Auftrag ausdrücklich UIA-persönlich
  ("jede UIA-Identität hat ihr eigenes Gedächtnis"); ein Root-Orchestrator,
  der `USER.md` einer UIA änderte, wäre ein Autoritäts-Übergriff analog zum
  in `agent_definition_tools.rs` verhinderten "authority elevation".
- **Ziel-Datei-Auflösung:** immer relativ zum *eigenen*
  `agent_dir` des aufrufenden Agenten (kein Parameter für einen fremden
  Agentenordner) — verhindert, dass eine UIA die Dateien einer anderen UIA
  ändert.
- **Pfad-Sicherheit:** dieselbe Traversal-Prüfung wie
  `harw_config::loader::configured_file_path` (kein Absolut-/`..`-Pfad
  nötig, da `target` ein geschlossenes Enum ist, keine freie
  Pfadangabe — das ist bewusst sicherer als ein freier `path`-Parameter).
- **Geheimnis-Scan:** dieselben `SECRET_PATTERNS` wie
  `agent_definition_tools.rs` vor dem Schreiben anwenden.
- **Freigabe:** **kein** Eintrag in `AUTO_APPROVED_TOOLS` — jeder Aufruf
  pausiert beim Menschen, exakt wie `agents.write_uia`. Kein
  `agents.commit_proposal`-Analogon nötig, da diese Dateien keine Rechte
  verleihen (keine Rechte-Delta-Prüfung erforderlich, siehe Research §4) —
  ein einzelner freigabepflichtiger Schreibvorgang reicht.
- **Diff-Anzeige:** vor dem Schreiben ein einfaches Zeilen-Diff gegen den
  bestehenden Inhalt anzeigen (wiederverwendbar:
  `agent_definition_tools::simple_line_diff`, ggf. in ein gemeinsames
  Hilfsmodul verschieben, wenn beide Provider es brauchen — Entscheidung im
  Umsetzungsauftrag).
- **Memory-Werkzeuge (falls gewünscht) sind explizit NICHT Teil dieser
  drei Werkzeuge:** Rohnotizen unter `memory/warm/<projekt>/<datum>/`
  werden nicht über `uia_self.update_document` geschrieben, sondern über
  das bestehende `Memory::record(Signal)`-API (`harw-memory`). Das hält
  die Grenze aus §2 auch auf Werkzeug-Ebene ein: kuratierte
  Identitäts-/Verhaltensdateien vs. akkumulierendes Gedächtnis laufen über
  unterschiedliche Mechanismen.

## 5. Betroffene Dateien/Module für den Umsetzungsauftrag

- `harw-config/src/loader.rs` — `load_uia_identity`, Erweiterung von
  `load_uia_personalization`.
- `harw-config/src/harness_config.rs` — falls dort Feldnamen für die
  UIA-Bundle-Dateiliste zentral gepflegt werden (prüfen).
- `harw-registry-defaults/src/agent_definition_tools.rs` —
  `build_agent_toml` korrigieren, `commit_uia_bundle`/`propose_uia` um
  `identity.md` erweitern, neuer `ToolProvider` für
  `uia_self.update_document`.
- `harw-registry-defaults/src/profile.rs` — Rollen-Gating für das neue
  Werkzeug (nur `user-interface`).
- `harw-registry-defaults/src/lib.rs` — sicherstellen, dass
  `uia_self.update_document` **nicht** in `AUTO_APPROVED_TOOLS` landet.
- `harw-cli/src/uia_bootstrap.rs` — Ersteinrichtung um `identity.md`
  erweitern (Frage + Datei + Tests).
- `harw-memory/src/file_store.rs` bzw. ein neues Modul (Name TBD im
  Umsetzungsauftrag, Vorschlag `agent_root.rs`) — Projekt-/Datums-Layout
  über `FileMemoryStore` (nur additiv, kein Bruch bestehender Tests).
- Laufzeit-Verdrahtung der Pro-Agent-Memory-Root: Ort muss im
  Umsetzungsauftrag lokalisiert werden (Kandidat:
  `harw-runtime/src/assembly.rs`, wo `ServiceMap`/`Arc<dyn Memory>`
  vermutlich zusammengesetzt wird — in diesem Planungsauftrag nicht
  gelesen, daher als offene Recherche für die Umsetzung markiert statt
  spekulativ als Fakt behandelt).
