# Memory v3 — Langzeitgedächtnis (LTM) mit Scopes, Fakten und Konsolidierung

**Status:** Design-Anker (Vertrag für die Umsetzung).
**Baut auf:** `docs/design/memory-v2.md` (STM/LTM-Tiers HOT/WARM/COLD, Signale, Heartbeat — bereits in `harw-memory` umgesetzt).
**Vorbilder (geprüft):**
- `~/codex/codex-rs/memories/` — zweistufige Pipeline: Phase 1 extrahiert pro Session eine strukturierte Erinnerung per Modell, Phase 2 konsolidiert global durch einen eigenen Agenten; Artefakte liegen als Dateien mit git-Baseline; Auswahl nach `usage_count` und `last_usage`, Verfall über `max_unused_days`.
- `~/.annabel/projects/<projekt>/memory/` bzw. Claude Code — eine Datei pro Fakt, YAML-Frontmatter (`name`, `description`, `metadata.type`), ein `MEMORY.md` als Index, Querverweise über `[[name]]`.
- `docs/design/config-structure.md` und `harw-scopes-contract.md` — Lebensdauern Session / Projekt / Global.

---

## 1. Warum v3

v2 liefert Tiers, Signale und einen deterministischen Heartbeat, aber:

1. **Kein Projektbezug.** Alles landet im Profil; Erinnerungen aus Projekt A tauchen in Projekt B auf.
2. **Keine adressierbaren Fakten.** HOT/WARM sind Zeilenlisten; ein Fakt kann nicht einzeln geändert, verlinkt, belegt oder gelöscht werden.
3. **Kein Modell im Spiel.** Erinnerungen entstehen nur aus expliziten Signalen; eine Session, in der niemand `/memory record` tippt, hinterlässt nichts.
4. **Kein Verfall nach Nutzen.** Es gibt keine Nutzungszähler, also auch keine begründete Verdrängung.

v3 ergänzt genau diese vier Punkte und lässt v2 als schnelle, LLM-freie Leseschicht bestehen.

---

## 2. Speicherorte nach Lebensdauer

| Scope | Ort | Inhalt |
|---|---|---|
| Session | Prozess (`short_term.rs`) | Ring-Buffer des laufenden Gesprächs, kein I/O |
| Projekt | `<projekt>/.harw/memories/` | Fakten zu diesem Repo: Architektur, Konventionen, Entscheidungen, offene Punkte |
| Global | `~/.harw/profiles/<p>/memories/` | Fakten über den Nutzer, wiederkehrende Vorlieben, projektübergreifende Werkzeuge |

Beide persistenten Wurzeln haben dasselbe Layout (bestehendes `FileMemoryStore`-Layout bleibt erhalten, `facts/` und `MEMORY.md` kommen hinzu):

```text
<root>/
  MEMORY.md              ← Index: eine Zeile je Fakt, wird generiert
  facts/<slug>.md        ← ein Fakt je Datei (Frontmatter + Text)
  HOT.md  INDEX.md       ← v2, unverändert
  warm/<namespace>.md  cold/<namespace>.md
  signals/*.jsonl  state.json  workflow.json
  usage.json             ← Nutzungszähler je Fakt
```

Das Projekt-`.harw/memories` ist bewusst **im Repo** und damit versionierbar und teilbar. Es gewährt keine Rechte — Rechte (Allow-Regeln, Workdirs) liegen laut `harw-scopes-contract.md` §2 außerhalb des Repos.

---

## 3. Der Fakt

```markdown
---
name: tui-approval-arming
description: Warum der Freigabe-Dialog eine Arming-Verzögerung hat
type: decision            # fact | decision | preference | pitfall | reference
scope: project            # project | global
created: 2026-09-14T05:12:00Z
updated: 2026-09-14T05:12:00Z
confidence: 0.9           # 0.0–1.0
sources:                  # Belege, optional
  - session:01H…          # Transcript
  - file:harw-tui/src/app.rs#L2402
tags: [tui, approval]
---

Tastendrücke gelten erst nach 250 ms und nur bei sichtbarem Panel.

**Warum:** Ein Turn kann genau in dem Moment ein Panel öffnen, in dem der
Nutzer tippt — ohne Verzögerung wäre das eine unbeabsichtigte Freigabe.

Verwandt: [[tui-tool-cell]], [[permissions-scopes]]
```

Regeln:
- Ein Fakt ist **ein** Sachverhalt, höchstens ~15 Zeilen. Längeres gehört in `docs/`.
- `name` ist der Dateiname ohne `.md`, kebab-case, stabil — Umbenennen bricht `[[links]]`.
- `[[name]]` darf auf noch nicht existierende Fakten zeigen; das markiert eine Lücke.
- `type` steuert die Auswahl: `preference` und `decision` werden bevorzugt geladen, `reference` nur bei Stichworttreffern.
- Kein Geheimnis im Fakt. Vor dem Schreiben läuft die bestehende Redaction über Muster für Schlüssel und Token.

`MEMORY.md` ist ein generierter Index (`- [Titel](facts/<slug>.md) — description`), damit ein Mensch und ein kleines Modell die Menge überblicken, ohne alle Dateien zu lesen.

---

## 4. Lesepfad (Hot Path, ohne Modell)

`MemoryContextProvider` liefert pro Turn höchstens `memory_token_budget` (Default 1500 Token):

1. **Immer:** `MEMORY.md`-Index des Projekts, gekürzt auf 40 Zeilen.
2. **Immer:** alle Fakten mit `type = preference` (global und Projekt), sortiert nach `updated`.
3. **Nach Bedarf:** Fakten mit Stichworttreffern gegen die Nutzernachricht (bestehender `context_selector`, BM25-artig über `name`, `description`, `tags`, Text) — Projekt vor Global.
4. **Dann:** HOT.md wie in v2.

Jeder ausgelieferte Fakt erhöht `usage.json[slug].count` und setzt `last_used`. Das kostet einen gepufferten Schreibvorgang pro Turn, kein Lesen im Hot Path.

Kein Modellaufruf. Reihenfolge und Budget sind deterministisch und testbar.

---

## 5. Schreibpfad

### 5.1 Sofort (deterministisch)
- `/memory record <text> [--global]` schreibt direkt einen Fakt (Default: Projekt).
- Bestehende Signale (`correction`, `reflection`, `pattern_hint`) bleiben und fließen unverändert in HOT/WARM.

### 5.2 Extraktion nach der Session (Phase 1, Modell)
Ausgelöst beim Start einer neuen Session, im Hintergrund, nie im Hot Path — wie bei Codex, damit die laufende Session nichts bezahlt.

- Auswahl: abgeschlossene Transcripts des Projekts, älter als `idle_min_secs` (Default 300), jünger als `max_age_days` (Default 30), noch nicht extrahiert (Marker in `state.json`).
- Eingabe: gefilterte Turn-Items (Nutzertext, Assistenz-Fazit, Fehler, Datei- und Befehlsnamen), höchstens 30 KiB.
- Prompt verlangt strenge Struktur: `facts: [{name, description, type, body, confidence, sources}]`, höchstens 5 pro Session, „nichts, was aus dem Code oder git-Log ablesbar ist".
- Ergebnis wird redigiert, dann als **Kandidat** unter `facts/_incoming/<slug>.md` abgelegt.

### 5.3 Konsolidierung (Phase 2, Agent)
- Läuft, wenn `_incoming/` nicht leer ist, unter einem Lock (eine Konsolidierung je Wurzel).
- Ein eigener Agent (`memory-steward`, Werkzeuge nur lesend plus Schreiben unterhalb der Memory-Wurzel, kein Netz, keine Freigaben) bekommt: Index, betroffene bestehende Fakten, alle Kandidaten, den Diff seit der letzten Baseline.
- Auftrag: zusammenführen statt anhäufen — Duplikate verschmelzen, Widersprüche über `contradiction_index` markieren und auflösen, veraltete Fakten senken (`confidence`) oder löschen, `MEMORY.md` neu schreiben.
- Danach: git-Baseline der Memory-Wurzel zurücksetzen (Projekt: normaler Repo-Commit-Kandidat, kein automatischer Commit; Global: eigenes `.git` in der Wurzel wie bei Codex).

### 5.4 Verdrängung
Beim Heartbeat (`maintain()`):
- `last_used` älter als `max_unused_days` (Default 90) und `usage_count == 0` → `confidence *= 0.5`; unter 0.2 → nach `cold/` verschoben.
- Ein Fakt, dessen `sources` alle verschwunden sind (Datei gelöscht, Transcript weg), wird markiert, nicht automatisch gelöscht.

---

## 6. Bedienung

| Befehl | Wirkung |
|---|---|
| `/memory` | Index mit Anzahl je Scope und Typ |
| `/memory record <text> [--global]` | Fakt schreiben |
| `/memory recall <stichwort>` | Suche über beide Scopes, zeigt Herkunft |
| `/memory forget <name>` | Fakt löschen (Projekt oder Global), mit Rückfrage |
| `/memory consolidate` | Phase 2 sofort anstoßen |
| `/memory stats` | Nutzung, Verfall, Kandidaten |

Config `[memory]`: `enabled`, `project_enabled`, `extraction = true|false`, `extraction_model`, `token_budget`, `max_unused_days`, `max_facts_per_session`.

---

## 7. Invarianten

1. Kein Modellaufruf im Lesepfad.
2. Ein Fakt ist eine Datei; die Datei ist die Wahrheit. Index und Nutzungszähler sind jederzeit neu ableitbar.
3. Projekt-Erinnerungen verlassen nie das Projekt; globale enthalten keine Projektgeheimnisse (Redaction plus Pfadprüfung beim Schreiben).
4. Extraktion und Konsolidierung sind idempotent und nehmen sich über Marker und Locks nichts weg.
5. Jeder automatisch erzeugte Fakt trägt seine Quelle; ohne Quelle kein automatischer Fakt.
6. Löschen ist immer möglich und vollständig — auch aus dem Index und den Zählern.

---

## 8. Umsetzung in Scheiben

| Scheibe | Inhalt |
|---|---|
| M1 | `harw-memory/src/facts.rs`: Fakt-Typ, Frontmatter, Laden/Schreiben/Löschen, Slug, Redaction, `MEMORY.md`-Generierung, Nutzungszähler |
| M2 | Zwei Wurzeln (Projekt + Global) im `MemoryContextProvider`, Auswahlreihenfolge aus §4, Budget, Zählerpflege |
| M3 | `/memory`-Unterbefehle aus §6 |
| M4 | Phase 1: Extraktion über den One-Shot-Modellaufruf (`harw-runtime/src/one_shot.rs`), Marker, Redaction, `_incoming/` |
| M5 | Phase 2: Rolle `memory-steward` plus Konsolidierungslauf, git-Baseline |
| M6 | Verfall im Heartbeat, `/memory stats` |
