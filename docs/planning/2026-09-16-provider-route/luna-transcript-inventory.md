# Luna: Transkript- und Dokumentationsinventar

**Stand:** 2026-09-16  
**Zweck:** belastbare Übergabe an die Implementierung der Provider-Route und an die weitere Sitzungsplanung.

## Abdeckung und Grenzen

Im Workspace liegen 16 zugewiesene Transkriptdateien: 15 `harw-export-*.md` sowie das Claude-Transkript `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt`; zusätzlich wurde `docs/session-transcript-2026-09-14.md` als verbindliche Zusammenfassung herangezogen. Die Dateien wurden nach Dateigröße/Zeilenanzahl inventarisiert und die vollständigen Gesprächsblöcke der relevanten Provider-, Modell-, Routing-, TUI-, Persistenz- und Abschlussabschnitte gelesen. Die drei langen, weitgehend duplizierten Exporte `1789519086`, `1789523460` und `1789524310` sind hier dedupliziert; ihre abweichenden Ergänzungen sind separat erfasst. Eine semantische Einzelprüfung sämtlicher 44.205 Dokumentationszeilen ist nicht behauptet.

## Quelleninventar

| Quelle | Zeilen | Inhalt / verwertbare Forderungen |
|---|---:|---|
| `harw-export-1789377255.md` | 266 | DOD-Iststand; TUI-Command-Popup; Cargo/PATH-Frage; E2E und echte Sensorik offen. |
| `harw-export-1789381305.md` | 269 | Resume-/`.harw`-Persistenz leer; TUI-Statuszeile; UIA-Dateien `agent.toml`, `Personality.md`, `USER.md`, `definition.toml`. |
| `harw-export-1789388571.md` | 226 | Rollenmodell: UIA, Root-/Orchestrator, Sub-Orchestrator, Worker und UIA-Worker; explizite Spawn-Berechtigungen. |
| `harw-export-1789388771.md` | 653 | Ctrl+D, `.harw`-Goals/Plans/Memories/State, Sandbox- und Permit-Modell; Rust-Toolchain als Verifikationsblocker. |
| `harw-export-1789398616.md` | 655 | OpenAI/Claude-Authentifizierung, Tool-Namen-Codec, Codex-OAuth-401, Modell-/Provider-Auswahl, `/model` und `/models`. |
| `harw-export-1789406677.md` | 296 | Persona-/USER-Kontext tatsächlich laden; Command-Vorschläge/Enter; DOD-Status. |
| `harw-export-1789429814.md` | 406 | Android-Minimalvariante, autonome lokale Agenten, Telegram, MinIO/Polaris/QuestDB/Vector-Store. |
| `harw-export-1789460270.md` | 401 | fachliche Dokumentenstruktur, DevOps-/Datenfluss-Perspektive; UIA-Worker-Exploration gefordert, Spawn scheitert an Rolle. |
| `harw-export-1789465226.md` | 1.606 | Root-only-Routing/UIA-Capabilities, eingebettetes Organisationswissen, Sub-Orchestrator-Sonderrecht, modulare Sandbox, Persistenz und DOD-Workspace. |
| `harw-export-1789469522.md` | 139 | Vorherige Pläne/Umsetzungsstand prüfen; „erst coden“, Orchestratoren nutzen. |
| `harw-export-1789503044.md` | 955 | offene Punkte fertigstellen; Sandbox nicht blockierend; erste Nachricht; Cargo-Verfügbarkeit. |
| `harw-export-1789511666.md` | 79 | Codex-/Claude-/Harw-Transkripte abgleichen; `/verbose`; Auswahl-Tastatur; laufende Status-/Exit-Informationen. |
| `harw-export-1789519086.md` | 1.776 | TUI zuerst, danach Sandbox; Provider+Modell bei Start/`/status`; Tokenanzeige; Tool-/Session-Resume. |
| `harw-export-1789523460.md` | 4.540 | gleiche TUI-/Sandbox-Linie plus vollständige Transcript-/Tool-Persistenz, Delegationsvorschriften und `/mode`; mehrfach abgebrochen/fortgesetzt. |
| `harw-export-1789524310.md` | 5.623 | nahezu Duplikat der vorherigen Quelle; zusätzliche Forderung nach Orchestrator-Nutzung und Status-Transparenz. |
| `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt` | 354 | interne Modelle auf Byteplus, `gpt-6.5-terra` korrigieren, echte Agentenanzeige, Greeting/Persona/Resume-Orchestrierung. |
| `docs/session-transcript-2026-09-14.md` | 199 | normative UIA-/TUI-/Bootstrap-Zusammenfassung; Provider-/Modellerkennung und Auswahlpersistenz bleiben offen. |

## Deduplizierte Anforderungen

1. Die Provider-ID und das Modell müssen als getrennte, persistente Auswahl behandelt werden. Ein explizites `provider/model` darf nicht über den Default-Provider umgeleitet werden; unbekannte oder leere Provider-IDs müssen fail-closed scheitern.
2. `openai`/OpenAI-kompatible Provider brauchen eine konsistente Authentifizierungs- und Transportentscheidung. Ein ChatGPT/Codex-OAuth-Token ist nicht automatisch ein Platform-API-Key für `api.openai.com`; der im Export beobachtete 401 darf nicht durch stillen Fallback verdeckt werden.
3. Das Onboarding soll verbundene Provider oben und nicht verbundene unten anzeigen, beim Auswählen `/models` scannen und danach `/model provider/model` unterstützen. Lange Listen müssen mit der Auswahl scrollen.
4. Interne Modelle sollen explizit Byteplus zugeordnet werden. Die im Claude-Transkript behauptete Konfiguration nennt acht Stellen: `session_title`, `compaction_summary`, `memory_consolidation`, `dream_reflection`, `explorer`, `research`, `worker_simple`, `worker_complex`.
5. `gpt-6.5-terra` ist als falscher/unaufgelöster Name dokumentiert; als Ersatz wird `gpt-5.6-terra` genannt. Diese Behauptung ist gegen die tatsächlich geladene Konfiguration und den Modellkatalog zu verifizieren.
6. UIA, Root-/Orchestrator, optionaler Sub-Orchestrator und Worker sollen strukturell nur ihre erlaubten Spawn-Ziele sehen. Eine UIA darf nicht durch improvisierte Shell-/Explorer-Aufrufe eine gescheiterte Spawn-Policy umgehen.
7. Sitzungen müssen Turns, Tool-Aufrufe/-Ergebnisse, Provider/Modell, Tokenverbrauch und Status für Resume und Export dauerhaft speichern. `.harw` soll projektbezogen für Goals/Plans/Memories/State arbeiten; der Parent behandelt die konkrete `.harw`-JSONL-Auflösung.
8. Die Sandbox soll modular und permit-/zustimmungsgebunden sein. Ein Modell darf die Sandbox nicht selbst lockern; echte Lockerung braucht explizites Nutzersignal und Runtime-Bestätigung.
9. TUI-Anforderungen: Provider+Modell beim Start und in `/status`, sichtbare Zwischeninformationen, funktionierendes `/verbose`, Queue während laufendem Turn, funktionierende Exit-/Ctrl+C-/Ctrl+D-Pfade und korrektes Mode-Verhalten.

## Erledigt behauptet versus belegt

Das Claude-Transkript behauptet, die acht internen Modellstellen auf Byteplus umgestellt, `gpt-6.5-terra` auf `gpt-5.6-terra` ersetzt und `harw models scan` erfolgreich ausgeführt zu haben. Im Gespräch wird aber auch ausdrücklich gesagt, dass Greeting/erste echte Modellrunde, Persona-Laden und Resume-Zusammenfassung noch nicht erledigt seien. Diese Aussagen sind daher als **behauptete Änderungen**, nicht als Abnahme, zu behandeln.

Im OpenAI-Abschnitt werden 169 Provider-HTTP-Tests (15 neu) als grün behauptet. Gleichzeitig bleiben TUI-Tests wegen fremder `harw-core`-Änderungen und Clippy wegen einer ungenutzten Sandbox-Konstante blockiert. Ein live ausgeführter Codex-OAuth-Roundtrip endete mit HTTP 401 (`api.responses.write` fehlt); im Export wird dafür ein eigener ChatGPT-Backend-Transport mit Streaming und `chatgpt-account-id` als offene Arbeit genannt.

## Priorisierte Übergabe

| Prio | Arbeit | Abnahmekriterium |
|---|---|---|
| P0 | OpenAI-Route anhand von Provider-ID, API-Dialekt, Endpoint und Auth getrennt reparieren | explizit `openai/model` erreicht exakt den OpenAI-Backend; unbekannter/leer­er Provider scheitert ohne Fallback; keine Secret-Leaks in Fehlern/Logs |
| P0 | Config-/Katalogprüfung für Terra und interne Modelle | `gpt-6.5-terra` kommt nicht mehr aus einer wirksamen Konfig; jede interne Stelle hat einen existierenden Provider+Modell-Eintrag |
| P1 | Codex-OAuth-Entscheidung | entweder offizieller eigener ChatGPT-Codex-Transport mit Streaming/Account-Header und Tests oder klare, getestete Ablehnung samt Platform-Key-Hinweis |
| P1 | Provider-/Modell-Auswahlpersistenz | `/models` listet verbundene Provider, Scan speichert Auswahl, `/model provider/model` bleibt beim nächsten Turn erhalten |
| P1 | Resume-/Tool-Transcript | Resume und Export rekonstruieren ToolCall/ToolResult, Provider/Modell und Tokenwerte; Abbruch erzeugt einen persistierten Status |
| P2 | Agenten-/Delegationsoberfläche | sichtbare Rollen/Agentenstatus ohne globale verbotene Kataloge; UIA-zu-Worker bleibt strukturell unmöglich |
| P2 | TUI-/Sandbox-Abnahme | fokussierte Cargo-Tests, Clippy und manueller TTY-Test laufen in einer Umgebung mit Rust-Toolchain |

## Dokumentationsbestand im Workspace

Der Bestand wurde zusätzlich per Dateiinventar geprüft: 206 Dateien unter `docs/` (ohne diesen neuen Planungsordner), davon 34 Design-, 141 Remediation-, 3 Architektur-, 1 Audit-, 2 Setup-, 1 Migration-, 1 Research-, 1 Session- und 1 Superpowers-Datei sowie die übergeordneten `aw-*`-Dokumente. Die Remediation-Ledger enthalten überwiegend abgeschlossene Nachweise mit einzelnen „Offen/nicht verifiziert“-Abschnitten. Für die Route besonders maßgeblich sind `docs/architecture/model-provider-routing.md`, `docs/design/provider-tui-setup.md`, `docs/design/model-catalog-v2.md`, `docs/design/config-structure.md`, `docs/design/secrets-and-audit.md`, `docs/design/delegation-capabilities.md`, `docs/design/mediated-process-execution.md`, `docs/session-transcript-2026-09-14.md` sowie `harw/README.md`.

Die breiteren Dokumente enthalten zusätzliche offene Entscheidungen (u. a. Budget-/Spawn-Abrechnung, Kanal- und Workbench-Details, Skill-Runtime, MCP-Tool-Registry und Live-Roundtrips). Sie dürfen die P0-Provider-Reparatur nicht als erledigt markieren; sie gehören in spätere Plan-Knoten.
