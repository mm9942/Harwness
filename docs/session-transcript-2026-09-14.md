# Sitzungsprotokoll – UIA, TUI-Eingabe, Arbeitswurzeln und Shell-Sandbox

**Datum:** 2026-09-14  
**Arbeitsverzeichnis:** `/home/mm29942/Harwness`

## Ziel und Plan

Die Sitzung bearbeitete den Plan `uia-and-external-access-plan`. Im Mittelpunkt standen:

1. eine zwingende, fail-closed konfigurierte UIA (`role = "user-interface"`),
2. nicht blockierende TUI-Eingabe mit FIFO-Verarbeitung während laufender Turns,
3. zustimmungsgebundene zusätzliche Arbeitswurzeln,
4. ein Bootstrap für eine fehlende aktive UIA beim lokalen Chat-Start.

Der Plan enthält außerdem noch offene Arbeit für Provider-/Modellverwaltung. Während der Sitzung wurde zusätzlich die Anforderung genannt, die Produkt-Sandbox für Shell-Kommandos grundsätzlich zu lockern, wobei explizite Sperrausnahmen erhalten bleiben sollen. Diese Anforderung wurde zunächst kartiert, aber noch nicht implementiert, weil anschließend um eine Neufassung dieses Protokolls gebeten wurde.

## Vorgefundener Arbeitsstand

Vor und während der Arbeit existierten lokale, nicht eingecheckte Änderungen in mehreren Crates. Sie wurden nicht verworfen oder überschrieben.

Bereits vorhandene Änderungen betrafen insbesondere:

- `harw-tui/src/app.rs`
- `harw-tui/src/command_popup.rs`
- `harw-config/src/harness_config.rs`
- `harw-config/src/discovery.rs`
- `harw-runtime/src/assembly.rs`

Die TUI-Änderungen umfassen unter anderem:

- einen auch während laufender Modell-, Tool- oder Freigabephasen editierbaren Composer,
- FIFO-Warteschlange für in Busy-/Freigabephasen abgesendete Eingaben,
- kooperativen Turn-Abbruch über `Ctrl+C`,
- dynamische Command-Popup-Befüllung aus Runtime-Command-Spezifikationen.

## UIA-Konfigurations- und Runtime-Vertrag

### Konfiguration

`HarnessConfig` enthält nun das optionale Feld:

```toml
active_uia_definition = "namespace.agent.name@1"
```

Beim Layer-Merge wird es wie `default_provider` und `default_model` vererbt: Ein späterer Layer übernimmt den früheren Wert, sofern er keinen eigenen UIA-Wert setzt.

### Validierung

`ResolvedConfig::validate` validiert `active_uia_definition` fail-closed:

1. Die referenzierte gesenkte Agentendefinition muss vorhanden sein.
2. Ihre Rolle muss exakt `AgentRoleId::UserInterface` (`user-interface`) sein.

Unbekannte IDs und Definitionen mit falscher Rolle werden abgelehnt.

### Runtime-Montage

Für `EntryKind::Tui` und `EntryKind::OneShot` gilt:

- Ohne `harness.active_uia_definition` montiert die Runtime nicht.
- Die UIA-Auflösung schlägt bei unbekannter oder falsch gerollter Definition fehl.
- Die UIA bestimmt die Root-Tool-Aktivierung.
- Eine frühere `active_agent`-Auswahl kann die UIA in UI-Einstiegen weder ersetzen noch ihre Tool-Decke überlagern.
- Die organisatorische Root-Rolle im Spawn-Kontext wird aus der UIA-IR abgeleitet.

Nicht-UI-Einstiege behalten die bisherige `active_agent`-Behandlung und benötigen keine UIA.

### Spawn-Abgrenzung

Die Rollenmatrix erlaubt einer UIA ausschließlich das Admitten von `RootOrchestrator`-Kindern. Deshalb registriert der initiale UIA-Spawner eingebaute Ziele als `RootOrchestrator`; ein unmittelbarer Worker-Spawn durch die UIA ist nicht möglich.

## UIA-Bootstrap beim lokalen Chat-Start

Neu hinzugefügt wurde:

- `harw-cli/src/uia_bootstrap.rs`
- Einbindung in `harw-cli/src/main.rs`
- Aufruf vor dem Onboarding und der Runtime-Montage in `harw-cli/src/chat.rs`

Der Bootstrap arbeitet vor der Runtime-Montage und nie über einen Modellaufruf.

### Verhalten

1. **Explizite Auswahl vorhanden**  
   `active_uia_definition` wird nicht verändert. Die übliche Config- und Runtime-Validierung bleibt zuständig.

2. **Genau eine entdeckte UIA**  
   Ihre kanonische Definition-ID wird als `active_uia_definition` in der Profil-`config.toml` persistiert.

3. **Mehrere entdeckte UIAs**  
   In einem interaktiven Terminal erscheint eine nummerierte Auswahl. Nur die explizite Auswahl wird gespeichert.

4. **Keine entdeckte UIA**  
   Der CLI-Start zeigt einen konkreten lokalen Entwurf:

   ```text
   id: harwness.agent.default-terminal-ui@1
   role: user-interface
   specialization: terminal-ui
   Beschreibung: Lokale, sichere Standardoberfläche für Harwness.
   ```

   Erst nach einer klaren Bestätigung (`ja`, `j`, `yes` oder `y`) wird die Datei
   `agents/default-terminal-ui/definition.toml` im aktiven Profil erzeugt und ihre ID als Standard gespeichert.

5. **Kein interaktives Terminal**  
   Fehlt eine UIA und stdin/stderr sind keine Terminals, schlägt der Start verständlich fehl. Es wird weder eine Definition erstellt noch eine UIA stillschweigend aktiviert.

Nach einer Persistenz lädt `chat.rs` die Konfiguration erneut, damit die Runtime genau die gespeicherte und validierte Konfiguration montiert.

## Shell-Sandbox: ermittelte Architektur

Die nachträglich gewünschte Lockerung betrifft die **Produkt-Sandbox** für `shell.exec`, nicht die isolierte Sandbox, in der dieser Coding-Agent läuft. Die lokale Agentensandbox kann nicht durch Repository-Code umkonfiguriert werden; dort waren `cargo` und `rustfmt` nicht im `PATH` verfügbar.

Für die Produkt-Sandbox wurden folgende Zuständigkeiten ermittelt:

- `harw-tool-shell/src/exec.rs`
  - führt `shell.exec` aus,
  - verlangt `Permission::ExecuteProcess`,
  - startet Befehle über Bubblewrap und ressourcenbegrenzte Prozesse.
- `harw-sandbox/src/bwrap.rs`
  - baut die Bubblewrap-Ausführung,
  - isoliert Netzwerk und Host-Bindungen,
  - ist nicht der Regelentscheidungsort für Command-Allow-/Deny-Patterns.
- `harw-config/src/permissions_toml.rs`
  - konfiguriert `[permissions]`, `[[permissions.allow]]` und `[[permissions.deny]]`,
  - eine Regel enthält `tool` sowie optional `pattern`.
- `harw-runtime/src/assembly.rs`
  - baut über `seed_allow_rule_set` die gemeinsame `AllowRuleSet` aus globalen und Projektregeln,
  - übernimmt Deny-Regeln ausdrücklich immer; Deny soll scope-übergreifend gewinnen.
- `harw-runtime/src/approval.rs` und `harw-extension-api::allow_rules`
  - sind der wahrscheinliche Policy-/Freigabeentscheidungspfad vor der Tool-Ausführung.

### Noch offene Designentscheidung

Die gewünschte Semantik lautet sinngemäß:

> Alle Bash-Kommandos sind standardmäßig zulässig; nur explizit konfigurierte Ausnahmen werden blockiert.

Das ist eine substanzielle Änderung gegenüber der vorhandenen Freigabekette und darf nicht einfach durch Entfernen von `ExecuteProcess`-Prüfungen oder durch Abschalten von Bubblewrap erfolgen. Der richtige Änderungspunkt ist die Approval-/Regelentscheidung für `shell.exec`: fehlende passende Regel müsste dort als **Allow** statt als Ask/Deny behandelt werden, während jede passende Deny-Regel weiterhin Vorrang behält. Die Prozess-Sandbox, Ressourcenlimits, Workspace-Bindung und Netzisolation müssen bestehen bleiben.

Vor einer Umsetzung sind gezielte Tests erforderlich für:

- ungematchte `shell.exec`-Aufrufe sind erlaubt,
- passende `[[permissions.deny]]`-Regeln blockieren weiterhin,
- Deny schlägt eine gleichzeitig passende Allow-Regel,
- andere Tools behalten ihre bisherige Freigabe-Semantik,
- `Permission::ExecuteProcess` bleibt als Capability-Grenze wirksam.

## Geänderte Dateien dieser Sitzung

Direkt ergänzt bzw. verändert:

- `harw-cli/src/uia_bootstrap.rs` (neu)
- `harw-cli/src/chat.rs`
- `harw-cli/src/main.rs`

In der Arbeitskopie außerdem vorhandene, nicht von dieser Sitzung verworfene Änderungen:

- `harw-config/src/discovery.rs`
- `harw-config/src/harness_config.rs`
- `harw-runtime/src/assembly.rs`
- `harw-tui/src/app.rs`
- `harw-tui/src/command_popup.rs`

## Validierung

Erfolgreich ausgeführt:

```sh
git diff --check
```

Der Check meldete keine Whitespace-Fehler.

Nicht ausführbar in der aktuellen Agentensandbox:

```sh
cargo test -p harw-config -p harw-runtime -p harw-tui
cargo check -p harw-cli
cargo fmt --check -p harw-cli
```

Grund:

```text
/bin/sh: cargo: not found
/bin/sh: rustfmt: not found
```

Daher stehen ein echter Rust-Build, die fokussierten Tests und Clippy weiterhin in einer Umgebung mit installierter Rust-Toolchain aus.

## Restarbeiten

1. `/uia list`, `/uia info <name>` und `/uia use <name>` vollständig als sitzungssicheren Wechsel für nachfolgende Turns abschließen und testen.
2. Produkt-Shell-Policy als expliziten Vertrag festlegen und implementieren: Standard-Allow für `shell.exec`, Deny-Ausnahmen mit Vorrang, ohne Bubblewrap-/Capability-Schutz zu entfernen.
3. Provider-/Modell-Erkennung, Alias- und Auswahlpersistenz gemäß den noch offenen Plan-Knoten spezifizieren und implementieren.
4. Cargo-Tests, Formatierung und Clippy in einer vollständigen Rust-Umgebung ausführen.
