# Bericht zur Harwness-/Codex-Session vom 17. September 2026

## Kurzfassung

Der Start von `harw` im Projekt `veyra` scheiterte an einem Formatkonflikt: Drei Harwness-Agent-Definitionen im DSL-Format wurden vom Setup-Skript als alte `agent.toml`-Konfiguration installiert. Dadurch wurde die Datei mit dem falschen Parser gelesen und der Eintrag `schema` als unbekannt gemeldet.

Die Konfiguration wurde auf den vorgesehenen Profilpfad mit `definition.toml` umgestellt, nicht auf `agent.toml`. Zusätzlich wurden nicht auflösbare `extends`-Verweise entfernt, ohne die eigentlichen Rollen-, Tool-, Spawn- und Budgetgrenzen aufzugeben.

## Ausgangsfehler

Beim Start erschien sinngemäß:

```text
runtime config error: failed to parse TOML
unknown field `schema`
expected ... `name`, `role`, `description`, `system_file`, ...
```

Das war kein inhaltlicher TOML-Syntaxfehler. Die Datei war eine gültige Harwness-Agent-DSL-Datei mit:

```toml
schema = "harwness.agent/v1"
```

Sie wurde aber an einem Ort abgelegt, an dem Harw das ältere Rollenformat erwartete.

## Was untersucht wurde

Es wurden die lokale Harw-Konfiguration, die Profil-Agenten, das Projekt-Setup-Skript, vorhandene Exporte/Dokumentation und die installierte Runtime geprüft. Dabei wurden insbesondere folgende Punkte geklärt:

- `py-explorer`, `py-analyst` und `py-evaluator` sind DSL-Definitionen, keine Legacy-`agent.toml`-Dateien.
- Der korrekte Profilpfad ist jeweils `profiles/default/agents/<name>/definition.toml`.
- `child-orchestrator-base@1` war in der installierten Registry nicht verfügbar.
- Auch der Versuch mit `worker-base@1` ließ sich für diese benutzerdefinierten Definitionen nicht auflösen.
- Die vorhandene gültige Profildefinition zeigte, dass diese Definitionen ihre Rollen- und Grenzen direkt enthalten können.

## Durchgeführte Änderungen

### Profil-Scope

Die drei Agenten liegen jetzt als:

```text
/home/mm29942/.harw/profiles/default/agents/py-explorer/definition.toml
/home/mm29942/.harw/profiles/default/agents/py-analyst/definition.toml
/home/mm29942/.harw/profiles/default/agents/py-evaluator/definition.toml
```

Die Dateien haben restriktive Rechte `0600`. Die falsch einsortierten DSL-Dateien unter `agent.toml` wurden entfernt. Die aktive UIA-Definition wurde ausdrücklich nicht verändert; ihre Aktivierung bleibt eine Nutzerentscheidung.

### Projektquellen

Die drei Quelldateien im Projekt wurden angepasst:

```text
/srv/dev-shared/projects/rust/veyra/py-explorer.toml
/srv/dev-shared/projects/rust/veyra/py-analyst.toml
/srv/dev-shared/projects/rust/veyra/py-evaluator.toml
```

Die nicht auflösbaren `extends`-Zeilen wurden entfernt. Erhalten blieben unter anderem:

- die Rollen `worker` bzw. `child-orchestrator`
- die Spezialisierungen
- die Read-only-Tool-Whitelist
- die Verbote für `fs.write` und `shell.exec`
- die Spawn-Tiefe und `max_parallel = 5`
- die Token-, Tool-Call- und Zeitbudgets
- der Rückgabevertrag `harwness.return.execution-summary@1`

### Setup-Skript

In `agent-setup.sh` wurde der dauerhafte Installationsweg korrigiert:

- Ziel ist nun `definition.toml` statt `agent.toml`.
- Dry-run-Ausgaben und Hinweise nennen den richtigen Pfad.
- Ein vorhandenes `agent.toml` wird nur entfernt, wenn es anhand von `schema = ...` eindeutig als falsch einsortierte DSL-Datei erkennbar ist.
- Eine gültige Legacy-Konfiguration wird dadurch nicht pauschal überschrieben.

## Verifikation

Die wesentlichen Prüfungen waren erfolgreich:

- `bash -n agent-setup.sh` war erfolgreich.
- `harw doctor` meldete `Harwness configuration is valid`.
- Die Runtime erkannte 19 Agenten, 10 Provider, 1286 Modelle, 8 Skills und 1 Channel.
- Die Profil-Definitionen wurden mit den Projektquellen verglichen und stimmten überein.
- `harw classify` konnte die Runtime wieder montieren.

Es gab zusätzlich Warnungen, dass der Projektpfad `/srv/dev-shared/projects/rust/veyra` in dieser Ausführungsumgebung schreibgeschützt war. Diese Warnungen betrafen das Anlegen projektlokaler `.harw`- und Memory-Verzeichnisse, nicht die erfolgreiche Validierung der globalen/profilbezogenen Konfiguration.

## Wobei ich geholfen habe

Ich habe:

1. den Parserfehler auf den falschen Ablageort und nicht auf einen TOML-Syntaxfehler zurückgeführt;
2. das erwartete Harwness-Dateilayout aus Runtime, Profil und vorhandenen Definitionen abgeleitet;
3. die drei Agent-Definitionen und das Setup-Skript auf den richtigen Pfad umgestellt;
4. die nicht verfügbaren Vererbungsreferenzen entfernt, ohne die Sicherheits- und Spawn-Grenzen zu verlieren;
5. die Löschlogik im Skript nachträglich auf eindeutig erkennbare DSL-Dateien eingeschränkt;
6. die Konfiguration anschließend mit `harw doctor`, Syntaxprüfung und Datei-Vergleich verifiziert.

Die früheren Telegram-Versuche in den Transkripten wurden abgebrochen und gehören nicht zur eigentlichen Harw-Agent-Reparatur.

## Git- und Abschlussstatus

Die Änderungen sind in den Dateien vorhanden. Nach dem geprüften Git-Status wurde jedoch kein neuer Git-Commit nachgewiesen. Der letzte sichtbare Commit war weiterhin:

```text
4940cd9 Update README.md
```

Das Projekt enthält außerdem zahlreiche bereits vorhandene oder parallele uncommittete Änderungen. Deshalb wurde nichts pauschal bereinigt oder committed.

## Session-Transkripte

In diesem Ordner liegen sieben `rollout-*.jsonl`-Dateien. Die Dateien enthalten in dieser Umgebung die Session-Metadaten, Benutzer- und Assistant-Nachrichten, Tool-Aufrufe und Tool-Ausgaben. Die `codex_exec`-Sessions sind an `originator: "codex_exec"` und am ursprünglichen Arbeitsverzeichnis `/home/mm29942/.harw` erkennbar; die aktuelle TUI-Session ist separat als `codex-tui` verzeichnet.

Die JSONL-Dateien sind Rohprotokolle und nicht als lesbarer Bericht formatiert. Außerdem können darin sensible Pfade, Befehle oder Konfigurationsausschnitte vorkommen. Für diese Session gilt daher: Ja, der Verlauf ist hier lokal auffindbar; eine allgemeine Garantie für jeden `codex exec`-Aufruf lässt sich aus diesem einzelnen lokalen Befund jedoch nicht ableiten. Die von mir gefundene offizielle OpenAI-Dokumentation beschreibt API-Sessions und Artefakte, dokumentiert aber nicht den lokalen Ablageort dieses CLI-Rohprotokolls.

Erstellt am 17.09.2026 aus den lokalen Rollout-Dateien in diesem Ordner.
