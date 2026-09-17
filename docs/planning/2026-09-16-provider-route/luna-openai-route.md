# Luna: OpenAI-Provider-Route – Befund und Abnahmeplan

## Befund zuerst

Der aktuelle Rust-Code besitzt bereits eine explizite, providerbewusste Routing-Schicht. `RoutingModelProvider::select` verwendet nur bei `provider_id = None` den Default; eine leere ID oder unbekannte ID liefert `ModelError::RequestFailed` und ruft keinen Backend-Provider auf (`harw-provider-http/src/routing.rs:65-99`). Das ist der gewünschte Sicherheitsvertrag. Der konkrete Fehler muss deshalb an der Übergabe von Config/Session/HTTP-Transport oder an der Codex-OAuth-Erwartung eingegrenzt werden, bevor ein Fallback ergänzt wird.

`build_provider_with_optional_resolver` baut alle aktivierten Provider in eine `BTreeMap`; ein nicht-default Provider mit Konstruktionsfehler wird als `UnavailableProvider` registriert, während ein Fehler des Defaults den Aufbau abbricht (`harw-provider-http/src/lib.rs:260-300`). Der Default wird über `config.harness.default_provider` und `default_model` festgelegt. `build_named_provider` wählt für den Default das Harness-Modell, für andere Provider das erste konfigurierte bzw. lexikographisch erste Modell (`lib.rs:311-375`).

Für alle Nicht-Anthropic-APIs wird aktuell `OpenAiResponsesProvider::from_named_config` verwendet (`lib.rs:356-375`). Diese Funktion prüft Endpoint/Auth, setzt `provider_id`, wählt über `transport_from_api` den Transport und akzeptiert nur `openai-chat`, `openai-responses` oder `ollama` (`lib.rs:842-921`, `1086-1095`). `openai-responses` geht an `/responses`, alle anderen unterstützten OpenAI-kompatiblen Dialekte an den Chat-Transport. Discovery fragt bei beiden OpenAI-Dialekten `{base_url}/models` mit Bearer-Auth ab (`harw-provider-http/src/discovery.rs:170-247`).

Der providerinterne Schutz ist ebenfalls vorhanden: `selected_model` weist einen Request für eine andere Provider-ID zurück und übernimmt nur ein nichtleeres `model_id` als Request-Override (`harw-provider-http/src/lib.rs:924-947`). Die fokussierten Routing-Tests decken Default-Routing, explizites Routing, unbekannte/leer­e IDs und deterministische Providerlisten ab (`harw-provider-http/src/routing.rs:143-320`).

## Wahrscheinliche Fehlerklassen

1. **Falsche Konfiguration:** Ein wirksamer Layer setzt `default_provider`/`default_model` anders als das Onboarding anzeigt. Die Discovery-Tests zeigen, dass Layer-Reihenfolge und eingeschränkte Repo-Layer entscheidend sind (`harw-config/src/discovery.rs:1257-1438`).
2. **Dialekt/Endpoint-Mismatch:** `openai-chat` und `openai-responses` sind unterschiedliche Wire-Pfade. Ein Gateway kann zwar OpenAI-kompatibel sein, aber nur einen der beiden Pfade anbieten. Die Auswahl muss aus `provider.api` stammen und darf nicht still überschrieben werden.
3. **Credential-Verwechslung:** Der Export `harw-export-1789398616.md:15-25, 537-574` dokumentiert sowohl `invalid x-api-key` als auch den späteren Codex-OAuth-Fehler `401 Missing scopes: api.responses.write`. Ein ChatGPT/Codex-Token ist damit nicht automatisch als `api.openai.com`-Platform-Key verwendbar. Der vorhandene Code erlaubt allowlistete Codex-Dateien nur für offizielle Hosts (`harw-provider-http/src/lib.rs:150-183, 1097-1110`), was die Exfiltration begrenzt, aber keinen ChatGPT-Codex-Backend-Transport implementiert.
4. **Session-Pinning:** Der Turn-Loop setzt `provider_id` aus `session.active_provider()` (`harw-core/src/turn_loop.rs:2149`). Ein `/model`- oder Session-Controller-Wechsel muss daher beide Werte konsistent aktualisieren; ein bloßer UI-Label-Wechsel reicht nicht.
5. **Falscher Modellname:** Das Claude-Transkript behauptet `gpt-6.5-terra` → `gpt-5.6-terra`. Das ist eine Config-/Katalogprüfung, keine Routing-Ausnahme. Modellnamen dürfen nicht benutzt werden, um Provider-Auswahl implizit zu verändern.

## Sicherheits- und Verhaltensregeln für die Reparatur

- `provider_id = None` bedeutet den konfigurierten Default; `Some(id)` bedeutet exakte Auswahl.
- `Some("")` und unbekannte IDs bleiben Fehler; kein Echo-Provider und kein stiller Default-Fallback.
- `model_id` bleibt beim ausgewählten Backend erhalten; der Router darf ihn nicht normalisieren oder gegen den Default austauschen.
- Auth-Fehler dürfen weder Secretwerte noch vollständige Credential-Pfade ausgeben.
- OpenAI-Platform-API-Key und ChatGPT/Codex-OAuth sind getrennte Auth-Verträge. Falls der Nutzer Codex-OAuth verlangt, muss der geeignete offizielle Backend-Transport explizit implementiert oder die Einschränkung klar gemeldet werden.
- `api = "openai-chat"` und `api = "openai-responses"` müssen jeweils ihren eigenen URL-/Body-/Response-Pfad testen.

## Konkrete Abnahmetests

| Test | Erwartung |
|---|---|
| Router mit Default `openai`, Request ohne Provider-ID | OpenAI-Backend wird aufgerufen, Modell-ID bleibt unverändert. |
| Router mit `provider_id = openai` und zweitem Provider | exakt OpenAI-Backend; zweites Backend bleibt unberührt. |
| Leere oder unbekannte Provider-ID | `RequestFailed`, kein Backend-Aufruf. |
| `openai-chat` | POST auf `{base_url}/chat/completions`, korrekte Bearer-/konfigurierte Header. |
| `openai-responses` | POST auf `{base_url}/responses`, Responses-Body und Reasoning-/Tool-Projektion. |
| Discovery | GET `{base_url}/models`, Auth-Fehler klassifiziert, Modell-ID ohne Preisannahmen. |
| Falscher Endpoint/HTTP-Redirect | vor Credential-Nutzung abgelehnt; kein Cross-Origin-Redirect. |
| Session-Providerwechsel | nächster Turn trägt Provider-ID und Modell konsistent; Resume stellt beides wieder her. |
| Codex-OAuth | klarer eigener Transport-/Capability-Test oder fail-closed, diagnostischer Hinweis ohne Token. |
| Terra-Konfiguration | kein wirksamer `gpt-6.5-terra`-Verweis; `gpt-5.6-terra` nur nach Katalog-/Discovery-Nachweis. |

## Parent-Handoff

Die Route-Datei ist read-only geprüft; ich habe keine Rust-Dateien verändert. Der Parent sollte die Implementierung auf die oben genannten Übergänge begrenzen und zuerst einen reproduzierbaren Test mit einem lokalen Mock-Provider ergänzen. Die Transkriptlage rechtfertigt keine automatische Umleitung von OpenAI zu Byteplus oder Foundry: Providerwahl und interne Modellzuordnung sind getrennte Anforderungen.

## Review des aktuellen Codex-Patches (read-only)

Der neue Patch bindet `/tokens/access_token` korrekt an die exakte Datei `HOME/.codex/auth.json`, den Pointer und die offiziellen Codex-/OpenAI-Basiswerte. Die Runtime überschreibt bei erkannter Codex-Credential den konfigurierten Endpoint auf `https://chatgpt.com/backend-api/codex`; `authorized_request` liest das Token pro Request neu und sendet es nicht an `api.openai.com`. Die Codex-Discovery verwendet `GET /models?client_version=...` und liest das erwartete `{ "models": [{"slug": ..., "context_window": ...}] }`-Schema. Das stimmt mit dem lokalen Codex-Referenzcode überein (`codex-rs/model-provider/src/models_endpoint.rs:90-108, 373-410`).

Ein konkreter Laufzeitblocker bleibt: Der Codex-Referenzclient setzt bei gestreamten Responses zusätzlich `Accept: text/event-stream` (Referenztest `codex-api/tests/clients.rs:387-425`). `harw-provider-http/src/lib.rs::authorized_request` setzt diesen Header derzeit nicht; der Patch setzt nur `stream = true` im JSON. Der Backend-Transport sollte den Header explizit für Codex setzen und per Mock-Request testen. Ohne diesen Header kann der Dienst zwar eventuell anhand des Bodies streamen, das ist aber kein abgesicherter Wire-Vertrag.

Zweiter Prüfpunkt: `login_headers` fügt `chatgpt-account-id` nur hinzu, wenn `/tokens/account_id` vorhanden ist. Der lokale Codex-Referenzclient behandelt den Account-Header beim Streaming als Teil des erwarteten Auth-Sets. Für eine belastbare Route sollte fehlende Account-ID entweder fail-closed diagnostiziert oder anhand des tatsächlichen Codex-Auth-Schemas bewusst als optional dokumentiert und getestet werden. Ein stiller Request ohne Account-ID ist derzeit nicht nachgewiesen.

Sicherheitsbefund: Die Route weist `authorization` und `chatgpt-account-id` als konfigurierbare Header zurück, lässt aber andere credentialartige Header (`api-key`, `x-api-key`, `cookie`, `proxy-authorization`) zu. Da `configured_headers` diese Werte vor dem Request auflöst, können sie zusätzlich an `chatgpt.com` gesendet werden. Das ist kein Cross-Origin-Exfiltrationspfad, sollte für die streng gebundene Codex-Route aber entweder als erlaubte Zusatzheader begründet oder durch eine vollständige Auth-Header-Allowlist verhindert werden.

SSE-Decoder: Fragmentierte `data:`-Zeilen, terminale `response.completed`/`response.incomplete`, Fehler und 16-MiB-Limit sind getestet. Nicht abgedeckt sind ein terminales Event ohne abschließende Leerzeile, ein Stream ohne `response.completed` bei späterem EOF sowie ein Mock-Request, der URL, Header und Body der Runtime gemeinsam prüft. Der EOF-Fehler ist fail-closed und daher sicher, aber die Tests sollten explizit den gewünschten Vertrag festschreiben.
