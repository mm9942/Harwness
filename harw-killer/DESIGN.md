# killer — Design und Umsetzung

Auftrag: Bash-Tool durch ein nutzbares Rust-Projekt mit Clap, präziser
Prozessauswahl und bewusst doppeltem SIGKILL ersetzen.

## Entscheidungen

- Linux-only, `/proc` + Kernel-pidfds, keine Shell-basierte Suche.
- Mehrere exakte ausführbare Dateinamen mit -p/--process (Fallback `comm`);
  kombinierbar mit mehreren expliziten --pid-Werten. Kein Regex.
- PID 1, eigener Prozess und Vorfahren bleiben ausgeschlossen.
- Geöffnete pidfds binden die Auswahl an die Prozessinstanz. Vor/nach Öffnen
  wird die Startzeit geprüft; zusätzlich wird geprüft, ob das pidfd bereits
  Exit meldet, bevor die Auswahl akzeptiert wird.
- Vorschau, Bestätigung bzw. `--yes`, `--dry-run`, JSON.
- KILL an die eigenen Ziele, gemeinsamer Timeout, nochmals KILL an Überlebende,
  begrenzte Nachkontrolle. Zombies zählen als beendet.
- Fremde Ziele: `sudo` führt einen internen Helfer aus. Dieser dupliziert mit
  `pidfd_getfd` das bereits gehaltene pidfd des Elternprozesses. Keine erneute
  Suche, keine Signalzustellung über nackte numerische Ziel-PIDs.
- Falls Kernel/Ptrace-/sudo-Policy diesen Weg blockieren, Fehler statt
  unsicherem Fallback. Ein bewusst als root gestarteter Aufruf braucht keinen Helfer.
- Keine Prozessgruppen, Nachfahrenauswahl oder dauerhaftes Nachverfolgen von
  neu gestarteten Prozessen in v0.1. Ein pidfd bindet die Prozessinstanz, nicht
  ihr Programm: ein späteres `exec` bleibt dieselbe Instanz.

## Modulgrenzen und Umsetzung

1. `cli.rs`: Auswahl, Optionen, Validierung.
2. `process.rs`: Procfs-Snapshot, Startzeit, Vorfahren, Auswahl.
3. `pidfd.rs`: sichere rustix-Grenze; OwnedFd und poll.
4. `engine.rs`: KILL, Timeout, KILL und Ergebniszustände.
5. `main.rs`: Vorschau, Zustimmung, Rechtewechsel, Ausgabe/Exitcodes.
6. Integrationstests mit isolierten eigenen Kindern; CI, README, Paket.

Kein setuid-Binary und kein installierter privilegierter Dienst. `sudo` startet
gewöhnlichen ausführbaren Code als root: nur ein selbst gebautes/vertrautes
Binary verwenden, keine pauschale NOPASSWD-Regel auf schreibbare Dateien setzen.

## Verbindliche Nutzerkorrektur

SIGKILL auch als erstes Signal und die Wiederholung sind ausdrücklich gewollt.
Die anfangs angenommene SIGTERM-Phase ist verworfen. Kein --no-kill/--no-retry-
Schalter: der Standardalgorithmus führt beide KILL-Phasen aus, soweit Ziele
noch leben. Kein zweites Signal an eine wiederverwendete PID.

## Generische CLI (Nutzerkorrektur)

-p/--process nimmt mehrere exakte ausführbare Namen entgegen; --pid mehrere
positive PIDs. Beide Selektoren bilden eine Vereinigung, --uid filtert diese.
Keine Rust-spezifischen Flags, Regex, Teilstrings oder Positionsargumente.

## Integration in Harwness

Das Projekt lebt als Workspace-Crate `harw-killer` weiter; Semantik unverändert.

- `src/lib.rs`: gesamte CLI-Orchestrierung (`run_cli`, `HelperInvocation`).
  `main.rs` ist nur noch ein dünner Einstieg für das `killer`-Binary.
- `HelperInvocation::Standalone` startet den sudo-Helfer wie bisher als
  `<exe> --helper …`; `HelperInvocation::Subcommand(["kill"])` als
  `<exe> kill --helper …`, damit `harw kill …` denselben Weg nimmt.
- `src/api.rs`: programmatische Schnittstelle für Agenten (`preview`,
  `kill_own`). Sie verwendet dieselbe Auswahl und dieselbe KILL/Warte/KILL-
  Engine, startet aber **nie** sudo: fremde Ziele erhalten ein Fehlerergebnis.
- `harw kill` (harw-cli) reicht alle Argumente unverändert an `run_cli` durch.
- Agent-Werkzeuge `process.list` (nur Vorschau) und `process.kill`
  (freigabepflichtig) liegen im Crate `harw-tool-process`.
