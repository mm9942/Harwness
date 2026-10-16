# H20 — Step-/TODO-Listenbibliothek: Contract-Draft

Status: Completed (Implementierung h21 abgeschlossen; harw-step-list Crate im Workspace, Tests grün).

## Befundene Fakten (mit Belegen)

1. **WorkDriver-Ops enqueue/status/stop**: Der WorkDriver bietet die Operationen enqueue, status und stop. Belege: `harw-ops/src/work_driver.rs:874`, `:1142`, `:1311`.
2. **Zustandsloses `WorkDriver::decide`**: Entscheidungen sind zustandslos implementiert — kein versteckter Zustand über Runden hinweg. Beleg: `harw-ops/src/work_driver.rs` (Kontext zu :874/:1142/:1311).
3. **Single-Attempt / kein Auto-Restart**: Der WorkDriver führt pro Element einen Versuch aus; kein automatischer Neustart. Beleg: `docs/guides/work-driver.md:104-119`.
4. **run_rounds / max_rounds**: Rundensteuerung mit Obergrenze `max_rounds`. Beleg: `job_worker_work_driver.rs:767-811`.
5. **ScreenLayout chat/agents/status/composer**: Das TUI-Layout definiert die Fenster chat, agents, status, composer. Beleg: `tui-layout/placement.rs:88-102`. Drei weitere, oben konfigurierbare Fenster sind **nicht belegt** (keine Annahme darüber).

## Vertragsspezifikation

### Listenmodell (Bibliothek, getrennt von TUI)
- Geordnete Elemente mit stabilen IDs (IDs überleiben Umbenennen/Neuordnen).
- Status pro Element (offen / in Bearbeitung / erledigt / blockiert) mit belegten Statusübergängen.
- Fortschritt nur, wo belegt (Beleg der abgeschlossenen Arbeit, z. B. WorkDriver-Ergebnis); keine Fortschrittsheuchelei ohne Quelle.
- Persistenz über Sessions: Liste ist serialisierbar und wird beim Neustart wiederhergestellt (IDs und Reihenfolge stabil).
- Nebenläufige Änderungen: definierte Merge-/Konfliktregeln; keine verlorenen Updates bei parallelen Editoren (Bibliothek entscheidet deterministisch, dokumentiert).

### Bibliotheks-API getrennt von TUI
- Die Listenbibliothek ist eigenständig und ohne TUI nutzbar; das TUI (chat/agents/status/composer-Layout, Befund 5) ist nur ein Konsument. Das Layout wird nicht Teil der Bibliotheks-API.

### Kontrollierte WorkDriver-Verbindung
- Verbindung nur über enqueue/status/stop (Befund 1); `decide` bleibt zustandslos (Befund 2).
- Grenzen: Runden (`run_rounds`/`max_rounds`, Befund 4), Token- und Walltime-Obergrenzen (Vertragszusatz, siehe offene Punkte), Stop/Crash/Resume-Verhalten:
  - Stop: geordnetes Anhalten, Zustand bleibt lesbar (status).
  - Crash: Liste bleibt persistent; Wiederaufnahme nach Resume am letzten stabilen Element-Zustand.
  - Resume: kein Auto-Restart, kein zweiter Versuch (Single-Attempt, Befund 3) — Resume startet bei noch offenen Elementen, nicht bei abgeschlossenen.
- **Verpflichtende Freigabe pro enqueue**: Jede Anfrage an enqueue erfordert eine explizite Freigabe des Aufrufers (Vertragszusatz; Implementierungsstand offen).
- **Keine automatische Ausführung beliebiger Aktionen**: Die Bibliothek triggert nie selbstständig Aktionen; jeder Ausführungsschritt geht auf einen autorisierten enqueue zurück.

## Offene Punkte
- **Enqueue-Freigabepflicht**: noch nicht vollständig verifiziert, ob im Code eine Freigabe pro enqueue erzwungen wird.
- **Token-/Walltime-Grenzen**: im Detail noch nicht verifiziert.
- Drei obere, konfigurierbare TUI-Fenster: unbelegt, nicht Teil dieses Vertrags.

## Tests
- Stabile IDs bei Reihenfolgeänderung/Rename; Persistenz-Roundtrip über Sessiongrenze.
- Statusübergänge (inkl. unzulässiger Übergänge → Fehler).
- Parallelität: zwei gleichzeitige Änderungen → deterministisches Ergebnis, kein verlorenes Update.
- WorkDriver-Integration: enqueue/status/stop; `max_rounds` wird respektiert; Single-Attempt (kein zweiter Lauf nach Fehlschlag); Stop → Zustand lesbar; Crash + Resume → offene Elemente bleiben, abgeschlossene nicht erneut.
- Enqueue ohne Freigabe → Abweisung (sobald Mechanismus verifiziert/gebaut).
- Bibliothek funktioniert ohne TUI (Unit-Tests ohne Layout-Abhängigkeit).
