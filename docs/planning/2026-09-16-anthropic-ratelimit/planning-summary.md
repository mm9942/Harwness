# Planning Summary — Anthropic-Provider Rate-Limit-Sonderplan

Auftrag: eigenständiger Plan NUR für Anthropic, keine Implementierung.
Vollständige Artefakte in diesem Verzeichnis:
`decomposition.md`, `dependency-research.md`, `doc-sources.md`,
`parent-tasks.md`, `goals.md`, `structure-plan.md`, `test-and-error-plan.md`.
Kein `database-plan.md` (kein Datenmodell betroffen).

## Diagnose

Drei voneinander unabhängige Ursachen tragen vermutlich zum gemeldeten
Symptom bei, mit unterschiedlichem Gewicht:

1. **Policy-Durchsetzung (neu, wichtigster Befund dieser Recherche)**:
   Anthropic hat die Nutzung von Claude-Abo-OAuth-Tokens durch
   Drittanbieter-Tools zwischen Januar und April 2026 stufenweise
   **serverseitig blockiert** (nicht mehr nur per ToS untersagt) — die im
   Code bereits vorhandene Warnung
   (`harw-provider-http/src/anthropic.rs:53-72`) beschreibt einen
   inzwischen überholten, milderen Stand ("pausierte Juni-2026-Änderung").
   Das erklärt plausibel ein **sofortiges** Scheitern schon beim ersten
   Request — konsistenter mit "sofort" als reine Kontingent-Erschöpfung.
   Quellenlage: mehrere Sekundärquellen (Presse/Community, Stand 2026),
   keine Anthropic-Primärquelle mit Datum direkt verifizierbar — als starkes
   Indiz, nicht als hundertprozentiger Beweis zu werten.
2. **Geteiltes-Kontingent-Hypothese (Auftrag)**: nicht widerlegt, aber
   nachrangig, weil sie ein bereits hohes Vorab-Nutzungsniveau voraussetzt
   und ein "sofortiges" Scheitern beim allerersten Request weniger gut
   erklärt als (1). Offizielle Doku (platform.claude.com/docs/en/api/rate-limits)
   beschreibt organisationsweite RPM/ITPM/OTPM-Limits für API-Keys; für
   Abo-Nutzung existiert separat ein 5h-Fenster
   (`anthropic-ratelimit-unified-5h-*`, laut Sekundärquellen), das
   plausibel geräteübergreifend pro Konto gilt.
3. **Codebefund (unabhängig von 1/2, eigenständig reproduzierbar)**:
   Ein Anthropic-429 wird in harw zu `ModelError::RateLimited`, welches in
   `ModelError::is_retryable()` (`harw-core/src/model.rs:658-660`) **nicht**
   als retrybar gilt — im generischen `RetryingProvider`-Pfad
   (`harw-provider-http/src/retry.rs`) führt das zu **sofortigem Aufgeben
   ohne jeden Retry-Versuch**. Dies ist in
   `docs/remediation/ledger/W3/C-MODEL.md:324-329` bereits als offene,
   bewusst liegen gelassene Altlast dokumentiert. Die TUI hat einen
   separaten, bereits vorhandenen Retry-Mechanismus
   (`harw-tui/src/app.rs:3460-3620`, bis 3× je max. 120 s), der aber bei
   langen Abo-Reset-Fenstern (Minuten/Stunden) zu kurz greift und dann
   ebenfalls aufgibt.

**Einordnung**: (3) ist ein handfester, unabhängig vom OAuth-vs-API-Key-Streit
bestehender Bug und sollte unabhängig von der Grundsatzentscheidung a) vs. b)
behoben werden. (1) ändert die Risikobewertung von Option (a)/(b) erheblich:
Abo-OAuth ist nicht mehr nur "ToS-riskant", sondern schon jetzt aktiv von
Blockaden betroffen — ein rein clientseitiges Pacing (Option b) kann eine
serverseitige Policy-Blockade nicht beheben.

## Optionsbewertung

**a) Wechsel auf echten Anthropic-API-Key**
- Vorteil: einziges Mittel, das eine mögliche Policy-Blockade (Diagnose 1)
  sicher umgeht; eigenes, dokumentiertes RPM/ITPM/OTPM-Kontingent,
  unabhängig von Claude-Code-Sitzungen.
- Nachteil: kostenpflichtig, getrennt vom bestehenden Abo; erfordert
  Nutzeraktion (Key erzeugen + `harw auth token anthropic`, bereits
  vorhandener Pfad in `harw-cli/src/auth.rs:74-100`, aber ohne
  Verwechslungsschutz Abo-Token/API-Key).
- Bewertung: **einzige Option, die (1) adressiert.** Höchste Priorität, wenn
  der Nutzer belastbaren, produktionsnahen Anthropic-Zugriff will.

**b) `[rate_limit]` für Anthropic aktivieren (proaktives Pacing)**
- Vorteil: geringer Aufwand (reine Konfiguration + Verdrahtung), reduziert
  echte RPM/ITPM/OTPM-429s durch Vorab-Warten.
- Nachteil: wirkungslos gegen Diagnose (1) (Policy-Block) und gegen ein
  bereits durch Claude Code verbrauchtes 5h-Fenster (Diagnose 2) — der Pacer
  sieht nur die *eigenen* zuletzt beobachteten Header, keine Cross-Client-
  Information.
- Bewertung: sinnvolle Ergänzung, aber **kein Ersatz** für (a) bei
  Policy-Blockade.

**c) `RateLimited`/Retry-Differenzierung in `retry.rs`**
- Vorteil: behebt einen unabhängig bestätigten, dokumentierten Bug
  (`is_retryable()` deckt `RateLimited` nicht ab); wirkt in **jedem**
  Aufrufpfad (nicht nur TUI); geringer, gut lokalisierter Änderungsumfang.
- Nachteil: kann eine Policy-Blockade oder ein mehrstündiges Kontingent-Reset
  nicht "wegretryen" — nur die technische Handhabung des Fehlers wird
  korrekter, nicht die zugrunde liegende Verfügbarkeit.
- Bewertung: **niedrigstes Risiko, sollte unabhängig von a)/b) umgesetzt
  werden**, weil es einen echten Bug behebt, den auch ein API-Key-Nutzer
  bei einem harten Provider-429 träfe.

**d) UX-Meldung bei Anthropic-Rate-Limit/Policy-Block**
- Vorteil: macht (1)/(2)/(3) für den Nutzer unterscheidbar und
  handlungsleitend, ohne dass er den Code lesen muss.
- Nachteil: reine Symptombehandlung, löst keine der Ursachen.
- Bewertung: sinnvolle, kostengünstige Begleitmaßnahme zu jeder anderen
  Option, sollte aber **nach** (a)/(c) kommen, damit die Meldung auf
  korrekte Handlungsempfehlungen verweisen kann.

## Empfehlung (Reihenfolge für Umsetzungsauftrag)

1. **T1/T3c** (= Option c): `RateLimited` in `is_retryable()`/`retry.rs`
   korrekt als retrybar behandeln — unabhängiger Bugfix, niedrigstes Risiko,
   sofortiger Nutzen für jeden Aufrufpfad.
2. **T5**: Warnhinweis-Text in `anthropic.rs` auf den aktuellen
   Durchsetzungsstand (April 2026) aktualisieren — reine Doku-Korrektur,
   kein Risiko.
3. **T3a** (= Option a): geführten, verwechslungssicheren Umstieg auf
   API-Key ermöglichen (`harw-cli/src/auth.rs`) — adressiert die
   schwerwiegendste mögliche Ursache (Policy-Block), erfordert aber eine
   bewusste Nutzerentscheidung (Kosten).
4. **T4** (= Option d): UX-Meldung verbessern, sobald T1 und T3a stehen, da
   sie sich auf beide bezieht.
5. **T2**: TUI-Retry-Fenster/Konstanten an reale, ggf. lange
   Abo-Reset-Zeiten anpassen (Kopplung an T4).
6. **T3b** (= Option b): `[rate_limit]` für Anthropic optional aktivieren —
   niedrigste Priorität, weil sie das schwerwiegendste Szenario (1) nicht
   löst und nur inkrementellen Nutzen gegen normale 429s bringt.

## Offene Punkte für den Umsetzungsauftrag (nicht in diesem Plan geklärt)
- Exakte Verdrahtungsstelle, an der `ProviderToml.rate_limit` an
  `AnthropicMessagesProvider::configure_rate_limit` übergeben wird
  (vermutlich `harw-runtime/src/assembly.rs`), muss vor T3b lokalisiert
  werden.
- Verhalten von `parse_retry_after` bei einem 429 ohne `retry-after`-Header
  (z. B. Spend-Cap-artiger Fehler) ist nicht Teil dieser Recherche und sollte
  vor T1 verifiziert werden.
- Ob Anthropic für Abo-OAuth tatsächlich `anthropic-ratelimit-unified-5h-*`
  an harw sendet (und nicht nur an offizielle Clients), ist unverifiziert —
  sollte im Umsetzungsauftrag durch Logging eines realen 429-Response-Header-
  Dumps geprüft werden, bevor Diagnose (1) vs. (2) final gewichtet wird.
