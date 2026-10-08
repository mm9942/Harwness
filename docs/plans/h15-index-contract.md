# H15 — Inkrementeller Datei-/Artefaktmetadaten-Index: Contract-Draft

Status: Contract-Draft (Befunde verifiziert am Quellcode; offene Punkte am Ende).

## Befundene Fakten (mit Belegen)

1. **Lese-Grenzen 1 MiB / UTF-8**: Dateiinhalte werden beim Einlesen auf 1 MiB begrenzt und müssen UTF-8 dekodierbar sein — nicht dekodierbare Dateien fallen aus dem Textpfad heraus. Beleg: `harw-lens-source/src/document.rs:254-261`.
2. **Inkrementeller Chunk-/Embedding-Cache**: Der Build-Prozess baut Chunks und Embeddings inkrementell mit Cache auf Zerlegungsebene. Beleg: `build.rs:20-54`.
3. **Invalidierung bei Modell-/Chunkerwechsel**: Wechsel von Embedding-Modell oder Chunker-Parametern invalidiert den Embedding-Cache gezielt. Beleg: `build.rs:484-503`.
4. **ChunkDigest-Identität**: Chunks tragen einen Digest als stabile Identität; Cache-Treffer werden über ihn ermittelt. Beleg: `harw-lens-source/src/chunk.rs:4-10`.
5. **Manifest-Provenance**: Manifeste dokumentieren Herkunft (Quelle, Parameter) der erzeugten Artefakte. Beleg: `manifest.rs:202-214`.
6. **Strukturelle Relationen**: Relationen zwischen Dokumenten/Chunks existieren als typisierte, belegte (evidence-backed) Verbindungen. Beleg: `relations.rs:1-19`.
7. **mtime/size-Invalidierungsschwäche**: Bestehende mtime/size-Prüfung ist zu schwach (gleiche mtime/size kann geänderten Inhalt maskieren; z. B. nach Restore/Touch). Beleg: `index.rs:314-324`.
8. **Sequenzielle Queries**: Föderierte Queries laufen sequenziell über die Stores; Parallelisierung ist noch nicht implementiert (Entwurf). Beleg: `federated.rs:309-328`.

## Vertragsspezifikation

### Artefaktmetadaten
- Pro indexiertem Artefakt (Datei/Chunk/Embedding) werden mindestens erfasst:
  - Beschreibung (Name, Pfad, MIME/Textschätzung),
  - Zuordnung (Projekt, Workspace, übergeordnetes Artefakt),
  - Hash (Content-Hash als Identität, nicht nur mtime/size — behebt Befund 7),
  - Version (Index-Schema-Version, Chunker-/Embedding-Konfiguration),
  - Provenance (erzeugende Komponente, Parameter — konsistent zu Befund 5).
- Inkrementalität: unveränderte Artefakte (Hash-Treffer) werden nicht neu gechunkt/ embedded; Modell-/Chunkerwechsel invalidiert gezielt (Befunde 2–3).

### Relationen
- Nur typisierte, belegte Relationen mit Evidence-Referenz (Quelle: Befund 6); freie Assoziationen ohne Beleg sind unzulässig.

### Retrieval
- Embedding-Suche mit parallelen Queries über alle Stores (Zielzustand; aktuell sequenziell, Befund 8). Schnittstelle so gestalten, dass Parallelisierung ohne API-Bruch möglich ist.
- Qwen-Mustervorschläge: Für Queries werden Mustervorschläge (z. B. Qwen-basierte Query-Varianten) generiert — ohne konkretes Modell.

### Autoritäts- und Secret-Grenzen
- Autorität für Metadaten liegt allein beim Index-Builder; Konsumenten sind read-only.
- Secrets werden nicht indexiert: `.gitignore` schließt `.env`, `auth.toml`, `*.key` aus. Offen: ein Secret-Scanner-Lauf über den Index ist nicht verifiziert (siehe offene Punkte) — bis dahin gilt: kein Artefakt ohne Path-Prüfung gegen die Ausschlussliste.

### Grenzfälle (verbindlich)
- **Stale**: mtime/size allein invalidiert nicht mehr; Content-Hash entscheidet.
- **Rename**: Umbenannte Dateien werden über Hash als dasselbe Artefakt erkannt; Metadaten werden fortgeführt, Pfad aktualisiert.
- **Binary/nicht-UTF-8**: Dateien >1 MiB oder ohne UTF-8-Dekodierbarkeit werden als Binär-Artefakt geführt (Metadaten, kein Chunking/Embedding) — nicht still verworfen.
- **Unsaved worktree**: Nur on-disk-Zustand wird indexiert; ungespeicherte Editor-Puffer liegen außerhalb der Autorität des Index.

## Offene Punkte
- **Keine konkrete Qwen-ID belegt**: welches Qwen-Modell Query-Vorschläge liefert, ist offen. Beleg-Bereich: `descriptor.rs:141-160`.
- Secret-Scanner-Lauf nicht verifiziert (Ausschlussliste derzeit statisch).
- Parallelisierung der federierten Queries ist Entwurf (Befund 8).

## Tests
- Grenzdateien: exakt 1 MiB, 1 MiB + 1 Byte, nicht-UTF-8.
- Cache-Treffer/-Miss bei unverändertem Hash, bei Touch ohne Inhaltsänderung (alte Schwäche, Befund 7), bei Rename.
- Invalidierung bei Chunker-/Modellparameterwechsel.
- Relationen nur mit Evidence.
- Query-Parallelausführung (Ergebnisäquivalenz zur sequenziellen Form).
- Secret-Ausschluss: `.env`, `auth.toml`, `*.key` erscheinen nicht im Index.
