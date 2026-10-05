# Multimodale Modellanbindung

> Status: Entwurf (Plan, nichts davon ist gebaut)
> Stand: 2026-10-02, Basis `consolidate/main`
> Regel: Der Code ist der Ist-Zustand; dieses Dokument beschreibt nur das Delta.

## 1. Ziel

Bilder zuerst, dann Audio und Video, durch **eine** Schicht statt je Provider
ein Sonderweg: Medien werden einmal aufgenommen, geprüft und gespeichert, die
Provider-Adapter übersetzen sie pro Modell, und was ein Modell nicht versteht,
wird ausdrücklich ersetzt (Beschreibung, OCR, Transkript) statt still
verworfen.

## 2. CURRENT (aus dem Code gelesen)

| Ort | Befund |
|---|---|
| `harw-protocol/src/items.rs` | `ContentPart` kennt `Text` und `ImageUrl{url, detail}`; `deny_unknown_fields`, im Transkript persistiert. Kein Audio, kein Video, keine Binärdaten. |
| `harw-core/src/history.rs` | `flatten_content` macht Bilder zu `[image]`: "bis multimodale Eingaben gebraucht werden". `ModelMessage::User` trägt nur `text: String`. **Bilder erreichen den Provider heute nie.** |
| `harw-provider-http` | `build_chat_body_with` baut `content` aus `text`; kein `image_url`-Teil, kein `input_audio`. |
| `harw-core` (`compaction.rs`, `history_tail.rs`) | Bilder zählen als 7 Byte (`[image]`). Kosten und Platzbedarf sind unbekannt. |
| `harw-model-catalog/src/descriptor.rs` | `Modality{Text,Image,Audio,Video}`, `ModalitySet` und `capabilities.image_input: bool`. Kein Audio-/Video-Flag, keine Grenzen (Bildzahl, Größe, Auflösung). |
| `harw-protocol` `ToolCallResult` | `Success{value: Value}` / `Error`: kein Weg für ein Bild als Werkzeugergebnis. |
| `harw-browser` | `ArtifactRef{id, kind: Screenshot, mime_type, size_bytes, uri}`: Screenshots sind Verweise, aber nichts verbindet sie mit dem Modell. |
| `doc.read_pdf` | Text-Werkzeug (Mistral-OCR-Installation, `harw-provider-http` Ankerkommentar). Dokument-OCR geht heute nur über Werkzeug, nicht über Modelleingabe. |
| TUI, SDK, Mobil | Zeigen `[Bild]`/`[image]`. Ein TUI-Test behandelt die Bild-URL als Geheimnis (`secret.example/token`): URLs dürfen nicht in Anzeige oder Log. |

## 3. Was die Anbieter verlangen (belegt, mit Quelle)

| Anbieter | Form | Grenzen |
|---|---|---|
| Anthropic ([Vision](https://platform.claude.com/docs/en/build-with-claude/vision)) | `image`-Block mit `source` = `base64` (+`media_type`), `url` oder `file` (Files-API-`file_id`); Bilder vor Text bevorzugt | JPEG/PNG/GIF/WebP, GIF nur erster Frame; 10 MB je Bild (base64), 100 Bilder je Anfrage bei 200k-Fenster, sonst 600; 8000×8000 px, über 20 Bilder strenger (2000 px); Request-Limit 32 MB; Kosten `⌈b/28⌉×⌈h/28⌉` Token, Obergrenze 1568 (Standard) bzw. 4784 (hohe Auflösung); `tool_result` darf Bilder tragen, wird dort nicht herunterskaliert sondern abgelehnt; Base64 wird jede Runde neu gesendet (Files-API empfohlen) |
| OpenAI ([Bilder](https://developers.openai.com/api/docs/guides/images-vision)) | Bild per URL oder Data-URL, `detail` = `low`/`high`/`auto`/`original` | PNG/JPEG/WEBP/nicht animiertes GIF; bis 512 MB Nutzlast, 1500 Bilder, 30 000 Patches; Bildtoken zählen zum TPM. Hinweis: die abgerufene Zusammenfassung nannte den Teiltyp `input_image` (Responses-API-Form); für Chat-Completions gilt `image_url` mit Objekt. Vor dem Bauen gegen die Referenz prüfen. |
| Mistral ([Vision](https://docs.mistral.ai/capabilities/vision)) | `image_url` (URL oder Base64-Data-URL) | Large 3, Medium 3.1, Small 3.2, Ministral 3. Dokumente/OCR laufen über den getrennten Document-AI-Endpunkt. Zahlen zu Größe/Anzahl stehen nicht auf der abgerufenen Seite. |
| vLLM selbst gehostet ([Multimodal Inputs](https://docs.vllm.ai/en/stable/features/multimodal_inputs/), [Qwen3-VL-Rezept](https://docs.vllm.ai/projects/recipes/en/stable/Qwen/Qwen3-VL.html)) | OpenAI-kompatibel: `image_url`, `video_url`; lokale Pfade `file://` nur mit `--allowed-local-media-path`; `--limit-mm-per-prompt` begrenzt Medien je Anfrage; Qwen3-VL: `--mm-encoder-tp-mode data` | Der Server **holt URLs selbst** (SSRF-Fläche auf seiner Seite). Die Doku-Seite lieferte beim Abruf HTTP 429; Angaben stammen aus Suchzusammenfassungen. |
| Qwen-Omni über vLLM-Omni ([Doku](https://docs.vllm.ai/projects/vllm-omni/en/latest/user_guide/examples/online_serving/qwen3_omni/)) | Audio-Eingabe über `chat/completions` (MP3, WAV, OGG, FLAC, M4A); `modalities: ["text","audio"]` für Sprachausgabe | `/v1/realtime` ist nicht mit dem OpenAI-Realtime-Protokoll kompatibel (16 kHz statt 24 kHz); RFC offen. |
| Aleph Alpha | PhariaInference hat `/transcribe`, `/tokenize`, `/embed`; Tool-Calling nur Worker-Typ `vllm` | Ob Chat-Bildeingabe oder Audio unterstützt wird, sagt die Doku nicht: messen. |
| Gemini, Ollama | nicht recherchiert | offen |

Nicht verifiziert: dass OpenAI-kompatible `tool`-Nachrichten nur Text tragen
dürfen (aus Erfahrung, nicht frisch belegt). Falls zutreffend, braucht ein
Werkzeug-Bild bei diesen Anbietern eine Folgenachricht (siehe M4).

## 4. TARGET: Grundsätze

1. **Medien liegen nicht im Transkript.** Items tragen einen `MediaRef`
   (inhaltsadressiert, SHA-256, MIME, Größe, optional Maße/Dauer). Die Bytes
   liegen in einem Medienspeicher. Gründe: Base64 würde jede Runde neu gesendet
   und jede Sitzungswiedergabe aufblähen; Verdichtung und die
   Kompakt-Strömung des Telefons brauchen kleine Items; URLs sind Geheimnisse.
2. **Fähigkeit steuert den Weg.** Der Anfragebauer fragt den Katalog (Modalität
   und Grenzen des gewählten Modells). Versteht das Modell das Medium nicht,
   wird es durch eine **benannte Ersetzung** ersetzt (OCR-Text, Bildbeschreibung,
   Transkript), mit Hinweis im Verlauf. Nie stilles Weglassen, wie es
   `flatten_content` heute tut.
3. **Prüfen vor Senden.** MIME aus den Magic Bytes (nicht Dateiendung), erlaubte
   Formate, Größe, Auflösung, Anzahl; auf die Stufe des Zielmodells
   herunterrechnen; Metadaten (EXIF/GPS) entfernen. Zahlen aus Abschnitt 3.
4. **Keine fremden URLs.** Wir senden Bytes, die wir selbst gehalten und geprüft
   haben. Eine Nutzer-URL geht nie ungeprüft an einen Dritten (bestehende
   `EgressUrl`-Regeln), und bei selbst gehosteten Servern bevorzugen wir
   Data-URL statt `file://` oder fremder URL (der Server holt sonst selbst).
5. **Medien sind unvertrauenswürdig.** Text im Bild ist Eingabe, nicht
   Anweisung. Werkzeug-Medien tragen die vorhandene Vertrauensmarke
   (`ResultTrust`), und ein Bild aus dem Browser darf keine Freigabe auslösen.
6. **Datenabfluss ist eine Richtlinienfrage.** Ein Bild an einen Cloud-Anbieter
   zu senden ist Abfluss. Pro Anbieter/Modell als `local` oder `cloud`
   behandeln (die Unterscheidung existiert in `harw-config`); Standard: Medien
   aus Dateien und Browser nur an lokale Modelle, an Cloud nur mit Freigabe.
7. **Kosten sind sichtbar.** Eine Schätzung je Anbieter fließt in das
   Kontextbudget und in die Verdichtung (Bild → Beschreibung + Verweis).
8. **Zusätzlich, nicht ersetzend.** `ImageUrl` bleibt lesbar; neue Varianten
   sind additiv.

## 5. DELTA (Zyklen, unten nach oben, je ein Entwurfs-PR)

| Zyklus | Inhalt | Prüfung |
|---|---|---|
| **M0 Typen** | `MediaRef`/`MediaKind` (Bild, Audio, Video) in `harw-types`; additive `ContentPart::Media`; Versionshinweis, weil `deny_unknown_fields` ältere Leser bricht (siehe Entscheidung 4) | Serde-Rundlauf, alter Leser gegen neue Daten, Mutationsprüfung |
| **M1 Medienspeicher** | Neues Crate (Schicht A/B): Aufnahme, Magic-Byte-Prüfung, Grenzen, EXIF entfernen, Herunterrechnen, inhaltsadressiert, Aufbewahrung | Tests mit absichtlich falschen MIME/Größen/Bombs (Dekompression), Gates (`warden-no-c-build`!) |
| **M2 Kern** | `ModelMessage::User` trägt `parts` statt `text`; `text()`-Hilfsfunktion für Altaufrufer; `flatten_content` nur noch für Schätzung/Verdichtung | alle bestehenden Tests grün; Test: Bild erreicht `ModelRequest` |
| **M3 Katalog** | `ModelCapabilities` um Audio-/Video-Eingabe und Grenzen (Bildzahl, Bytes, Stufen) erweitern; Belegung aus `providers.toml`/Scan; beobachtete Werte gewinnen | Katalog-Tests, Scan gegen vLLM (`--limit-mm-per-prompt`) |
| **M4 Adapter** | `openai-chat` (`image_url`, `input_audio`), Anthropic (`image`-Quellen; später Files-API), Mistral (`image_url`), vLLM/Qwen-VL (`video_url`); Werkzeug-Bild: Anthropic im `tool_result`, sonst Folgenachricht nach der `tool`-Nachricht | Pro Adapter Body-Tests; Mutation: ohne Capability-Check wird gesendet |
| **M5 Ersatzwege** | `media.describe`, OCR (bestehendes `doc.read_pdf`-Muster), Transkription (`/transcribe` bei Pharia oder selbst gehostetes ASR) als Werkzeuge für Modelle ohne die Modalität | Nicht-Vision-Modell bekommt Beschreibung + Hinweis |
| **M6 Erzeuger** | TUI (`/attach`, Einfügen), SDK (`Input::image`), Telefon (Foto), Browser-Screenshot und `fs.read` eines Bildes → `MediaRef` | je Erzeuger ein E2E gegen scripted Provider |
| **M7 Sitzungsprotokoll** | Frames tragen `MediaRef`; `media.get` mit Vorschau für das Kompaktprofil; Recht `media.read`; Größenkappen; Telefon lädt nur auf Wunsch | E2E gegen echten Daemon, wie bei den Sitzungs-Tests |
| **M8 Budget/Verdichtung** | Schätzer je Anbieter (Anthropic `⌈b/28⌉×⌈h/28⌉`, Obergrenzen; OpenAI-Patches; lokal nach Server-Grenzen); Verdichtung ersetzt Medien durch Beschreibung + Verweis | Budgettests, kein Medium wird ohne Ersatz entfernt |
| **M9 Ausgabe-Modalitäten** | Sprache/Bild als Modellausgabe (`modalities` bei Qwen-Omni). **Zurückgestellt.** | |

Reihenfolge: M0 → M1 → M2 → M3 → M4 (nur Bilder) → M6 (TUI) ist der kürzeste
Weg bis zum ersten Bild an ein Modell. Audio und Video erst danach.

## 6. Entscheidungen, die der Nutzer treffen muss

1. **Aufbewahrung**: Wo liegt der Medienspeicher (Sitzungsverzeichnis,
   inhaltsadressiert) und wie lange? Löschen bei Sitzungsende?
2. **Bild-Abhängigkeit**: Ein reiner Rust-Decoder (z. B. das `image`-Crate)
   muss durch `warden-dependency-budget` und `warden-no-c-build`; sonst nur
   Magic-Byte-Prüfung ohne Herunterrechnen. Das Budget bestimmt den Umfang von M1.
3. **Datenschutz-Standard**: Medien nur an lokale Modelle (Empfehlung) oder
   auch an Cloud mit Freigabe?
4. **Schema**: Additive `ContentPart::Media` mit Protokollversion anheben
   (`harw.session.v1` bleibt tolerant für neue Frames, aber das gespeicherte
   Transkript nicht)?
5. **Reihenfolge**: Bilder zuerst (Empfehlung), Audio/Video später.

## 7. Nicht geklärt

- Aleph Alpha: Bild-/Audio-Eingabe im Chat und die Form der Anfrage.
- Gemini- und Ollama-Formen.
- OpenAI-kompatible `tool`-Nachrichten mit Bildern (siehe oben).
- Echte Messung gegen einen laufenden vLLM-Server mit Qwen-VL (braucht GPU,
  hier nicht verfügbar); bis dahin sind die vLLM-Flags Doku-Stand.
- Kosten und Grenzen bei Mistral (Zahlen fehlen auf der abgerufenen Seite).
