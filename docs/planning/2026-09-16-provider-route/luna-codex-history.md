# Luna: Codex-Historie und Zusatzbefunde

**Stand:** 2026-09-16  
**Quellenumfang:** projektbezogene Codex-JSONL vor dem Start der aktuellen Session; Forks dedupliziert nach `session_meta`.

## Abdeckung

Ausgewertet wurden 17 projektbezogene Codex-Logs:

- zwei ergänzende Projektlogs vom 14.09.: `rollout-2026-09-14T17-30-51-...jsonl` und `rollout-2026-09-14T18-48-47-...jsonl`;
- 15 Logs vom 15./16.09. bis vor dem Dateinamens-Cutoff `2026-09-16T04:24:25`;
- darunter die Root-Session `rollout-2026-09-16T04-05-22-01a0a7f6-28c4-7443-8ef3-d692e57e1037.jsonl` und ihre 12 Luna-Forks.

Die Forks hatten alle denselben Parent `01a0a7f6-28c4-7443-8ef3-d692e57e1037`; sie wurden nicht als eigenständige Nutzerhistorie doppelt gewertet. Nutzer- und Assistant-Nachrichten wurden vollständig aus den JSONL-Payloads extrahiert. Toolausgaben wurden zunächst nach Session, Tool und Schlüsselwörtern indexiert und für Provider-, TUI-, `.harw`-, Auth- und Orchestrierungsbefunde gezielt geprüft. Secrets, Tokenwerte und verschlüsselte Toolargumente wurden nicht in dieses Dokument übernommen.

## Frühere Projektlogs vom 14.09.

### `17-30-51`

Der Einmal-Modus konnte zunächst nur lesende Aufgaben ausführen. Shell-Aufträge wurden trotz passender `[permissions] allow`-Regel abgelehnt, weil `ApprovalChain::install_over_default` eine regellose Default-Policy behielt und die regelbewusste Policy verwarf. Der gezielte Fix (`clear_approval_handlers`, Austausch der Default-Kette) wurde anschließend laut Log implementiert, mit 3 Runtime-Tests und 61 Extension-API-Tests geprüft, gebaut und mit einem echten `git status --short` verifiziert. Das Binary wurde bewusst nicht nach `~/.local/bin` installiert, weil dort ein laufendes Gateway lag.

Der Log enthält außerdem einen erneuten OpenAI-Wire-Fehler durch ungültige Toolnamen (`input[5].name`), obwohl ein Tool-Namen-Codec zuvor als behoben gemeldet wurde. Das ist ein Hinweis, dass der Codec an jedem Provider-Transport und an jeder History-/Tool-Call-Projektion konsistent angewendet werden muss.

### `18-48-47`

Der Nutzer wollte fünf Bild-/Dokumentdateien aus Git entfernen. Die Dateien waren bereits im Commit `68478bc`; sie wurden aus dem Index entfernt, lokal aber bewusst behalten. Das ist relevant für die heutige Dokumentationslage: `docs/sessions/session-transcript-2026-09-14.md` und `emily-harw-config/README.md` können lokal vorhanden, aber absichtlich nicht mehr versioniert sein. Ein späterer Inventarlauf darf „lokal vorhanden“ nicht mit „Git-Quelle“ verwechseln.

## Codex- und Providerbefunde

### OpenAI/Codex-Auth

Die ältere Codex-Arbeit entfernte den Import aus `~/.codex/auth.json` über `/tokens/access_token` und erlaubte nur `/OPENAI_API_KEY`. Begründung war: ein ChatGPT-/Codex-Login-Token sei kein Platform-API-Key und dürfe nicht an `https://api.openai.com/v1/responses` gesendet werden. Der Export beobachtete dort HTTP 401 wegen fehlendem Scope `api.responses.write`.

Für die aktuelle Implementierung ist daraus eine **begründete technische Entscheidung des Parents** abzuleiten: Wenn der vorhandene `/tokens/access_token` unterstützt werden soll, braucht er einen gesonderten, fest gebundenen nativen Codex-Transport. Das ist kein wörtliches Nutzerzitat und kein stiller OpenAI-Fallback. Der historische Zielvertrag nennt `chatgpt.com/backend-api/codex`, Streaming und `chatgpt-account-id`; diese Angaben stammen aus dem damaligen Transkript und müssen gegen den lokalen Codex-Referenzcode sowie die vorhandene Auth-Dateistruktur verifiziert werden.

Abnahmeregeln für diese Route:

- `/tokens/access_token` wird nie als Platform-Key an `api.openai.com` geschickt.
- Die Route ist explizit an die Codex-Credentialquelle und den offiziellen Codex-Host gebunden.
- Provider-TOML oder ein Modellname darf den Codex-Token nicht auf einen fremden Endpoint umleiten.
- Streaming, Account-Header, Fehlerklassifikation und Secret-Redaction werden separat getestet.
- Ein fehlender/ungültiger Account-Kontext schlägt fail-closed mit einer diagnostischen, secretfreien Meldung fehl.

### Modell-Discovery und `/models`

Der Codex-Verlauf belegt eine Modellkatalog-Regression nach einer Neuinstallation: alte Modell-IDs blieben aktiv, während Recherche-/models.dev-Daten den Bestand anwachsen ließen. Als Korrektur wurde `harw models scan` auf Live-Abfragen umgestellt. Die sichere Credential-Auflösung sollte beim Scan dieselben `secrets:`-/Home-gebundenen Quellen wie die Runtime verwenden.

Die gewünschte Datenhaltung wurde präzisiert:

- `models/*.toml` ist der vom erfolgreichen Scan gepflegte Live-Bestand;
- `providers/<provider>.toml::models` ist die vom Nutzer aktivierte und in der TUI sichtbare Auswahl;
- ein erfolgreicher Scan entfernt nur nicht mehr live vorhandene aktivierte IDs und aktualisiert die Live-Details;
- Fehler wie 401, 404 oder Netzwerkfehler dürfen keine lokalen Modelle löschen;
- `harw models add/delete provider/model` und der interaktive Provider-/Modell-Picker verändern die Auswahl, nicht den kompletten Live-Katalog.

Gemeldete Providerbefunde: Mistral hatte eine veraltete Modell-ID; Anthropic erzeugte zeitweise `/v1/v1/models`; Cloudflare antwortete auf pauschales `GET /models` mit 405; Fireworks mit 412. Diese Fälle sind getrennte Providerverträge. OpenAI darf keine OpenRouter-/Foundry-/ChatGPT-Metadaten übernehmen, wenn der eigene Endpoint nur IDs meldet.

## TUI, UIA und Session-Lebenszyklus

Die nicht blockierende UIA war im Codex-Verlauf ausdrücklich als offenes Feature H geführt. Zwar existieren ein `tokio::select!`-Eventloop und Quit-Ereignisse, aber `/command` wurde innerhalb dieses Loops direkt `await`et. Damit ist nicht bewiesen, dass Eingabe und Rendering während langer Provider-/Tool-Aufträge frei bleiben.

Die echte Startmeldung ist ebenfalls nicht belegt: Eine `WELCOME`-Konstante war vorhanden, aber unbenutzt. Die UIA-Bootstrap-Logik lädt bzw. speichert eine Definition, erzeugt aber nicht automatisch eine sichtbare, modellformulierte Assistant-Nachricht.

Verbindlicher Restpunkt:

- neue Session: genau ein sichtbarer und persistierter UIA-Assistant-Turn mit aktiver UIA-Identität, Rolle und Provider/Modell;
- Resume: kurze Lage-/Auftragsmeldung statt einer zweiten Begrüßung;
- lange Delegation: UIA quittiert sofort, TUI bleibt bedienbar, Ergebnis wird später in die Turn-Queue/Fanin-Struktur eingefügt;
- `/status`, `/usage`, `/verbose`, `/cancel` und Exit bleiben währenddessen erreichbar;
- Ctrl+C/Exit und Raw-Mode-Rückkehr werden als Zustandsübergänge getestet.

Resume bleibt lückenhaft: Tool-Calls und Tool-Results werden zwar persistiert, erscheinen nach Resume aber als Platzhalter; `total_usage` wird beim Resume nicht zuverlässig übernommen; der lokale `.harw`-Sessionindex war in den älteren Logs noch offen.

## Orchestrierung und `.harw`

Die Logs bestätigen wiederholt die Diskrepanz zwischen Planobjekt und Runtime-Agent: Ein `uia-worker` oder Orchestrator wurde als Plan-Knoten angelegt, obwohl kein ausführbarer Runtime-Agent mit passender Rolle/Capability registriert war. Der korrekte Pfad ist:

```text
Nutzer → UIA → Root-Orchestrator → Sub-Orchestrator/Worker → UIA/Fanin
```

Die UIA darf keine normalen Worker direkt starten. Sub-Orchestrator-Spawns brauchen sowohl Parent-Grant als auch eine ausdrückliche Freigabe in der konkreten Agentendefinition. Worker delegieren nicht weiter. Sichtbarkeit und Spawn müssen aus Rolle, Definition, Parent-Grant, Tiefe, Budget, Autorität, Tool-/Netz-/Dateiscope und Sandbox-Ceiling berechnet werden.

Der `.harw`-Stand ist nicht vertrauenswürdig genug für eine „erledigt“-Meldung: Goal- und Plan-Namen widersprachen sich, die Historie reichte über den letzten Snapshot hinaus, und Resume/Usage-/Delegationsknoten fehlten im wirksamen Snapshot. Vor weiterer Umsetzung müssen Goal, Plan, Snapshot und Journal auf einen konsistenten ausführbaren Plan zusammengeführt werden.

## Priorisierte offene Arbeit

| Prio | Offener Punkt | Abnahme |
|---|---|---|
| P0 | Native Codex-Route für `/tokens/access_token` | eigener Host/Transport, Streaming und Account-Kontext; kein Versand an `api.openai.com`; secretfreie Fehler |
| P0 | OpenAI-Modellwerte in `/models` korrigieren | nur providergebundene Live-Daten; unbekannte Metadaten bleiben unbekannt; keine fremden Kataloge |
| P0 | TUI während langer Arbeit entblocken | Eingabe/Rendering/Status bleiben responsiv; Delegation läuft asynchron |
| P0 | UIA-Startidentität und Greeting | genau ein persistierter Assistant-Turn bei neuer Session, definierte Resume-Meldung |
| P1 | Resume/Usage/Toolzellen | ToolCall/ToolResult, Status und Usage werden vollständig rehydriert |
| P1 | Runtime-Orchestrierung | UIA→Root→Sub/Worker, Capability-Schnitt und Fan-in werden tatsächlich ausgeführt |
| P1 | `.harw`-Plan-/State-Konsistenz | aktiver Snapshot entspricht Historie, Goal und ausführbaren offenen Knoten |
| P2 | Sandbox/Permit-Restarbeiten | Ledger, UI-Bestätigung, einmaliger Permit, Audit und Workerprofile runtimeverdrahtet |
| P2 | Provider-Sonderfälle | Anthropic, Cloudflare, Fireworks und Mistral mit eigenem Discovery-Vertrag testen |

## Quellenbezug

Die zentrale Codex-Root-Quelle ist `rollout-2026-09-16T04-05-22-01a0a7f6-28c4-7443-8ef3-d692e57e1037.jsonl`; ihre Forks tragen Agentpfade `read_codex_transcript`, `read_harw_exports_a/b`, `read_harw_export_9503044`, `read_harw_export_9511666`, `read_harw_export_9519086`, `read_harw_export_9523460`, `read_harw_export_9524310`, `read_claude_raw_transcript`, `read_bash_transcript` und `read_docs_session`. Die zwei ergänzenden 14.09.-Quellen sind oben separat ausgewertet. Die konkreten Codebefunde der OpenAI-Route stehen ergänzend in [luna-openai-route.md](luna-openai-route.md).
