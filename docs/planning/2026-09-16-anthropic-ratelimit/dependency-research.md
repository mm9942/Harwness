# Research — Anthropic Rate Limits / Abo-OAuth-Durchsetzung

## R1 — Policy-Durchsetzung ist bereits aktiv (nicht nur ToS-Text)

Mehrere unabhängige, aktuelle Quellen (Stand September 2026) berichten, dass
Anthropic die Nutzung von Claude-Abo-OAuth-Tokens durch Drittanbieter-Tools
**serverseitig blockiert** hat, nicht mehr nur per Nutzungsbedingungen
untersagt:

- 9. Januar 2026: serverseitige Checks blockieren Tools wie OpenCode, Cline,
  RooCode über Nacht ohne Vorwarnung, wenn sie Abo-OAuth-Tokens nutzen.
- 19. Februar 2026: Anthropic aktualisiert die Doku, um klarzustellen, dass
  OAuth-Token-Nutzung durch Drittanbieter-Tools gegen die Nutzungsbedingungen
  verstößt.
- 4. April 2026, 12:00 PT: Anthropic blockiert Abo-Zugriff für
  Drittanbieter-Harnesses vollständig (zuerst OpenClaw, dann weitere Tools in
  den folgenden Wochen). OAuth-Token-Auth ist danach nur noch für Claude.ai
  und Claude Code selbst vorgesehen.

**Konsequenz für harw**: Der im Code bereits vorhandene
`ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING`
(`harw-provider-http/src/anthropic.rs:53-72`) beschreibt einen **veralteten**
Stand ("Juni 2026 pausiert" bezieht sich auf eine frühere, seither überholte
Ankündigung). Die tatsächliche Durchsetzung ist zum jetzigen Zeitpunkt
(2026-09) bereits produktiv scharf. Das erklärt plausibel, warum harw "sofort"
(schon beim ersten Request) abgewiesen werden kann — nicht erst nach
Verbrauch eines Kontingents, sondern weil der Request selbst als
Drittanbieter-Client erkannt und abgelehnt/limitiert werden kann (z. B. über
Client-Fingerprinting, User-Agent, Beta-Header-Kombination, IP/Geräte-
Heuristik). Dies ist eine **plausiblere Erklärung für "sofort"** als reine
Kontingent-Teilung, weil eine reine Kontingent-Teilung ("5h-Fenster ist schon
verbraucht") ein bereits vorher hohes Nutzungsniveau voraussetzt, während eine
aktive Client-Erkennung bereits beim ersten Request greifen kann.

Quellen (Sekundärberichte, keine Anthropic-Primärquelle mit Datum
verifizierbar; als Indiz, nicht als Beweis werten):
- https://dev.to/mcrolly/anthropic-kills-claude-subscription-access-for-third-party-tools-like-openclaw-what-it-means-for-3ipc
- https://gigazine.net/gsc_news/en/20260220-anthropic-third-party-block/
- https://geol.ai/briefing/anthropic-blocks-thirdparty-agent-harnesses-for-claude-subscriptions-apr-4-2026-what-it-changes-for
- https://kersai.com/anthropic-killed-third-party-claude-access-heres-every-workaround-that-still-works/

## R2 — Offizielle Rate-Limit-Doku (platform.claude.com), Stand heute

Aus `https://platform.claude.com/docs/en/api/rate-limits` (per Fetch
verifiziert, offizielle Erstquelle):

- Rate-Limits sind **organisationsweit** (API-Key-Organisation), gemessen in
  RPM/ITPM/OTPM je Modellklasse — das ist das Modell für **API-Key**-Nutzung
  (Console/platform.claude.com), NICHT explizit für Abo-OAuth dokumentiert.
- Es gibt zusätzlich `anthropic-ratelimit-unified-5h-status/-remaining/-reset`
  Header (laut Sekundärquellen/GitHub-Issues, nicht in der oben zitierten
  Haupttabelle explizit aufgeführt) — diese bilden das **5-Stunden-Fenster**
  ab, das Claude Code/Claude.ai-Abos nutzen. Das deutet auf ein vom
  API-Key-RPM/ITPM/OTPM-Modell **getrenntes** Limit-Konzept für
  Abo-Nutzung hin.
- Für Spend-Cap-429 gibt es **keinen** `retry-after`-Header
  (`enforced_spend_limit_reached`); normale Rate-Limit-429 liefern
  `retry-after`. Falls Anthropic einen vergleichbaren "Abo-Kontingent
  erschöpft"-Fehler ohne `retry-after` zurückgibt, würde harws
  `parse_retry_after`-Fallback greifen (Verhalten dort nicht Teil dieser
  Recherche, sollte im Umsetzungsauftrag geprüft werden).
- Kein öffentlich dokumentierter Hinweis darauf, dass verschiedene
  "User-Agent"/Client-Kennungen **getrennte** Kontingente innerhalb
  desselben Abo-Kontos hätten — im Gegenteil, Community-Berichte (R1) sagen,
  dass Client-Erkennung zur **Blockade**, nicht zur Kontingent-Trennung
  führt. D. h. die im Auftrag vermutete "geteiltes Kontingent"-Hypothese ist
  nicht direkt falsifiziert, aber die Blockade-Hypothese (R1) ist die
  wahrscheinlichere Erstursache für ein *sofortiges* Scheitern.

## R3 — Codebefund: 429 wird in harw NICHT über den generischen Retry-Pfad behandelt

`harw-provider-http/src/anthropic.rs:764-774`: Jeder Anthropic-429 wird zu
`ModelError::RateLimited { retry_after_secs, message }` — **nicht** zu
`ModelError::Transient` und **nicht** zu `ModelError::QuotaExceeded`.

`harw-core/src/model.rs:658-660` (`ModelError::is_retryable`):
```rust
pub fn is_retryable(&self) -> bool {
    matches!(self, Self::Transient { .. } | Self::Timeout { .. })
}
```
`RateLimited` ist **nicht** in dieser Liste. Das ist in
`docs/remediation/ledger/W3/C-MODEL.md:324-329` explizit als **bewusste,
offene Altlast** dokumentiert ("Offene Annahme 2"): der ursprüngliche Auftrag
verlangte wörtlich nur Transient/Timeout als retryable; die Migration von
`RateLimited` zu `Transient{status:Some(429),..}` wurde für eine spätere
Welle (A-ANTH/A-OAI) vorgesehen und ist laut `git status`/aktuellem Code
**bis heute nicht durchgeführt**.

**Konsequenz**: `harw-provider-http/src/retry.rs` (`RetryingProvider`,
genutzt z. B. im CLI-/Headless-Pfad) wiederholt einen Anthropic-429 **nie**
— `retry_decision` gibt sofort `GiveUp` zurück, weil `is_retryable()` für
`RateLimited` `false` liefert. Das ist eine **direkte, unabhängig von
Kontingent-Teilung bestehende Codeursache** für "sofortiges" Scheitern in
jedem Aufrufpfad, der `RetryingProvider` nutzt.

## R4 — TUI hat einen separaten, bereits vorhandenen Retry-Mechanismus für `RateLimited`

`harw-tui/src/app.rs:3460-3465,3588-3617` (`drive_turn_animated`):
- `RATE_LIMIT_MAX_ATTEMPTS = 3`, `RATE_LIMIT_AUTO_RETRY_CAP_SECS = 120`.
- Bei `ModelError::RateLimited` wartet die TUI bis zu 3-mal, je
  `retry_after_secs` (gedeckelt auf 120 s) plus Jitter, bevor sie aufgibt und
  `"⏱ Rate limit — provider busy; retry in {}s ({} attempts used)"`
  zurückgibt.
- **Problem**: Wenn Anthropics `retry_after_secs` für ein erschöpftes
  5h-Abo-Kontingent im Bereich von Minuten bis Stunden liegt (nicht Sekunden
  wie bei einem klassischen RPM/ITPM-429), wird die Wartezeit auf 120 s
  gekappt — die TUI wartet 3× maximal 120 s (~6 Minuten) und gibt dann auf,
  obwohl das Kontingent laut Provider erst viel später zurückkommt. Das
  erzeugt genau das gemeldete Bild "sofort/schnell Rate-Limit", weil die
  Retry-Schleife das eigentliche Reset-Fenster nicht respektiert, sondern
  nur kurz pollt.
- Die TUI hat **keinen** Zugriff auf die Information, ob es sich um ein
  Policy-Block (R1) oder ein echtes Kontingent-429 handelt — beide Fälle
  landen im selben `ModelError::RateLimited`.

## R5 — Pacer (`ProviderRateLimiter`) ist für `anthropic` nicht konfiguriert

`harw-config/src/provider_toml.rs:37` (`rate_limit: Option<RateLimitToml>`)
und `providers/anthropic.toml` (kein `[rate_limit]`-Abschnitt, laut Auftrag
bereits verifiziert) → `ProviderRateLimiter::new(None)` →
`enabled = false` (`harw-provider-http/src/rate_limiter.rs:108-118`). Der
Pacer beobachtet zwar `anthropic-ratelimit-*`-Header
(`apply_anthropic_family`, `rate_limiter.rs:153-176`), tut aber nichts damit,
solange `enabled=false`. Proaktives Pacen vor einem harten 429 findet also
für Anthropic aktuell nicht statt.

## R6 — Bestehender Auth-Pfad für Umstieg auf API-Key existiert bereits

`harw-cli/src/auth.rs` (`AuthAction::Token { provider }`, Zeilen 74-100):
`harw auth token anthropic` nimmt bereits einen beliebigen Token/Key
entgegen und speichert ihn; es gibt bereits eine Sonderprüfung für
`openai`-JWT-Fehlnutzung (Zeilen 91-98), aber **keine** entsprechende Prüfung
für `anthropic`, die einen `sk-ant-oat01-…`-Abo-Token von einem echten
`sk-ant-api03-…`-API-Key unterscheidet und den Nutzer warnt/korrigiert.
`warn_anthropic_subscription_token()` (Zeile 78) warnt nur beim
OAuth-Login-Flow, nicht wenn versehentlich ein Abo-Token über `token`
eingegeben wird.
