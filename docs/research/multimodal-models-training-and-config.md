# Multimodale Modelle: Training und Konfiguration

Stand: 2026-10-02. Ergänzt `docs/planning/91-multimodal/README.md`. Grundlage sind
die Technikberichte und Modellkarten (Quellen je Abschnitt). **Abgeleitete**
Zahlen (Rechnungen von mir) sind als solche gekennzeichnet; was nicht
veröffentlicht ist, steht ausdrücklich da.

## 1. Überblick

| Familie | Bild | Audio | Video | Training offengelegt | Lizenz |
|---|---|---|---|---|---|
| Qwen3-VL (2B/4B/8B/32B dicht, 30B-A3B/235B-A22B MoE) | ja | nein | ja | **ausführlich** (Stufen, Token, RL) | Apache 2.0 (8B-Karte) |
| Qwen3-Omni (30B-A3B Thinker) | ja | ja, auch Sprachausgabe | ja | **ausführlich** | Apache 2.0 (Base, Thinking, Captioner) |
| Mistral Small 3.2 / Pixtral | ja | nein | nein | Architektur ja, **Training nicht** | Apache 2.0 |
| Aleph Alpha Pharia-1 / HAT | nein (Text) | nein | nein | **ausführlich** (Daten, Rechenzeit) | Open Aleph License (nur Forschung/Lehre); HAT nicht kommerziell veröffentlicht |

## 2. Qwen3-VL

Quellen: [Technikbericht](https://arxiv.org/abs/2511.21631) ([HTML](https://arxiv.org/html/2511.21631)),
[Modellkarte 8B](https://huggingface.co/Qwen/Qwen3-VL-8B-Instruct), `config.json`
und `preprocessor_config.json` derselben Karte.

### Architektur
- **Bildencoder:** SigLIP-2, dynamische Eingabeauflösung; SigLIP2-SO-400M als
  Standard, SigLIP2-Large (300M) bei 2B/4B. 2D-RoPE mit interpolierten
  absoluten Positionen.
- **Interleaved-MRoPE:** Zeit-, Höhen- und Breitenachse gleichmäßig über die
  Frequenzbänder verteilt (nicht blockweise). Konfiguration 8B:
  `mrope_section = [24, 20, 20]`.
- **DeepStack:** Bild-Token aus drei ViT-Schichten werden über Residual-Pfade in
  die ersten drei LLM-Schichten eingespeist (8B: `deepstack_visual_indexes = [8, 16, 24]`).
- **Zeitstempel als Text:** Video-Zeit wird mit Textmarken wie `<3.0 seconds>`
  ausgerichtet (Sekunden und HMS), nicht mehr über T-RoPE.

### Konfiguration (8B, aus `config.json`)
Text: 36 Schichten, Hidden 4096, 32 Köpfe, 8 KV-Köpfe, Vokabular 151 936,
`max_position_embeddings` 262 144, bf16. Bild-ViT: Tiefe 27, Hidden 1152, Patch
16, `spatial_merge_size` 2, `out_hidden_size` 4096. Spezial-Token:
Vision-Start 151652, Vision-Ende 151653, Bild 151655, Video 151656.

### Vorverarbeitung (aus `preprocessor_config.json`)
`patch_size` 16, `merge_size` 2, `temporal_patch_size` 2, `image_mean` und
`image_std` je 0,5; `shortest_edge` 65 536 und `longest_edge` 16 777 216 (das
sind Pixel-Zahlen, also min/max Pixel je Bild).
- **Abgeleitet:** Patch 16 × Merge 2 = 32 px je Bild-Token. 65 536 px ≈ 64
  Token Minimum, 16 777 216 px ≈ 16 384 Token Maximum je Bild. Ein 1000×1000-Bild
  kostet grob 1000 Token (Rundung auf Vielfache von 32 nicht geprüft).

### Training (Technikbericht)
| Stufe | Ziel | trainiert | Token | Sequenzlänge |
|---|---|---|---|---|
| S0 | Bild-Sprache-Ausrichtung | nur Merger (LLM und Encoder eingefroren) | 67 Mrd. | 8 192 |
| S1 | multimodales Vortraining | alle Parameter | ca. 1 Bio. | 8 192 |
| S2 | Langkontext | alle Parameter | ca. 1 Bio. | 32 768 |
| S3 | Ultralangkontext | alle Parameter | 100 Mrd. | 262 144 |

Daten: Bildbeschreibungen und verschränkte Text-Bild-Folgen, Wissen (Entitäten),
OCR (30 Mio. Beispiele, 39 Sprachen), Dokumentparsing und lange Dokumente,
Grounding/Zählen (Box und Punkt, normiert auf [0,1000]), räumliches Verstehen
und 3D, Code (Text und multimodal), Video mit zeitbewussten Beschreibungen,
STEM (6 Mio. Diagrammbeschreibungen, über 12 Mio. Denkaufgaben), Agentenaufgaben
(GUI, Function Calling, Suche).

Nachtraining:
1. **SFT:** etwa 1,2 Mio. Beispiele, ein Drittel nur Text, zwei Drittel
   multimodal; erst 32K Länge (eine Epoche), dann 256K im Mischplan. Abfrage-
   und Antwortfilter (Regeln und Modell).
2. **Strong-to-Weak-Destillation:** erst Off-Policy (Lehrerausgaben), dann
   On-Policy (Schüler erzeugt, KL-Minimierung).
3. **RL:** Reasoning-RL (ca. 30 000 Aufgaben: Mathe, Code, Grounding, Rätsel),
   allgemeines RL (Instruktionsbefolgung, Präferenz), "Denken mit Bildern"
   (10 000 Kaltstart-Beispiele, dann 120 000 destillierte). Algorithmus: SAPO.
- Verlust: Wurzel-Gewichtung zwischen reinem Text und multimodal.
- Thinking- und Instruct-Varianten werden getrennt nachtrainiert.
- Infrastruktur: bis 10 000 GPUs, Tensor-/Pipeline-/Kontext-/Experten-Parallelität
  und ZeRO-1; Inferenz mit vLLM oder SGLang.

### Empfohlene Erzeugungsparameter (Modellkarte)
| Aufgabe | Temp. | top_p | top_k | presence_penalty | max. Ausgabe |
|---|---|---|---|---|---|
| Bild/Video | 0,7 | 0,8 | 20 | 1,5 | 16 384 |
| reiner Text | 1,0 | 1,0 | 40 | 2,0 | 32 768 |

`flash_attention_2` wird besonders für Mehrbild und Video empfohlen;
Transformers ≥ 4.57.0; Kontext nativ 256K, erweiterbar auf 1M.

## 3. Qwen3-Omni

Quellen: [Technikbericht](https://arxiv.org/abs/2509.17765) ([HTML](https://arxiv.org/html/2509.17765)),
[vLLM-Omni](https://docs.vllm.ai/projects/vllm-omni/en/latest/user_guide/examples/online_serving/qwen3_omni/).

- **Aufbau:** Thinker-Talker-MoE. Thinker 30B-A3B, Talker 3B-A0,3B mit
  MTP-Modul (80M) und Code2Wav-Vocoder (200M, Faltungsnetz). Mehrcodebuch-
  Sprachcodec, theoretische Erstpaket-Latenz 234 ms (Audio) und 547 ms (Video)
  bei Parallelität 1, Echtzeitfaktor unter 1,0.
- **Audioencoder AuT:** ca. 650M Parameter, 20 Mio. Stunden überwachtes Audio,
  12,5 Hz Ausgaberate (80 ms je Frame). Zusammensetzung: 80 % chinesisch/englisch
  pseudo-gelabelte ASR, 10 % mehrsprachige ASR, 10 % Audioverstehen.
- **Vortraining:** S1 Encoder-Ausrichtung (LLM eingefroren, 8 192), S2 allgemein
  multimodal (ca. 2 Bio. Token, 8 192; Aufteilung Text 0,57 / Audio 0,77 /
  Bild 0,82 / Video 0,05 / Video-Audio 0,05 Bio.), S3 Langkontext (32 768).
- **Nachtraining:** Thinker: SFT → Strong-to-Weak-Destillation → GSPO. Talker:
  hunderte Millionen Sprachbeispiele → CPT → DPO → Sprecher-Feintuning.
- **Sprachen:** Text 119, Sprachverstehen 19, Sprachausgabe 10.
- **Veröffentlicht:** Base, Thinking und ein Captioner (Audio→Text), Apache 2.0.
- **Serving-Grenze:** Der Echtzeit-Endpunkt (`/v1/realtime`) ist nicht mit dem
  OpenAI-Realtime-Protokoll kompatibel (16 kHz statt 24 kHz); RFC offen.
  Ausgabe-Modalitäten über `modalities: ["text","audio"]` bei `chat/completions`.

## 4. Mistral: Pixtral und Small 3.x

Quellen: [Pixtral-Paper](https://arxiv.org/abs/2410.07073) ([HTML](https://arxiv.org/html/2410.07073)),
[Small 3.2 Karte](https://huggingface.co/mistralai/Mistral-Small-3.2-24B-Instruct-2506),
dessen `params.json`.

### Architektur (Pixtral 12B)
Vision-Encoder **Pixtral-ViT** (400M, von Grund auf trainiert): 24 Schichten, 16
Köpfe, Dim 1024, Kopf 64, Hidden 4096, Patch 16, Kontext 4096. Vier Besonderheiten:
`[IMAGE BREAK]` zwischen Bildzeilen und `[IMAGE END]` (trennt gleiche Patchzahl bei
verschiedenem Seitenverhältnis), RoPE-2D statt gelernter Positionen, gated FFN,
Sequenz-Packing mit Block-Diagonalmaske (kein Aufmerksamkeitsfluss zwischen
Bildern). Decoder: Mistral Nemo 12B (40 Schichten, 32 Köpfe, Dim 5120, Kontext
131 072), Verbindung über zwei volle Schichten mit GeLU; Bild-Token wie Text-Token
mit RoPE-1D. Native Auflösung und Seitenverhältnis.

### Konfiguration Mistral Small 3.2 (`params.json`)
Text: `dim` 5120, 40 Schichten, `head_dim` 128, `hidden_dim` 32768, 32 Köpfe, 8 KV-Köpfe,
`rope_theta` 1e9, Vokabular 131 072, `max_position_embeddings` 131 072. Vision:
`image_size` 1540, `patch_size` 14, Hidden 1024, 24 Schichten, 16 Köpfe,
`spatial_merge_size` 2, `image_token_id` 10, `image_break_token_id` 12,
`image_end_token_id` 13, `mm_projector_id` `patch_merge`.
- **Abgeleitet:** Patch 14 × Merge 2 = 28 px je Bild-Token. Maximum 1540 px → 55×55
  = 3025 Token plus Zeilenumbrüche und Ende (genauer Zähler nicht geprüft).

### Betrieb (Modellkarte)
`vllm serve … --tokenizer_mode mistral --config_format mistral --load_format
mistral --tool-call-parser mistral --enable-auto-tool-choice
--limit-mm-per-prompt '{"image":10}'`. Temperatur 0,15, Systemprompt aus
`SYSTEM_PROMPT.txt` empfohlen, etwa 55 GB GPU-Speicher in bf16. 3.2 gegenüber 3.1:
bessere Instruktionsbefolgung, halb so viele Endlosausgaben, robustere
Function-Calling-Vorlagen. Apache 2.0.

### Was Mistral **nicht** veröffentlicht
Das Pixtral-Paper nennt keine Trainingsdaten und -größen, keine Stufen, keine
Hyperparameter, keine Datenpipeline, keine Dauer oder Rechenleistung, keine
Verlustfunktion und keine Instruktions-Tuning-Methode; nur "vortrainiert auf
großen verschränkten Bild-Text-Dokumenten". Für Small 3.x liegt nur die
Architektur in `params.json` vor.

## 5. Aleph Alpha: Pharia-1 und HAT

Quellen: [Modellkarte](https://huggingface.co/Aleph-Alpha/Pharia-1-LLM-7B-control),
[HAT](https://arxiv.org/abs/2501.10322), [T-Free-Blog](https://aleph-alpha.com/en/blog/t-free-hierarchical-autoregressive-transformers-for-language-fairness-and-sovereignty/),
[GermanWeb](https://arxiv.org/html/2505.00022v3). Beide sind **Textmodelle**; sie
gehören hierher als Vergleich, weil Aleph Alpha die Anbindung über PhariaInference
mit vLLM-Workern anbietet und dort Qwen-/Mistral-Gewichte laufen können.

### Pharia-1-LLM-7B (eigenes Vortraining, nicht von einem Fremdmodell abgeleitet)
- **Architektur:** 27 Schichten, Dim 4608, 36 Köpfe, 4 KV-Köpfe (GQA), Kopfgröße 128,
  Kontext 8192, RoPE-Basis 1 000 000, ca. 7B Parameter.
- **Tokenizer:** 128 000 Einträge, Unigram (SentencePiece); geringere "Fertility"
  für Deutsch, Italienisch, Niederländisch, Spanisch als bei Mistral/Llama.
  (Familie getestet: 64k–192k, BPE vs. Unigram; Unigram mit 128k gewählt.)
- **Daten:** 7,7 Bio. Token (Stand April 2023): 60 % Web (4,7 Bio.), 40 %
  strukturiert (3 Bio.); Sprachen Englisch 66,7 %, Spanisch 9,8 %, Deutsch 8,5 %,
  Französisch 8,4 %, Italienisch 4,9 %, Portugiesisch 1,1 %, Niederländisch 0,6 %.
- **Rechenzeit:** A100: 582k Iterationen, 356k GPU-Stunden, ca. 2,75·10²³ FLOPs;
  H100: 350k Iterationen, 96k GPU-Stunden, ca. 1,68·10²³ FLOPs; MFU 0,66 (A100)
  bzw. 0,5 (H100); bis 256 GPUs; Schrittdauer 8,6 s (A100), 3,6 s (H100).
- **Stufen:** zwei Curriculum-Phasen Vortraining → Instruktions-Feintuning
  (Curriculum, mehrteilige Beispiele) → `-control-aligned` mit DPO.
- **Prompt-Format:** Llama-3-nah (`<|start_header_id|>`, `<|eot_id|>`), Rollen
  system/user/assistant.
- **Sicherheit (Herstellerangabe):** `-control-aligned` 8,9 % unsichere Ausgaben
  gegenüber 35 % bei der unalignierten Variante. Der Hersteller sagt, die
  unalignierte Variante brauche anwendungsspezifische Absicherung. Abweichend
  berichten Sekundärquellen, `aligned` liege hinter Llama-3.1-8B-Instruct;
  eine Primärmessung dazu habe ich nicht.

### HAT / T-Free (Forschung)
- Aufbau: leichter Zeichen-Encoder (Wort-Embeddings aus Zeichen), 32-Schichten-
  Wort-Backbone im Llama-3-Stil, kleiner Zeichen-Decoder; 256 Einträge (UTF-8-Bytes),
  Ende-zu-Ende ohne vorab angepassten Tokenizer.
- Ergebnisse: bis 7B auf Augenhöhe mit Subword-Tokenizern, robuster gegen
  Eingabestörungen, fast doppelt so schnelles Weitertrainieren auf neuer Sprache
  (Finnisch: 54 Mrd. Wörter), besseres Behalten alten Wissens; 7B mit 2,3 Bio.
  Token-Äquivalenten auf DCLM-Baseline, Kontext 3072 Wörter, MMLU 59,4 %.
- Autoren: Neitemeier, Deiseroth, Eichenberg, Balles (arXiv 2501.10322).
- Daten: [Aleph-Alpha-GermanWeb](https://arxiv.org/html/2505.00022v3), 628 Mrd.
  Wörter (Common Crawl gefiltert 78, FineWeb2 235, synthetisch 329 Mrd.).

## 6. Speicher- und Kontextrechnung (abgeleitet)

KV-Cache je Token = 2 (K und V) × Schichten × KV-Köpfe × Kopfgröße × 2 Byte (bf16):

| Modell | je Token | bei Maximalkontext |
|---|---|---|
| Qwen3-VL-8B (36 × 8 × 128) | 144 KiB | 256K Token → ca. 36 GiB |
| Mistral Small 3.2 (40 × 8 × 128) | 160 KiB | 131 072 → ca. 20 GiB |
| Pharia-1-7B (27 × 4 × 128) | 54 KiB | 8192 → ca. 0,4 GiB |

Dazu kommen die Gewichte (Small 3.2: ca. 55 GB nach Modellkarte). Folge: Ein Bild
von etwa 1000 Token (Qwen) oder bis 3000 Token (Mistral, höchste Auflösung) frisst
Fenster und KV-Speicher spürbar; auf kleinen Fenstern (siehe
`docs/setup/local-models.md`: unter 64k passen volle Tool-Schemas oft nicht) ist die
Auflösung vor dem Senden zu begrenzen (Plan M1).

## 7. Konsequenzen für `harw`-Konfiguration

Im Code gefunden: **keine** Felder für Sampling (`temperature`, `top_p`, `top_k`,
`presence_penalty`) und **kein** `chat_template_kwargs` oder `extra_body`
(Suche in `harw-config`, `harw-provider-http`, `harw-model-catalog`). Die Modelle
verlangen aber genau das:

| Bedarf | Beleg | Vorschlag |
|---|---|---|
| Sampling je Modell und Aufgabe (Qwen-VL: 0,7/0,8/20/1,5; Mistral/Devstral: 0,15) | Modellkarten | Modelldatei `models/<id>.toml` mit `[sampling]`, getrennt für Bild/Text; Provider-Vorgabe überschreibbar |
| Denken ein/aus (`enable_thinking`) | vLLM-Rezepte | `chat_template_kwargs` je Rolle/Modell, als geschlossene Menge statt freiem JSON |
| `reasoning_effort` bei Mistral Small 4: nur `none`/`high` | vLLM-Rezept | Wertemenge je Modell im Katalog prüfen, falsche Werte lokal ablehnen |
| `--limit-mm-per-prompt` (Mistral: 10 Bilder) und Pixelgrenzen (`min/max_pixels`) | Karten, `preprocessor_config.json` | Katalog-Grenzen (Plan M3) aus Server-Scan füllen; Vorverarbeitung (M1) hält sie ein |
| Video: Bildrate, Einzelbilder | Qwen-Karte | als eigene Grenze, später |
| Mistral-Parser/Tokenizer-Flags | Mistral-Karte | in `docs/setup/local-models.md` ergänzen |
| Lizenz je Modell (Pharia nur Forschung/Lehre) | Karten | Katalogfeld `license`, Warnung bei Nutzung in kommerzieller Konfiguration |

## 8. Offen

- Qwen3-Omni: Thinker-`config.json` (Schichten, KV-Köpfe) nicht abgerufen, daher
  keine KV-Rechnung.
- Qwen3-VL: ob der Prozessor Seiten auf Vielfache von 32 rundet, nicht geprüft.
- Mistral Small 3.2: genauer Bild-Token-Zähler und Trainingsdaten unbekannt.
- Pharia: Primärmessung zur Sicherheitsangabe fehlt; die Karte nennt 8,9 %/35 %
  als eigene Messung.
- Messung mit echtem vLLM und GPU steht aus.
