# Provider-Route: Transkriptbefund und Entscheidungsgrundlage

Stand: 2026-09-16. Dieses Dokument trennt explizite Nutzeraufträge, historische
Agentenbehauptungen und den heute im Arbeitsbaum nachprüfbaren Zustand. Es
enthält keine Credential-Werte.

## Ergebnis für die aktuelle OpenAI-Provider-Reparatur

Der Provider `openai` ist heute ausschließlich der Platform-API-Key-Pfad:
`https://api.openai.com/v1`, `openai-responses`, `OPENAI_API_KEY` bzw. ein
importierter `OPENAI_API_KEY`. Das ist konsistent und darf nicht mit einem
ChatGPT-/Codex-OAuth-Access-Token vermischt werden.

Die alte Fehlroute ist im aktuellen Code nur teilweise abgesichert: `harw auth
import codex-oauth` bricht mit einer Erklärung ab, und der `codex`-Import
übernimmt nur den Platform-Key. Die tatsächlich lokale Konfiguration
`~/.harw/profiles/default/providers/openai.toml` verweist jedoch auf
`file-json:~/.codex/auth.json#/tokens/access_token` bei
`https://api.openai.com/v1`. Damit wird ein ChatGPT-/Codex-OAuth-Token im
praktischen Standardprofil an die Platform-API geroutet und die Route ist
unbenutzbar. Eine vollständige Codex-/ChatGPT-OAuth-Integration existierte vor
dem laufenden Fix nicht; der autorisierte Fix richtet deshalb eine native,
getrennte Route ein.

Die historischen Transkripte enthalten den Vorschlag, ChatGPT-OAuth direkt an
`chatgpt.com/backend-api/codex` zu senden. Das ist eine frühere
Agentenempfehlung, keine vom Nutzer getroffene Architekturentscheidung. Die
angeforderten Claude-Transkripte enthalten keinen Nutzertext zu `app-server`,
`app server` oder `direct backend` (vollständige, groß-/kleinschreibungsfreie
Suche). Auch eine Entscheidung zwischen eigener Anmeldung und gemeinsamer
`~/.codex/auth.json` fehlt.

## Belegter Ist-Zustand

| Bereich | Befund | Quelle |
|---|---|---|
| OpenAI-Katalog | Provider-ID `openai`, `base_url=https://api.openai.com/v1`, `api=openai-responses`; Auth ist API-Key plus lokaler `codex`-Import. | `harw-model-catalog/src/providers.toml:7-17` |
| API-Key-Import | `codex` wird nach `openai` abgebildet; importiert wird ausschließlich `/OPENAI_API_KEY`. Das schützt nicht eine bereits vorhandene `file-json`-Konfiguration mit OAuth-Pointer. | `harw-cli/src/auth.rs:123-138`; `harw-model-catalog/src/sources.rs:366-388`; lokales Profil laut Parent-Befund |
| OAuth-Sperre | `codex-oauth` wird nicht importiert; eine JWT-artige direkte Eingabe für `openai` wird abgewiesen. | `harw-cli/src/auth.rs:91-100,129-138,290-307` |
| Browser-Login | `harw auth login` erlaubt nur `anthropic`. | `harw-cli/src/auth.rs:34-71,254-265` |
| Externe Datei-Credentials | Die Allowlist akzeptiert aus `.codex/auth.json` nur `/OPENAI_API_KEY` und nur für `api.openai.com`. | `harw-provider-http/src/lib.rs:169-187,1270-1341` |
| Request-Bau | Provider-Konfiguration wählt Bearer-Auth, Validierung der HTTPS-URL und konfigurierte Header; ein eigener OAuth-Refreshpfad ist dort nicht vorhanden. | `harw-provider-http/src/lib.rs:848-921,949-1017` |
| Unfertiger OAuth-Typ | `SghAuth::ChatGptOAuth` ist feature-gated und setzt selbst keine Header. | `harw-provider/src/auth.rs:64-96,112-118` |

## Transkriptbefunde

### Explizite Nutzeraufträge

1. Codex OAuth als Provider-Login ordentlich prüfen und dazu
   `../codex/codex-rs` ansehen.
   Quelle: `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:12053-12055`.
2. Den Codex-Auth-Prozess von Harw reparieren; anschließend nochmals mit
   `../codex/codex-rs` als Referenz wiederholt.
   Quelle: `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:1739-1745,1962-1968`.
3. Mehrere Provider schlugen nach einer Neuinstallation/einem Update fehl, ohne
   dass der Nutzer Keys geändert hatte. Besonders genannt: Mistral; später
   wurden 401/404 für Anthropic, BytePlus, Cloudflare, DashScope, Fireworks,
   Mistral und OpenAI gezeigt.
   Quelle: Codex-Export: `30501-30523`, `39069-39077`.
4. `harw models scan` soll nach erfolgreichem Scan nicht mehr vorhandene
   Modell-Dateien entfernen und vorhandene ersetzen; außerdem wurden CLI/TUI-
   Auswahl und Hinzufügen/Löschen von Modellen verlangt.
   Quelle: Codex-Export: `39339-39349`, `41067-41086`.

Punkt 3/4 sind Kontext und kein Auftrag, die Codex-OAuth-Route ohne
Entscheidung zu aktivieren.

### Historische technische Behauptungen

Die historische Prüfung meldete, dass ein OAuth-Access-Token früher als Bearer
an `api.openai.com/v1/responses` ging, obwohl die damalige Codex-Referenz einen
anderen Backend-Endpunkt, zusätzliche Header und Refresh verwendete. Sie
empfahl einen separaten Provider, Token-Satz und Refresh.

* Quelle der Behauptung: Bash-Transkript `12159-12337`; gespiegelt im
  Codex-Export `562-595` und `1154-1323`.
* Historischer Status: **überholt als Fehlerbeschreibung**. Der gegenwärtige
  Code importiert den OAuth-Token nicht mehr. Die Behauptung, dass ein eigener
  OAuth-Pfad samt Refresh weiterhin fehlt, ist dagegen durch den heutigen Code
  bestätigt.
* Keine Transcriptstelle belegt einen echten Request gegen OpenAI oder einen
  erfolgreichen OAuth-Refresh. Beschriebene 401/403 und Testresultate sind
  deshalb **nicht unabhängig verifiziert**.

Die gleiche Transkriptreihe behauptet, eine neue Test-Binärdatei habe bei
mehreren anderen Providern vorhandene Secrets korrekt aufgelöst. Das kann für
die Diagnose der damaligen Installationsregression nützlich sein, belegt aber
nicht den heutigen Git-Stand und bezieht sich nicht auf Codex-OAuth.

## Priorisierter Backlog

1. **P0 abgeschlossen/bei Änderung absichern:** Der `openai`-API-Key-Weg darf
   nur Platform-Keys und `api.openai.com/v1` verwenden. Behalten bzw. ergänzen:
   Tests für `codex`-Import nur mit `/OPENAI_API_KEY`, Ablehnung von
   `codex-oauth`, Ablehnung JWT-artiger Werte bei `auth token openai`.
2. **P0 laufend, Architektur entschieden:** Die native Codex-Route ist als
   kontrollierte, nur lesende Mitnutzung der kanonischen Codex-CLI-Datei
   festgelegt. Sie korrigiert nur die Standardbase, liest das Access-Token pro
   Request neu und schreibt/rotiert keine Tokens.
3. **P1 anschließend:** Getrennte, gezielte Fehlerdiagnostik und Tests für
   Token-Lebenszyklus, Account-Kontext, SSE und Discovery ergänzen. Historische
   Header-/Refresh-Vorschläge sind keine als aktuell verifiziert geltenden
   Details aus den Transkripten.
4. **P2 separat halten:** Die damaligen Multi-Provider-401, Katalog-/Scan- und
   Modellpicker-Anforderungen gegen die tatsächlich installierte Binärdatei
   reproduzieren. Nicht mit einer Codex-Auth-Änderung koppeln.

## Vollständigkeit und Grenzen der Transkriptlektüre

Die vier angeforderten Quellen wurden als vollständige Dateien inhaltlich
indiziert (Nutzer-, Assistant-, Tool- und Subagentenabschnitte), mit
Zeilenzählung, Hash und fallunabhängiger Suche nach Route/Auth/OAuth/
`app-server`/Backend/API-Key. Wiederholte eingebettete Toolausgaben wurden
dedupliziert; daraus wurden keine neuen Anforderungen abgeleitet.

| Quelle | Zeilen | SHA-256 (Kurzform) | Relevanz |
|---|---:|---|---|
| `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md` | 48.798 | `1bd5a5af…35686905` | Export mit den expliziten Reparaturaufträgen und Spiegelungen der Bash-Session. |
| `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt` | 13.879 | `3171977a…99ca006c` | Primärquelle der damaligen Codex-Audit-Behauptungen. |
| Claude Harwness `3fe35b1e-…` | 37 | `c6a01749…7c95f175` | Kurzsession; keine abweichende Codex-Entscheidung. |
| Claude Harwness `81510137-…` | 52 | `662b2079…fe40d175` | Session mit Arbeits-/Subagentenspuren; keine Nutzerentscheidung zu app-server/backend. |
| Claude Profil `5f177549-…` | 184 | `9cf3b543…c70dd938` | Provider-/Profiltranskript; keine abweichende Codex-Entscheidung. |

Die Dateien können Secrets in Toolausgaben enthalten. Dieses Dokument nennt
keine Werte und darf nicht als Aufforderung gelesen werden, Token aus den
Transkripten zu übernehmen.
