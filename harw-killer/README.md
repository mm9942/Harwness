# killer

Ein Linux-CLI in Rust mit Clap: Prozesse präzise auswählen, Vorschau ansehen,
**sofort SIGKILL senden**, auf Exit warten und noch laufenden Zielinstanzen
**erneut SIGKILL** senden. Genau diese doppelte KILL-Logik ist beabsichtigt.

Beide Signalversuche verwenden denselben **pidfd**. Es gibt keine SIGTERM-Phase.

## Voraussetzungen und Installation

- Linux, gemountetes `/proc`, Kernel **5.3+** für die normale pidfd-Nutzung.
- Kernel **5.6+**, `sudo` und passende Ptrace-Berechtigung für den automatischen
  Fremdbenutzer-Helfer. Seccomp, Yama, Container-Capabilities oder sudo-Regeln
  können diesen Weg blockieren. Dann gibt es einen Fehler, keinen PID-Fallback.
- Rust mit Cargo, rustfmt, Clippy; Make. Projektedition 2024, deklarierte MSRV 1.85.
  Den tatsächlich geprüften Toolchain-Stand dokumentiert `VALIDATION.md`.
- Python 3 nur für die ausdrücklich aktivierten Systemtests.

```sh
unzip killer-rust-project.zip
cd killer
make clippy-tests
make test
make test-system
make install                 # ~/.local/bin/killer
```

`~/.local/bin` muss in deinem PATH liegen. `make install PREFIX=/usr/local` ist
für eine systemweite, bewusst mit passenden Schreibrechten ausgeführte Installation
möglich. Keine automatische Installation oder Änderung an sudoers.
`Cargo.lock` wird mitgeliefert; alle Make-Builds verwenden `--locked`.

## Beispiele

```sh
killer -p rustc rust-analyzer cargo -n     # Vorschau für mehrere Namen
killer --process node python3             # Vorschau und Bestätigung
killer -p rustc rust-analyzer -y           # direkt ausführen
killer -p cargo -y -t 3                    # 3 Sekunden bis zweitem KILL
killer --pid 12345 12346 -y                # mehrere explizite PIDs
killer -p cargo --pid 12345 -n             # Namen ODER PIDs
killer -p node -p python3 -n               # -p darf wiederholt werden
killer -p cargo --uid 1000 -n --json       # UID-Filter für alle Treffer
killer -p cargo --no-sudo -y
```

`-p/--process` akzeptiert einen oder mehrere **exakte ausführbare Dateinamen**.
`--pid` akzeptiert eine oder mehrere positive PIDs. Beide sind kombinierbar:
ein Prozess muss mindestens einen Namen oder eine PID treffen; `--uid` schränkt
anschließend die gesamte Auswahl ein. Mehrfachtreffer werden dedupliziert.
Optionen beenden die jeweilige Werteliste. Namen mit Leerzeichen quotieren.
`-n` ist Vorschau, `-y` überspringt die Bestätigung, `-t` setzt die Wartezeit.
Ohne Selektor zeigt Killer einen Argumentfehler, niemals eine Auswahl aller Prozesse.

Keine speziellen Compiler-Flags, Regex, Globs oder Positionsargumente. Ein Name
wie `rustc` trifft `rustc-wrapper` nicht. `-p` bedeutet nun **process**, nicht PID;
für PIDs ausdrücklich `--pid` verwenden.

Verglichen wird der Basename von `/proc/PID/exe`. Ist dieser nicht lesbar, wird
`comm` verwendet (normalerweise auf 15 Bytes begrenzt). Scripts laufen häufig
unter dem Namen ihres Interpreters; verwende dessen Namen oder eine explizite PID.

Die Tabelle zeigt PID, PPID, effektive UID, Name und Kommandozeile. Pfad und
Startzeit in Kernel-Ticks stehen zusätzlich im JSON-Snapshot. Kontrollzeichen
werden für Terminalausgabe escaped. Kommandozeilen können sensible Argumente
enthalten: entsprechende Vorschauen/JSON nicht unbedacht weitergeben.

## Verhalten und Grenzen

1. Einmaliger Snapshot sichtbarer Prozesse. Init/PID 1, killer selbst und alle
   ermittelbaren Vorfahren (z.B. die aufrufende Shell) bleiben geschützt.
2. Auswahlbedingungen und Startzeit werden vor/nach `pidfd_open` verglichen;
   bereits beendete Handles werden verworfen. Kernelhandles bleiben ab dann offen.
3. Vorschau und Rückfrage. In Pipes/Skripten ist `--yes` nötig. JSON-Terminierung
   verlangt ebenfalls `--yes`; JSON-Vorschau benötigt keine Bestätigung.
4. Eigene Ziele erhalten zuerst alle KILL; danach folgt eine gemeinsame Wartephase
   (`--timeout`, Standard 5 Sekunden). Nur Überlebende erhalten erneut KILL,
   gefolgt von `--kill-wait` (Standard 2 Sekunden). Bereits beendete Ziele werden
   nicht erneut signalisiert. Sind alle beendet, endet die Wartephase frühzeitig.
5. Fremde Ziele werden anschließend als eigene Gruppe über sudo behandelt.
   Damit gilt der Timeout **je Gruppe**, nicht als globale Obergrenze inklusive
   Passwortabfrage. Ein bereits als root gestarteter Aufruf benötigt keinen Helfer.
6. Der root-Helfer übernimmt die offenen Zielhandles mit `pidfd_getfd` aus dem
   wartenden Elternprozess. Er sucht keine Zielprozesse erneut anhand von Namen/PIDs.
   Er dupliziert alle übergebenen Handles vor dem ersten Signal.

Kein Zombie wird als noch laufender Prozess behandelt. KILL-Erfolg bedeutet nicht,
 dass ein Prozess sofort aus einer nicht unterbrechbaren Kerneloperation zurückkehrt;
`survived` meldet deshalb nur, dass innerhalb der Nachwartezeit kein Exit sichtbar war.
`killed` meldet beobachteten Exit nach dem ersten Versuch, `killed_after_retry`
nach dem zweiten Versuch. Ein Exit zwischen zwei Phasen kann nicht kausal einem
Signal bewiesen werden. Die Wiederholung ist eine bewusste zweite Zustellung;
sie macht SIGKILL nicht stärker und überspringt keine ununterbrechbare Kernelarbeit.

Ein pidfd hält eine Prozessinstanz fest, nicht ihren aktuellen Programmcode.
Ein späteres `exec` oder ein Credential-Wechsel derselben Instanz kann stattfinden.
Keine rekursive Kindprozessauswahl, kein Prozessgruppen-Kill, keine automatische
Wiederholung für neu gestartete Prozesse. `/proc`-Sichtbarkeit ist durch Namespace,
hidepid und Rechte begrenzt. Windows und macOS sind hier nicht implementiert.

`sudo` führt das installierte killer-Binary als root aus. Verwende ein vertrautes
Binary; keine pauschale NOPASSWD-Regel auf eine von anderen beschreibbare Datei.
Blockierte `pidfd_getfd`-Berechtigungen bleiben ein klar gemeldeter Fehler.

## Ausgabe und Exitcodes

JSON-Ausgabe (`--json`) bleibt auf stdout; strukturierte Tracing-Diagnose auf stderr.
Das Report-Schema enthält `schema_version`, `dry_run`, `targets` und `results`.
Ergebnisse: `killed`, `killed_after_retry`, `already_exited`, `survived`, `error`.
Fatale Fehler vor Report-Erzeugung erscheinen auf stderr; es wird dann kein
vollständiger JSON-Report versprochen.

| Code | Bedeutung |
| --- | --- |
| 0 | Vorschau erfolgreich oder alle ausgewählten Prozesse beendet |
| 1 | Fehler oder mindestens ein nicht als beendet beobachtetes Ziel |
| 2 | Keine zulässigen Treffer; auch Clap verwendet 2 für ungültige Argumente |
| 3 | Interaktiv abgebrochen |

## Projektstruktur

| Datei | Verantwortung |
| --- | --- |
| `src/cli.rs` | Clap und Argumentvalidierung |
| `src/process.rs` | Procfs, Matching, Schutz und Snapshot |
| `src/pidfd.rs` | Sichere rustix-Handles, Signale, Exit-Beobachtung |
| `src/engine.rs` | KILL/Wartezeit/KILL; austauschbare Testgrenze |
| `src/privilege.rs` | sudo-Protokoll und austauschbarer CommandRunner |
| `src/types.rs` | Metadaten und typisierte Ergebnisse |
| `src/error.rs` | Handgeschriebene Fehlerdomäne und Ursachenketten |
| `src/typestate/` | Ausgewählter → bestätigter Plan |
| `src/main.rs` | CLI-Orchestrierung, Datenansicht und Reports |

Details: `DESIGN.md`, `RULES_APPLIED.md`, `TESTING.md`, `VALIDATION.md` und
`DEPENDENCIES.md`. Kein Lizenzmodell wird ohne deine Entscheidung vorausgesetzt;
`publish = false` verhindert versehentliches Veröffentlichen auf crates.io.
