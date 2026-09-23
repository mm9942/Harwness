# Anwendung von Mias Rust Coding Bible auf killer

Grundlage: bereitgestellte `Rust_Coding_Bible_Regelwerk.pdf`, 20. September 2026,
208 Regeln einschließlich der ausdrücklich dokumentierten Konflikte K01–K22.
Der Auftrag „beachte die Regeln“ aktiviert die für dieses Projekt einschlägigen
Vorgaben; SGH-Domäneninfrastruktur und historische Claude-Modellnamen werden nicht
als vorhandene Komponenten behauptet.

## Konkrete Umsetzung

| Regeln | Anwendung |
| --- | --- |
| R001–R003, R005 | Modulplan in DESIGN; error/types/CLI/Kernel/Logik getrennt |
| R011–R018 | Direkte Cargo-Aufrufe nach Upload auf fmt/add/metadata beschränkt; Orchestrator verwendet Make, zuerst clippy-tests |
| R019–R027, R034 | Dependencies neu mit cargo add eingetragen; Cargo.lock; docs/Registry-Abgleich; kein anyhow/thiserror/failure/log/env_logger im aufgelösten Produktionsgraph |
| R035–R062 | Borrowed Eingaben, OwnedFd per Ownership, Cow für unveränderten Ausgabetext, spezifische Konvertierungen |
| R063–R076 | Keine Threads/Async/Shared-Mutation nötig; schmale statisch dispatchte Test-Traits |
| R077–R099 | Zentraler handgeschriebener Crate-Fehler, Display/Debug/source/From/Result, Kontext am I/O-Rand; kein produktives unwrap/expect |
| R100–R113 | Exhaustive Completion-Enums, Iteratoren, kontrollierte Guards; kein Panic-Fallback |
| R114–R120 | Plan<Selected> → Plan<Approved> konsumiert self; PhantomData, privater Besitz |
| R121–R127 | forbid(unsafe_code); Low-Level-Aufrufe an sichere rustix-API delegiert |
| R128–R141 | Keine Krypto-/UUID-/Mandantendomäne. sudo besitzt die Passwortinteraktion; killer liest keine Passwörter ein |
| R142–R148 | Synchron, keine gestarteten Worker-Threads oder Async-Tasks |
| R149–R158 | tracing, einmaliger Subscriber, --log LEVEL, strukturierte Felder und Kill-Span; CLI-Daten getrennt |
| R159–R176 | Keine produktiven Panic-Platzhalter; dokumentierte Derives; Modul-/Item-/Ownership-/Fehlerdokumentation |
| R177–R185 | Pure Unit-Tests am Modulende; externe Tests unter tests/ explizit ignored; austauschbare Signal-/Subprozessgrenzen |
| R186–R198 | Disjunkte Fehler-, CLI/Engine-, Test-/Doc-Arbeit mit verfügbaren internen Agenten; Build zentral, Review lesend |
| R199–R208 | Kein fremder Bestands-Code verschoben, keine bestehende SGH-Produktgrenze geändert; Helper-Argumente und JSON gemeinsam betrachtet |

## Sichtbare Konfliktentscheidungen für dieses neue Projekt

- **K01–K04:** Strenge direkte Cargo-Whitelist. Das neu erstellte Makefile ist
  Teil des ausdrücklich beauftragten Projektsetups, kein versteckter Alias zum
  Umgehen eines bestehenden Projektverbots. `clippy-tests` prüft tatsächlich
  ausschließlich statisch. `test` und `test-system` sind getrennte explizite Ziele.
- **K05–K06/K17:** Die genannten Claude-/Sonnet-Rollen sind in dieser Laufzeit
  nicht vorhanden. Verfügbare interne Agenten übernehmen die entsprechenden
  Aufgaben mit disjunkten Dateien; kein erfundener Sonnet-Aufruf, keine externe Instanz.
- **K07:** Eine konkrete zentrale Fehlerdomäne für dieses kleine einzelne Binary.
  Kernel-/Procfs-/CLI-/Helper-Operationen erhalten benannte Varianten und Kontext.
- **K08–K10:** Produktiv keine unwrap/expect; fehlende optionale Procfs-Metadaten
  bekommen dokumentierte Fallbacks und Diagnosen, Pflichtwerte scheitern explizit.
- **K11/K16:** Die ausdrücklich gewünschten Clap-Derives und Serde-Derives bleiben
  mit Expansionskommentar. Tabellen und JSON sind CLI-Ausgabe; Betriebsmeldungen tracing.
- **K12/K15:** Nicht klonbarer Plan mit konsumierender Zustimmung; keine eigenen
  unsafe-Blöcke. Ein Typmarker ersetzt nicht die tatsächliche CLI-Bestätigung.
- **R183:** Dateisystem-/Prozesstests sind ausnahmslos opt-in. `make test-system`
  führt genau diese ignorierten Tests aus, `make test` nur die standardmäßigen Tests.

## Verlauf und Nachweisgrenzen

Vor Eingang des Regelwerks existierte ein nicht fertiger Entwurf mit anyhow,
libc-Syscalls, manuell eingetragenen Dependencies und direkten Cargo-Testläufen.
Diese Läufe waren absichtlich rote Tests gegen eine leere main-Funktion, kein
Erfolgsnachweis. Nach Eingang wurden die betroffenen Produktionspfade ersetzt
und die Dependencies über cargo add neu aufgenommen.

Ein Code-/Dokumentationsreview und erfolgreiche Prüfungen sind keine formale
Vollständigkeitsgarantie für sämtliche 208 Regeln. Konkrete ausgeführte Checks,
noch nicht geprüfte Plattformen und sudo-Grenzen stehen in VALIDATION.md.

Die spätere ausdrückliche Nutzerkorrektur legt KILL → warten → KILL fest.
Keine Regel wird als Anlass verwendet, diese gewünschte Semantik durch TERM zu ersetzen.
