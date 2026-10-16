# Selbst gehostetes Qwen und Aleph Alpha: Anbindung und Tool-Calls

Stand: 2026-10-02. Online-Recherche plus Abgleich mit dem Code
(`harw-provider-http`, `harw-model-catalog`, `docs/setup/local-models.md`).
Jede Aussage trägt ihre Quelle; was nicht belegt ist, steht unter
„Nicht verifiziert".

## 1. Kurzfassung

1. **Beide Wege sind derselbe Weg.** Qwen auf vLLM und PhariaInference sprechen
   OpenAI-kompatibles `chat/completions` mit `tools`. `harw` hat dafür schon
   den Transport `openai-chat`; ein eigener Aleph-Alpha-Transport ist nicht
   nötig, nur ein Provider-Eintrag und Betriebsregeln.
2. **Der Tool-Call-Mechanismus liegt im Server, nicht im Client.** vLLM parst die
   Modellausgabe (Parser pro Modellfamilie) und liefert strukturierte
   `tool_calls`. Falsche Parser-Wahl = Tool-Calls kommen als Text zurück.
3. **Es gibt eine echte Lücke im Client** (siehe 5): das Qwen3-Coder/3.5/3.6-Format
   (`<function=..><parameter=..>`) wird vom Text-Rückfall nicht erkannt.
4. **Aleph Alpha**: Tool-Calling nur für Worker-Typ `vllm`; die Doku nennt
   weder `tool_choice`, parallele Aufrufe noch Streaming. Das muss vor
   Produktivnutzung gegen eine echte Instanz gemessen werden.

## 2. Qwen selbst hosten

### Server und Parser (belegt)

| Modell | vLLM-Parser | Reasoning-Parser | Quelle |
|---|---|---|---|
| Qwen2.5, QwQ-32B, Qwen3 (Instruct/Thinking) | `hermes` | `qwen3` (Qwen-Doku nennt `deepseek_r1` für vLLM ≥ 0.8.5) | [vLLM Tool Calling](https://docs.vllm.ai/en/latest/features/tool_calling/), [Qwen Function Calling](https://qwen.readthedocs.io/en/latest/framework/function_call.html) |
| Qwen3-Coder | `qwen3_xml` (vLLM-Doku) bzw. `qwen3_coder` (unsere Doku) | `qwen3` | wie oben; Abweichung siehe „Nicht verifiziert" |
| Qwen3.5 / 3.6 | `qwen3_coder` (ältere Qwen3.5-Fixes: `qwen35_coder`) | `qwen3` | [vLLM Recipes Qwen3.5/3.6](https://docs.vllm.ai/projects/recipes/en/stable/Qwen/Qwen3.5.html), [PR #35347](https://github.com/vllm-project/vllm/pull/35347) |

Beispiel (Recipes-Seite): `vllm serve Qwen/Qwen3.6-27B --max-model-len 262144
--reasoning-parser qwen3 --enable-auto-tool-choice --tool-call-parser qwen3_coder`.
Denken abschalten serverweit: `--default-chat-template-kwargs
'{"enable_thinking": false}'`. Für Latenz: MTP (`--speculative-config
'{"method":"mtp","num_speculative_tokens":1}'`), kostet unter hoher Last
Durchsatz, weil spekulative Tokens KV-Cache belegen.

### Wie Tool-Calls vermittelt werden (belegt)

- Das Modell erzeugt Text in seinem Format (Hermes: JSON im `<tool_call>`-Tag;
  Qwen3-Coder-Familie: XML). Der vLLM-Parser macht daraus `tool_calls[]`
  mit `id`, `type: function`, `function.{name,arguments}`.
- `tool_choice`: `auto` über den Parser (ohne Grammatik, außer `strict: true`
  oder `--tool-strict-level`), `required` und benannte Funktion über
  Structured Outputs (dann garantiert), `none` schaltet Tools ab.
- Streaming: vLLM sendet jeden Tool-Call vollständig, sobald seine Region
  parst; unvollständige oder nicht parsbare Aufrufe fallen weg.
- Reasoning kommt getrennt in `reasoning_content`, wenn `--reasoning-parser`
  gesetzt ist; im Thinking-Modus steht es vor den Tool-Calls.
- Ergebnisse gehen als `role: "tool"` mit `tool_call_id` zurück; parallele
  Aufrufe in einer Antwort sind möglich.
- Das Protokoll wird nicht garantiert eingehalten; Produktionscode braucht
  Schutz gegen fehlerhafte Ausgaben (Qwen-Doku).

### Bekannte Fallen (belegt)

- Hermes-Parser und Streaming: Fehler mit bestimmten Tokens bei Qwen3
  ([vLLM #31871](https://github.com/vllm-project/vllm/issues/31871),
  [PR #45310](https://github.com/vllm-project/vllm/pull/45310)); Aleph Alpha
  warnt davor in der eigenen Doku.
- `tool_choice: "required"` zusammen mit Reasoning und Tool-Calls lieferte 400
  ([vLLM #19051](https://github.com/vllm-project/vllm/issues/19051)).
- Parameter werden teils falsch serialisiert (Arrays als String).
- Stop-Wort-basierte Templates sind bei Reasoning-Modellen nicht empfohlen.
- llama.cpp/`llama-server` braucht `--jinja`, sonst scheitern Tool-Calls still;
  Ollama und vLLM wenden die Templates selbst an
  ([Qwen llama.cpp](https://qwen.readthedocs.io/en/latest/run_locally/llama.cpp.html),
  [Ollama: Streaming mit Tools](https://ollama.com/blog/streaming-tool)).
- Empfohlene Sampling-Werte der Qwen-Doku: `temperature=0.7`, `top_p=0.8`,
  `repetition_penalty=1.05` (Beispielwerte, je Modellkarte prüfen).

## 3. Aleph Alpha (PhariaAI / PhariaInference)

Belegt:

- PhariaInference bietet `/chat/completions` (u. a. neben `/complete`,
  `/embed`, `/tokenize`); Zugang per Token aus PhariaOS (Service-Konto)
  ([API-Übersicht](https://docs.aleph-alpha.com/phariaai-admin-guide/latest/pharia-inference/api/index.html)).
- Chat-Completions sind in den meisten Parametern OpenAI-kompatibel;
  Aleph Alpha empfiehlt einen vorhandenen OpenAI-Client statt eines eigenen
  ([Chat](https://docs.aleph-alpha.com/products/pharia-ai/pharia-os/references/inference/endpoints/chat/),
  Suchtreffer-Zusammenfassung).
- Tool-Calling folgt der OpenAI-Spezifikation: `tools[].function.{name,
  description,parameters,strict}`, Antwort in `choices[0].message.tool_calls`,
  Tool-Nachricht mit `tool_call_id`. **Nur Worker-Typ `vllm`**, im Worker mit
  Chat-Fähigkeit aktiviert
  ([Tool calling](https://docs.aleph-alpha.com/phariaai-admin-guide/latest/pharia-inference/tool-calling.html)).
- Hinter dem Worker steckt vLLM, also gelten dieselben Parser-Regeln wie in
  Abschnitt 2; die Doku nennt `hermes` und warnt vor Streaming-Problemen bei
  Qwen3.
- Worker laufen als Kubernetes-Deployment (Helm), ziehen Aufträge aus einer
  Queue, brauchen Inference-API-URL und Worker-Token; vLLM-Worker laden
  Hugging-Face-Modelle (`type: vllm`, `model_path`, `tensor_parallel_size`)
  ([Worker-Deployment](https://docs.aleph-alpha.com/phariaai-install-config-guide/latest/configuration/worker-deployment.html),
  [vLLM-Worker 2024-10](https://docs.aleph-alpha.com/docs/changelog/2024-10-14-releasing-vllm-based-worker/)).
- Externe OpenAI-kompatible APIs lassen sich als Connectors einhängen
  ([Release 1.250800.0](https://docs.aleph-alpha.com/phariaai-home/latest/release-notes/pharia-ai/1.250800.0.html)).
- Eigene Modelle: Pharia-1-LLM-7B ist Open-Weight (Open Aleph License,
  nur Forschung/Lehre, 8k Kontext)
  ([Hugging Face](https://huggingface.co/Aleph-Alpha/Pharia-1-LLM-7B-control)).
  Wer Tools will, nimmt über den vLLM-Worker ein Modell mit Tool-Template
  (z. B. Qwen), nicht Pharia-1.

## 4. Wie `harw` das heute vermittelt (Code-Stand)

- Transport `openai-chat`; lokale Provider bekommen andere Voreinstellungen
  (`max_concurrency=1`, `strict_tools=false`, `parallel_tool_calls=false`,
  längere Timeouts) – `docs/setup/local-models.md`.
- `reasoning_content` und `reasoning` werden gelesen
  (`harw-provider-http/src/lib.rs`), `<think>` wird aus Text entfernt.
- Text-Rückfall `text_tool_calls.rs`: Hermes-JSON, `NAME{json}`,
  `<arg_key>/<arg_value>`, Mistral, Llama-Python-Tag, blankes JSON;
  fail-closed (unbekannte Namen oder kaputtes JSON werden nie ausgeführt).
- Kein Aleph-Alpha-Eintrag im Katalog; der Katalog listet Qwen-Modelle
  (`providers.toml`, `vendor_qwen.rs`) nur über Router-Provider.

## 5. Lücken und Vorschläge

| # | Befund | Vorschlag | Aufwand |
|---|---|---|---|
| 1 | Text-Rückfall erkennt das Qwen3-Coder-Format `<tool_call><function=NAME><parameter=KEY>WERT</parameter></function></tool_call>` nicht (nur `<arg_key>`-Stil). Fällt der Server-Parser aus (falsche Flags, Streaming-Bug), landet der Aufruf als Text. | Dialekt in `text_tool_calls.rs` ergänzen, fail-closed, Parameter nur als String bzw. gegen das Tool-Schema typisiert. | klein |
| 2 | Kein Provider-Vorlage `pharia` im Katalog. | `providers.toml`: `api=openai-chat`, `auth_header=bearer` (Token aus PhariaOS), Basis-URL pro Installation, `max_concurrency` aus Queue-Größe; kein Seed mit festen Modellen. | klein |
| 3 | `harw provider scan` für Pharia unbekannt (`/model-settings` statt `/v1/models`). | Scan-Variante, die `/model-settings` liest; sonst Modelle von Hand. | mittel |
| 4 | Server-Parser-Fehler sind für den Nutzer unsichtbar (Tool-Text statt Aufruf). | Diagnose: bei erkanntem Text-Aufruf Warnung „Server liefert keine strukturierten tool_calls; `--enable-auto-tool-choice`/`--tool-call-parser` prüfen“ im Log und in `harw provider scan`. | klein |
| 5 | `tool_choice: required` + Reasoning (vLLM #19051) und Streaming mit `hermes`. | Pro Provider-Capability: `tool_choice` nur `auto`, Streaming-Tool-Calls abschaltbar (nicht streamen, am Ende lesen). | mittel |
| 6 | Thinking-Modus in Mehrschritt-Tool-Schleifen. | `reasoning_content` nicht zurückspielen (außer Modellkarte verlangt es); Schalter `enable_thinking` pro Rolle (`chat_template_kwargs`). | mittel |

## 5a. Skills als Gegenmittel für kleine Kontextfenster (Mistral Vibe Work)

Quelle: [Mistral Vibe Work, Skills](https://docs.mistral.ai/vibe/work/skills).

- Ein Skill ist ein Ordner mit `SKILL.md` (Anleitung) plus optionalen
  Dateien. Kategorien: eingebaut, persönlich, Workspace (geteilt); Admins
  können Skills erzwingen.
- **Progressive Offenlegung** in drei Stufen: (1) beim Start nur Name und
  Beschreibung (etwa 100 Token je Skill), (2) bei passender Aufgabe wird das
  volle `SKILL.md` geladen, (3) bei der Ausführung werden verwiesene Dateien
  nachgeladen. Die Beschreibung entscheidet über die Aktivierung und ist als
  „Verwenden, wenn …“ zu schreiben. Aufruf per `/name`, per Namensnennung
  oder automatisch.
- Die Seite sagt nichts darüber, wie Skills zu Tools/Function-Calls stehen.
  Das bleibt Entwurfsfrage bei uns.

Warum das hier zählt: `docs/setup/local-models.md` hält fest, dass bei
Fenstern unter 64k die vollen Tool-Schemas oft nicht neben Aufgabe und Verlauf
passen. Selbst gehostetes Qwen mit `--max-model-len 32768` und
PhariaInference-Worker mit 8192 Token (Beispielkonfiguration der Doku) sind
genau dieser Fall. Progressive Offenlegung ist deshalb für diese Anbindungen
wichtiger als für Cloud-Modelle:

1. **Skills statt vorgeladener Anleitung**: nur Name und Beschreibung im
   Prompt, Rest bei Bedarf. Spart Kontext, der sonst für Tool-Schemas fehlt.
2. **Tool-Sätze pro Skill** (Vorschlag, nicht belegt): ein Skill nennt die
   Werkzeuge, die er braucht; der Aufrufer bietet dem lokalen Modell nur
   diese an. Das passt zur bestehenden Rolle „explorer mit wenigen Tools“.
3. **Der Skill ersetzt keinen Parser**: Tool-Calls laufen weiter über
   `tools`/`tool_calls` (Abschnitt 2); ein Skill ist Text im Kontext, keine
   Ausführung. Berechtigungen bleiben bei den Werkzeugen, ein Skill darf sie
   nicht erweitern.
4. **Erzwungene Skills** (Workspace-Admin) wären für uns eine
   Richtlinienfrage und gehören hinter die Freigabe-Schicht, nicht in den
   Prompt.

Offen: ob `harw` schon ein Skill-Format hat (im Repo nur Planungsdokumente
zu Harness-Mustern, unter `docs/planning/75-harness-patterns`); wenn ja,
Abgleich mit `SKILL.md` und Beschreibungs-Regel, wenn nein, eigener
Planungspunkt.

## 6. Nicht verifiziert

- Ob PhariaInference `tool_choice`, `parallel_tool_calls` und Streaming von
  Tool-Calls unterstützt: die Doku sagt dazu nichts. Messung nötig.
- Basis-URL-Form und genaue Header von PhariaInference (Doku nennt nur
  „Bearer"-Token ohne Format).
- Welche Modelle ein bestimmter Pharia-Cluster mit `vllm`-Worker bereitstellt
  (kundenspezifisch).
- Parser-Name für Qwen3-Coder: vLLM-Doku nennt `qwen3_xml`, unsere
  `local-models.md` und das Recipes-Dokument nennen `qwen3_coder`; je nach
  vLLM-Version prüfen (`vllm serve --help`).
- Das Aufrufformat von Qwen3-Coder in Lücke 1 stammt aus Kenntnis des
  Chat-Templates, nicht aus einer frisch abgerufenen Quelle; vor dem Bauen
  gegen das Template des Zielmodells prüfen.
- Alle Aussagen zu SGLang: nicht recherchiert.

## 7. Nächste Schritte (Reihenfolge)

1. Lücke 1 und 4 (klein, ein Crate, mit Mutationsprüfung).
2. Provider-Vorlage `pharia` (Lücke 2) und Messung gegen eine echte Instanz,
   um Abschnitt 6 zu schließen.
3. Lücken 5 und 6 erst nach Messung.
