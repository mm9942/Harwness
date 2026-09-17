# Goals

## Muss-Ziele
1. Jede UIA-Definition kann eine `identity.md` besitzen, die real vom
   Loader gelesen und in den System-Kontext eingehängt wird (kein
   totes Feld wie das heutige `identity_md → agent.toml`).
2. Jede UIA-Definition besitzt ein eigenes `memory/`-Verzeichnis, das
   **nicht** mit dem globalen `harw-memory`-Root oder mit
   `harw-knowledge` kollidiert.
3. Die Abgrenzung Identity/Personality/Memory ist so klar dokumentiert,
   dass ein Umsetzungsauftrag ohne Rückfrage weiß, welcher Inhalt in
   welche Datei gehört.
4. Drei Pflege-Werkzeuge sind spezifiziert (Signatur, Rolle, Freigabe),
   mit begründeter Entscheidung "3 Werkzeuge vs. 1 parametrisiertes".
5. Kein neues, viertes Memory-Konkurrenzsystem — die Empfehlung baut auf
   `harw_memory::FileMemoryStore` auf.

## Soll-Ziele
6. Migrationsfreundlichkeit für bestehende Bundles ohne neue Dateien.
7. Konsolidierungspfad (`Memory.md` als Root-Zusammenfassung) so
   beschrieben, dass er später optional an `dream_reflection` anschließen
   kann, ohne dass dies Voraussetzung für M1 ist.

## Nicht-Ziele (aus dem Auftrag übernommen)
- Keine Implementierung.
- Keine Änderung an `harw-knowledge`/`dream_reflection`-Prompts selbst.
- Keine LLM-Konsolidierungs-Logik in diesem Planungsauftrag.
