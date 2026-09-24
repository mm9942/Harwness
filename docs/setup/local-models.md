# Lokale Modelle: vLLM, LM Studio und Ollama

Status: Ist (ab Runde 7, Teil L)

Diese Seite beschreibt, wie `harw` mit einem lokal (oder im eigenen LAN)
betriebenen Modell-Server arbeitet. Unterstützt wird jeder Server mit einer
OpenAI-kompatiblen `chat/completions`-Schnittstelle. Erprobt sind vLLM,
LM Studio und Ollama.

## Was `harw` bei lokalen Providern anders macht

Ein Provider gilt als **lokal**, wenn seine Basis-URL auf die eigene Maschine
zeigt (`localhost`, `127.0.0.0/8`, `::1`), wenn er den Transport `ollama`
nutzt oder wenn er mit `allow_insecure_lan = true` auf eine private LAN-IP
zeigt. Für lokale Provider gelten andere Vorgaben. Jede davon lässt sich in
`providers/<name>.toml` einzeln überschreiben.

| Einstellung | Cloud-Vorgabe | Lokale Vorgabe | Bedeutung |
|---|---|---|---|
| `auth_header` | `bearer` | `none` (nur ohne `auth`) | kein Schlüssel, kein `Authorization`-Header |
| `max_concurrency` | unbegrenzt | `1` (beim Anlegen und im Katalog) | ein Modell, eine GPU: Anfragen laufen nacheinander |
| `request_timeout_secs` | 120 | 600 | Gesamtzeit ohne Streaming bzw. Wartezeit bis zu den Antwort-Headern |
| `stream_idle_timeout_secs` | = Request-Zeitlimit | 120 | Abbruch erst, wenn so lange **kein Byte** kommt |
| `retry_timeouts` | `true` | `false` | Zeitüberschreitungen werden lokal nicht wiederholt |
| `max_tokens_field` | `max_completion_tokens` | `max_tokens` | Feldname der Ausgabegrenze (`both` sendet beide) |
| `send_reasoning_effort` | `true` | `false` | `reasoning_effort` im Request |
| `strict_tools` | `true` | `false` | `"strict"` und strikte Schemas je Werkzeug |
| `parallel_tool_calls` | (weggelassen) | `false` | Wert von `parallel_tool_calls`, sobald Werkzeuge angeboten werden |

Weitere Punkte:

- **Streaming:** Beim Streaming gibt es kein Gesamt-Zeitlimit mehr. Eine lange,
  aber stetig fließende Antwort wird also nicht abgeschnitten. Das gilt auch
  für Cloud-Provider.
- **Kontextfenster:** Lokale Modelle bekommen nie das Fenster des Herstellers
  aus dem eingebauten Katalog. Ein lokal gestartetes `Qwen3-32B` hat das
  Fenster, das der Server mit `--max-model-len` bzw. der Kontextlänge in
  LM Studio bekommen hat. Maßgeblich ist `context_window` in
  `models/<id>.toml`. `harw provider scan` trägt das Fenster ein, sobald der
  Server es meldet. Fehlt es, gilt ein Rückfallfenster von 32k Token, und die
  Sitzung warnt im Log.
- **Proxy:** Für Loopback-Adressen verwendet `harw` nie einen Proxy aus
  `HTTP(S)_PROXY`. Das wirkt wie ein automatisches `NO_PROXY` für `localhost`.
- **Denktext und Werkzeug-Syntax:** `<think>…</think>` erscheint im Live-Stream
  als Denktext, nicht als Antwort. Text ab `<tool_call>`, `[TOOL_CALLS]` oder
  `<|python_tag|>` wird nicht live angezeigt. Am Ende der Antwort werden
  solche Text-Aufrufe erkannt: Hermes/Qwen-`<tool_call>`, Mistral-
  `[TOOL_CALLS]`, Llama-`<|python_tag|>` sowie eine Antwort, die nur aus einem
  JSON-Aufruf eines angebotenen Werkzeugs besteht. Unbekannte Werkzeugnamen
  oder kaputtes JSON werden nie ausgeführt. Der Text bleibt dann Text.
- **Modelle ohne Werkzeuge:** Steht in `models/<id>.toml` unter
  `[capabilities]` der Eintrag `tool_calling = false`, bietet `harw` dem Modell
  keine Werkzeuge an. Stattdessen bekommt es einen kurzen Hinweis. `harw
  provider scan` setzt den Wert, wenn der Server `supported_parameters` ohne
  `"tools"` meldet.

## vLLM

vLLM braucht für Werkzeugaufrufe zwei Startoptionen:
`--enable-auto-tool-choice` und einen zum Modell passenden
`--tool-call-parser`. Ohne sie antwortet das Modell nur mit Text.

```bash
# Qwen3 (Hermes-Format für Werkzeuge), 32k Kontext, eine GPU
vllm serve Qwen/Qwen3-32B \
  --port 8000 \
  --max-model-len 32768 \
  --enable-auto-tool-choice \
  --tool-call-parser hermes \
  --reasoning-parser qwen3

# Qwen3-Coder
vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --port 8000 --max-model-len 65536 \
  --enable-auto-tool-choice --tool-call-parser qwen3_coder
```

Übliche Parser: `hermes` (Qwen2.5/Qwen3, Hermes), `qwen3_coder`, `mistral`,
`llama3_json` (Llama 3.x). Mit `--reasoning-parser` liefert vLLM den Denktext
getrennt in `reasoning_content`. `harw` liest ihn, zeigt ihn aber nicht als
Antwort an.

`harw` einrichten:

```bash
harw provider add vllm --api openai-chat --base-url http://localhost:8000/v1 --no-auth
harw provider scan vllm          # liest die Modelle und `max_model_len`
harw config set default_provider vllm   # optional: vLLM als Standard
harw model default Qwen/Qwen3-32B
```

Läuft vLLM mit `--api-key`, statt `--no-auth` die Referenz angeben:
`--auth env:VLLM_API_KEY`. Der Header ist dann `Authorization: Bearer …`.
`--auth-header x-api-key` wählt einen anderen Transport.

## LM Studio

1. In LM Studio unter **Developer** den lokalen Server starten
   (Standard-Port 1234).
2. Ein Modell laden und dabei die **Context Length** bewusst setzen (z. B.
   32768). Das ist das Fenster, mit dem `harw` rechnet.
3. In `harw` einrichten:

```bash
harw provider add lmstudio --api openai-chat --base-url http://localhost:1234/v1 --no-auth
harw provider scan lmstudio
```

LM Studio meldet das Kontextfenster nicht über `/v1/models`. `harw provider
scan` fragt deshalb für lokale Provider zusätzlich `GET /api/v0/models` ab und
übernimmt `loaded_context_length` bzw. `max_context_length`. Ein von Hand in
`models/<id>.toml` eingetragenes `context_window` bleibt bei einem erneuten
Scan erhalten, wenn der Server keines meldet.

Der Katalog-Seed legt `providers/lmstudio.toml` und `providers/vllm.toml`
bereits deaktiviert an, mit `auth_header = "none"` und
`max_concurrency = 1`. `harw provider enable lmstudio` genügt dann.

## Ollama

```bash
harw provider add lokal --api ollama --base-url http://localhost:11434 --no-auth
harw provider scan lokal
```

Ollama meldet das Kontextfenster nicht. Deshalb `context_window` in
`models/<id>.toml` passend zu `num_ctx` setzen.

## Server im LAN

`http` zu einer privaten IP (`10.0.0.0/8`, `172.16.0.0/12`,
`192.168.0.0/16`, IPv6-ULA) ist nur mit ausdrücklichem Opt-in erlaubt. Der
Datenverkehr ist dann unverschlüsselt:

```bash
harw provider add gpu-box --api openai-chat \
  --base-url http://192.168.1.20:8000/v1 --no-auth --allow-insecure-lan
```

Hostnamen (`gpu-box.lan`) zählen nicht, weil DNS überallhin zeigen kann. Wer
einen Namen braucht, stellt TLS davor und nutzt `https`.

## Kontextfenster und Nebenläufigkeit

```toml
# models/Qwen%2FQwen3-32B.toml (von `harw provider scan` geschrieben)
id = "Qwen/Qwen3-32B"
provider = "vllm"
context_window = 32768

[capabilities]
tool_calling = true
```

```toml
# providers/vllm.toml
name = "vllm"
api = "openai-chat"
base_url = "http://localhost:8000/v1"
auth_header = "none"
max_concurrency = 1          # 2–4, wenn der Server genug KV-Cache hat
request_timeout_secs = 900   # sehr lange Prompts auf langsamer Hardware
```

`max_concurrency` ist die empfohlene Stelle für die Parallelität.
`[rate_limit] max_concurrent` gilt zusätzlich je Budget-Bucket (auch je
Modell). Stehen beide, gewinnt die kleinere Grenze. Weitere Anfragen, etwa von
parallelen Kindern, warten in der Warteschlange des Providers. Die Wartezeit
zählt nicht zum Request-Zeitlimit, und das Log meldet
`provider.concurrency.waiting_for_slot`.

Bei Fenstern unter 64k passen die vollständigen Werkzeugschemas oft nicht
neben Auftrag und Verlauf. Solche Kinder am besten mit einer Rolle starten,
die nur wenige Werkzeuge hat, z. B. Explorer.

## Rollen gemischt: lokal und Cloud

Die Rollen-Modelle lassen sich je Stelle wählen. Ein häufiges Muster:
Erkunden und einfache Worker laufen lokal, der Orchestrator in der Cloud.

```bash
harw model internal set explorer Qwen/Qwen3-32B --provider vllm
harw model internal set worker-simple Qwen/Qwen3-32B --provider vllm
harw model internal set root-orchestrator claude-opus-5-5 --provider anthropic
```

Gleichwertig in der Harness-Konfiguration:

```toml
[internal_models.explorer]
provider = "vllm"
model = "Qwen/Qwen3-32B"

[internal_models.worker_simple]
provider = "vllm"
model = "Qwen/Qwen3-32B"
```

Mit `max_concurrency = 1` arbeitet eine Explorer-Welle ihre Kinder dann
nacheinander ab, statt den lokalen Server zu überlasten.
