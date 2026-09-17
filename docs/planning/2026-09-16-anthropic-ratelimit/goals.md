# Goals — Anthropic-Rate-Limit-Sonderplan

## Primärziel
Nutzer versteht die tatsächliche(n) Ursache(n) der "sofortigen"
Rate-Limits bei Anthropic/sonnet-5-über-OAuth und erhält einen
priorisierten, umsetzbaren Fahrplan — ohne dass in diesem Schritt bereits
Code geändert wird.

## Teilziele
1. Diagnose ist durch Code-Belege (Datei:Zeile) UND externe Recherche
   gestützt, nicht nur Vermutung.
2. Alle vier im Auftrag genannten Optionen (a-d) sind mit Vor-/Nachteilen
   bewertet, keine Vorentscheidung ohne Begründung.
3. Es gibt eine begründete Priorisierung/Reihenfolge (siehe
   `planning-summary.md`), die direkt als Grundlage für einen
   Umsetzungsauftrag (analog zu den vier vorherigen Addenda-Aufträgen)
   dienen kann.
4. Jede vorgeschlagene Änderung ist so konkret (Datei/Zeile) beschrieben,
   dass ein `debug-orchestrator`/`development-orchestrator`-Auftrag direkt
   darauf aufsetzen kann.
5. Der Plan bleibt strikt auf den Anthropic-Provider beschränkt (kein
   Seiteneffekt auf OpenAI/DashScope/Codex-Pfade in den Empfehlungen).

## Nicht-Ziele
- Keine Implementierung, kein Cargo-Build, kein Commit.
- Keine Entscheidung "für" den Nutzer zwischen API-Key und Abo-OAuth — die
  Empfehlung schlägt vor, entscheidet aber nicht eigenmächtig für den
  Nutzer (kostenpflichtige Konsequenz).
