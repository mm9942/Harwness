# Decomposition — Anthropic-Provider Rate-Limit-Sonderplan

Auftrag: eigenständiger Plan NUR für den Anthropic-Provider, Ursache für
"sofortige" Rate-Limits bei sonnet-5-über-OAuth klären, Optionen bewerten,
Empfehlung liefern. Keine Implementierung.

## Teilprobleme

1. **D1 — Policy-Ebene**: Ist Abo-OAuth für harw (Drittanbieter-Tool) überhaupt
   noch zulässig/technisch funktionsfähig, unabhängig vom Kontingent selbst?
2. **D2 — Kontingent-Ebene**: Teilt sich harw das Kontingent mit der aktiven
   Claude-Code-Session desselben Kontos (5h-Fenster)?
3. **D3 — Code-Ebene (Retry-Pfad)**: Wird ein 429 von Anthropic in harw
   tatsächlich als retrybar behandelt, oder gibt es sofort auf — unabhängig
   von D1/D2?
4. **D4 — Code-Ebene (Pacing)**: Ist proaktives Pacing (`ProviderRateLimiter`)
   für `anthropic` aktiv konfiguriert?
5. **D5 — UX-Ebene**: Wie erfährt der Nutzer aktuell von einem
   Rate-Limit/Policy-Block, und ist die Meldung handlungsleitend?
6. **D6 — Optionsbewertung**: a) API-Key statt Abo-OAuth, b) `[rate_limit]`
   aktivieren, c) Retry-Differenzierung für `RateLimited`, d) UX-Meldung.
7. **D7 — Empfehlung/Reihenfolge** für einen späteren Umsetzungsauftrag.

## Reihenfolge der Bearbeitung

D1 → D2 → D3 → D4 → D5 (Diagnose) → D6 (Optionen) → D7 (Empfehlung/Synthese).
D1 wurde durch Recherche in eine höhere Priorität verschoben, weil ein
aktueller externer Befund (siehe `dependency-research.md`) die ursprüngliche
Fragestellung überlagert: Anthropic hat die Nutzung von Abo-OAuth durch
Drittanbieter-Tools zwischenzeitlich aktiv serverseitig blockiert
(nicht mehr nur ToS-Warnung), was schwerer wiegt als die reine
Kontingent-Teilungs-Hypothese.
