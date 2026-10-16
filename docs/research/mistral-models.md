# Mistral-Modelle verwenden: API, selbst gehostet, Tool-Calls

Stand: 2026-10-02. Online-Recherche plus Abgleich mit dem Code. Ergänzt
`qwen-and-aleph-alpha-self-hosted.md` (gleiche Mechanik: OpenAI-kompatibles
`chat/completions`, Parser im Server).

## 1. Kurzfassung

1. **Cloud**: `harw` hat den Provider `mistral` (`api.mistral.ai/v1`,
   `openai-chat`, `MISTRAL_API_KEY`) im Katalog. Funktioniert im Grundsatz, hat
   aber eine **echte Lücke bei Tool-Call-IDs** (Abschnitt 3).
2. **Selbst gehostet** läuft über vLLM mit `--tool-call-parser mistral
   --enable-auto-tool-choice`; ohne beides kommt fehlerhaftes JSON oder Text.
3. **Lizenz**: Large 3, Ministral und Devstral Small 2 sind Apache 2.0
   (Devstral 2 groß: modifizierte MIT); Magistral Medium ist nur per API.
4. Mistral hat eine eigene Eigenheit (IDs, `tool_choice: any`), die ein
   generischer OpenAI-Client nicht kennt.

## 2. Wire-Format der Mistral-API (belegt)

Quelle: [Mistral Function Calling](https://docs.mistral.ai/capabilities/function_calling).

- `tools[]` wie OpenAI (`type: function`, `function.{name,description,parameters}`).
- `tool_choice`: `"auto"` (Standard), `"any"` (erzwingt Tool-Nutzung),
  `"none"`. Der OpenAI-Wert `"required"` ist dort **nicht** genannt.
- `parallel_tool_calls`: Standard `true`; `false` erzwingt einen Aufruf nach dem anderen.
- Antwort: `tool_calls[]` mit `id`, `type: function`, `function.name`,
  `function.arguments` (JSON-String).
- Ergebnis zurück: `{"role":"tool","name":…,"content":…,"tool_call_id":…}`;
  das Feld `name` gehört bei Mistral zur Tool-Nachricht.
- Empfohlene Modelle laut Doku: Mistral Large 3, Devstral 2.0, Magistral
  Medium 1.2, Ministral 3.

## 3. Die Falle: Tool-Call-IDs (belegt, mehrfach in Open-Source-Agenten)

Die Mistral-API verlangt IDs aus `a-z A-Z 0-9` mit **genau 9 Zeichen**. IDs
anderer Anbieter (`call_1740241912345`, mit Unterstrich, andere Länge) führen
zu HTTP 400, sobald sie in einer Folge-Runde zurückgeschickt werden, etwa nach
einem Modellwechsel mitten in der Sitzung oder bei Router-Anbietern.
Belege: [opencode #1680](https://github.com/anomalyco/opencode/issues/1680),
[openclaw #23595](https://github.com/openclaw/openclaw/issues/23595),
[mistral-vibe #1075](https://github.com/mistralai/mistral-vibe/issues/1075),
[vLLM #59212](https://github.com/vllm-project/vllm/issues/59212) (der
Mistral-Tokenizer kürzt auf die letzten 9 Zeichen und kann dabei IDs
zusammenfallen lassen). Gängige Lösung (LangChain): stabiler Base62-Hash,
auf 9 Zeichen gekürzt.

Stand im Code: `harw-provider-http` schickt `tool_call_id` unverändert
(`lib.rs`, ToolResult-Zweig) und kennt kein `tool_choice`. Für `harw` heißt
das: Sitzungen, die mit einem anderen Anbieter begonnen wurden, brechen bei
Mistral beim nächsten Request ab; der synthetische Text-Rückfall
(`call_text_<n>`) ist ebenfalls nicht 9 Zeichen.

## 4. Selbst hosten (belegt)

| Modell | Serve-Flags (Kern) | Hinweis |
|---|---|---|
| Devstral Small 2 (24B, Apache 2.0, 256k) | `--tool-call-parser mistral --enable-auto-tool-choice --max-model-len 262144` | `mistral_common ≥ 1.8.6`, Temperatur 0,15 empfohlen ([HF](https://huggingface.co/mistralai/Devstral-Small-2-24B-Instruct-2512)) |
| Mistral Small 3.2 (24B) | dieselben Parser-Flags, `--max-model-len 32768` | ([Spheron](https://www.spheron.network/blog/deploy-devstral-gpu-cloud/), Suchtreffer) |
| Mistral Small 4 (119B, vereint Instruct/Reasoning/Devstral) | zusätzlich `--reasoning-parser mistral`; `reasoning_effort` nur `"none"` oder `"high"` (über `extra_body`) | 2×H200/B200; bei vollem 256k OOM möglich ([vLLM-Rezept](https://recipes.vllm.ai/mistralai/Mistral-Small-4-119B-2603)) |

Allgemein: vLLM ≥ 0.20.0 für die Mistral-Parser; ältere Versionen brauchen
Mistrals eigenes Docker-Image. Ohne `--tool-call-parser mistral` wird das
native Format falsch gelesen und liefert kaputtes JSON. Das Text-Format
`[TOOL_CALLS]…` erkennt unser Rückfall (`text_tool_calls.rs`, Mistral-Zweig)
bereits, fail-closed.

Lizenzen: Large 3 und Ministral Apache 2.0
([Mistral 3](https://mistral.ai/news/mistral-3/)); Devstral 2 modifizierte MIT,
Devstral Small 2 Apache 2.0 ([Devstral 2 und Vibe CLI](https://mistral.ai/news/devstral-2-vibe-cli/));
Magistral Small Apache 2.0, Magistral Medium nur API (Suchtreffer, vor
Einsatz je Modellkarte prüfen).

## 5. Mistral Vibe

- Vibe ist Mistrals Agent: **Work** (Web/Mobil, schnell/denken) und **Code**
  (Terminal, VS Code, Cloud-Sitzungen)
  ([Vibe](https://docs.mistral.ai/vibe/)). Die Vibe-CLI installiert man mit
  `uv tool install mistral-vibe` und startet sie mit `vibe` im Projekt (HF-Karte).
- Skills dort: Ordner mit `SKILL.md`, Offenlegung in drei Stufen (siehe
  Abschnitt 5a im Qwen-Dokument). Die Doku nennt weder Modelle noch
  Selbst-Hosting noch MCP; das ist nicht belegt.
- Für uns relevant als **Gegenstück** (gleiche Aufgabe wie `harw`), nicht als
  Abhängigkeit. Dass Vibe-Nutzer die ID-Fehler selbst melden (Issues #1075,
  #1104), zeigt, dass auch Mistrals eigener Client daran leidet.

## 6. Lücken und Vorschläge

| # | Befund | Vorschlag | Aufwand |
|---|---|---|---|
| 1 | Tool-Call-IDs werden unverändert gesendet; Mistral lehnt alles außer 9 alphanumerischen Zeichen ab. | Beim Bauen der Mistral-Anfrage IDs abbilden: stabiler Hash (Base62, 9 Zeichen) der Original-ID, in Aufruf **und** Ergebnis gleich; Kollisionen in einer Anfrage prüfen und fail-closed melden. Pro Provider als Option (`tool_call_id_format = "mistral9"`), Voreinstellung für `api.mistral.ai`. | klein–mittel |
| 2 | Kein `tool_choice`-Pfad im Provider. | Abbildung `Required` → `"any"` für Mistral, `"required"` für vLLM/OpenAI; nur wenn der Aufrufer es verlangt. | klein |
| 3 | `parallel_tool_calls` für Mistral nicht gesetzt (Standard `true`). | Wie bei lokalen Providern konfigurierbar lassen. | klein |
| 4 | Tool-Nachricht ohne `name`. | Für Mistral `name` mitsenden (laut Doku Teil der Nachricht). Messung nötig, ob es ohne geht. | klein |
| 5 | Nutzer-Hilfe für selbst gehostetes Mistral. | `docs/setup/local-models.md` um Mistral/Devstral (Parser `mistral`, Mindestversionen, Temperatur) ergänzen. | klein |

## 7. Nicht verifiziert

- Ob die Mistral-API `tool_choice: "required"` als Alias akzeptiert.
- Ob `name` in der Tool-Nachricht Pflicht ist.
- Aktuelle Lizenzen je Modellkarte (Suchtreffer sind Sekundärquellen).
- Reihenfolge und Verhalten von `reasoning_content` bei Magistral über die API.
- Ob der Katalog die IDs `mistral-medium-2604` und Verwandte tatsächlich
  anbietet (nur aus `providers.toml` gelesen, nicht gegen die API geprüft).
