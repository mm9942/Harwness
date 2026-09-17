# Konsolidierter Anforderungskatalog aus Codex-, Claude- und Harw-Artefakten

Stand 2026-09-16. Dieser Katalog fasst explizite Nutzeraufträge, aktive
Goal-Kriterien und dokumentierte offene Fehler zusammen. Er trennt sie von
historischen Assistant-/Subagentenvorschlägen. Details und Primärstellen stehen
in `terra-provider-route-evidence.md` und `terra-harw-state.md`.

| Priorität | Bereich | Anforderung / Akzeptanz |
|---|---|---|
| P0 | Codex/OpenAI | Bestehende `file-json:~/.codex/auth.json#/tokens/access_token`-Konfiguration darf bei Standard-OpenAI-Base nicht mehr an `api.openai.com/v1` scheitern. Native Route nur für kanonische Datei/Pointer; API-Key und Custom-Base unverändert. Token je Request neu lesen, kein Refresh/keine Rotation, Responses-SSE und Discovery gleichermaßen routen. |
| P0 | Secrets | Keine Tokenwerte, Auth-Header oder Credential-Dateiinhalte loggen, persistieren oder in Fehlern ausgeben. |
| P1 | Provider | Nach Update/Reinstall entstandene Providerprobleme reproduzieren: Mistral sowie gemeldete 401/404 von Anthropic, BytePlus, Cloudflare, DashScope, Fireworks, Mistral und OpenAI. Auth-, Netz- und API-Fehler getrennt anzeigen. |
| P1 | Modellscan | Erfolgreicher Scan ersetzt die Modelldaten eines Providers und entfernt nicht mehr verfügbare Modelle; Fehlerhafte/andere Dateien bleiben geschützt. Ein Provider gilt erst nach echtem API-Scan als verbunden. |
| P1 | Model-UX | `harw models add/delete provider/model` und ein navigierbarer Picker für aktivierte Provider; Modellwahl per Checkbox/Leertaste, Enter bestätigt. |
| P1 | UIA/Delegation | UIA kann nur eine eng begrenzte Research-Worker-Rolle für erlaubte Netzrecherche starten. Keine Schreib-, Shell-, lokalen Workspace-, Credential- oder weitere Spawn-Autorität. Spawns und Rollen im TUI sichtbar machen. |
| P1 | Fanout/Modelle | Explorer-Kinder wählen explizit `gpt-5.6-luna`; unbekannter globaler Fallback erlaubt fünf direkte Kinder, ohne andere Rollenstandards zu verändern. |
| P1 | Sandbox | Strict ist Default. Profile Strict, Cargo, tmux-Socket und Host werden zentral gewählt; Tools können kein Profil setzen. Kein pauschales Home oder `/tmp`; tmux bindet nur validierten einzelnen Socket. |
| P1 | Permit | Jeder Prozesslauf wird fail-closed über Permit geprüft; Permit bindet Befehl, Worker, Session und Umgebung. Lockerungen nur durch lokale UI-Bestätigung, nie CLI-Flag, Toolparameter, Modellentscheidung oder Kind. |
| P1 | TUI/Session | Nutzer-, Assistant-, Tool- und Spawn-Ereignisse visuell unterscheiden. Ctrl+C beendet nur den laufenden Turn, nicht die fachliche Sitzung; `harw -r` muss fortsetzbare Sessions im Projekt bzw. mit All-Projekte finden. |
| P1 | Plan/Goal | Planstatus nur über erlaubte Übergänge; Revisionen monoton. Modell darf ein Ziel nicht selbst abschließen/aufgeben. Aktive Goal-/Plan-Drift bereinigen oder sichtbar ausweisen. |
| P2 | Token-Effizienz | Stabile Prompt-Reihenfolge, Prompt-Caching nach Providervertrag, kontextfensterrelative Compaction (70/30), Usage-Aufzeichnung und Vermeidung breiter Re-Reads/fehlgeschlagener Toolloops. |
| P2 | Rate limits | Höchstens drei Retries, `retry-after`, 120-s-Cap, deterministischer Jitter und lesbarer Endfehler statt Sessionabbruch. |
| P2 | Knowledge/Memory | Knowledge-/Memory-Quellen projektbezogen, nachvollziehbar und ohne Secrets verwalten; Remember-Summarizerfehler und fehlende Sessiondateien als Datenlücke behandeln. |
| P2 | Tests | Neue Funktion nur mit fokussierten Unit-/Integrationstests und anschließend passendem Crate-/Workspace-Check. Bestehende unabhängige Fehler separat erfassen, nicht als Erfolg ausgeben. |

## Quellen und Status

* **Explizite Nutzeraufträge:** Codex-Export `:1095-1105`, `:1739-1745`,
  `:1962-1968`, `:30501-30523`, `:39069-39077`, `:39339-39349`,
  `:41067-41086`; Bash-Transkript `:12053-12055`.
* **Aktive, aber unbelegte Zielkriterien:** `.harw/goals/default/rev-25.json`.
* **Historische/zu prüfende Befunde:** `.harw/memories/facts/`,
  `.remember/logs/` und die vollständigen Claude-JSONLs. Sie sind keine
  automatische Bestätigung eines heutigen Builds.
* **Bereits erledigt laut Planstore:** die 21 `completed`-Knoten aus
  `terra-harw-state.md`; sie benötigen bei relevanter Änderung dennoch
  Code-/Testbelege.
