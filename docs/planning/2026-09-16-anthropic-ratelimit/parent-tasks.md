# Parent Tasks — Anthropic-Rate-Limit-Sonderplan (backlog-ready)

Reihenfolge = Empfehlung (siehe `planning-summary.md`). Jede Task ist
eigenständig durch `debug-orchestrator`/`development-orchestrator` umsetzbar
und referenziert konkrete Dateien/Zeilen.

## T1 — `RateLimited` in den generischen Retry-Vertrag migrieren (Code-Bug-Fix)
- **Dateien**: `harw-core/src/model.rs:658-660` (`is_retryable`),
  `harw-provider-http/src/anthropic.rs:764-774` (Konstruktion),
  `harw-provider-http/src/retry.rs` (`retry_decision`, Tests Zeile 452-491),
  `harw-core/src/turn_loop.rs:1606-1649` (`transition_after_turn_failure`,
  nutzt `RateLimited`-Spezialprüfung — muss synchron mitgezogen werden),
  `docs/remediation/ledger/W3/C-MODEL.md` (offene Annahme 2 schließen).
- **Ziel**: Ein Anthropic-429 im generischen `RetryingProvider`-Pfad (z. B.
  CLI/Headless) löst tatsächlich einen Retry aus, statt sofort aufzugeben.
- **Abhängigkeit**: keine (reiner Bugfix bestehender, dokumentierter
  Diskrepanz). Höchste Priorität, weil er unabhängig von OAuth-vs-API-Key
  in jedem Fall die "sofort kein Retry"-Symptomatik im Nicht-TUI-Pfad behebt.

## T2 — TUI-Retry-Fenster an reale Reset-Zeiten anpassen
- **Dateien**: `harw-tui/src/app.rs:3460-3465` (`RATE_LIMIT_AUTO_RETRY_CAP_SECS`,
  `RATE_LIMIT_MAX_ATTEMPTS`), `3588-3617` (Retry-Schleife).
- **Ziel**: Unterscheiden zwischen kurzem RPM/ITPM-429 (Sekunden,
  `retry-after` klein) und langem Abo-Kontingent-429 (`retry_after_secs`
  groß, ggf. Minuten/Stunden) — bei letzterem nicht sinnlos 3× kappen und
  aufgeben, sondern dem Nutzer die tatsächliche Wartezeit klar kommunizieren
  (Kopplung an T4).
- **Abhängigkeit**: T1 (gemeinsame Fehlersemantik), T4 (UX-Text).

## T3a — Option (a): Wechsel auf echten API-Key ermöglichen/absichern
- **Dateien**: `harw-cli/src/auth.rs:74-100` (`token()`-Funktion — Analogie
  zur bestehenden OpenAI-JWT-Fehlnutzungsprüfung Zeile 91-98, aber für
  Anthropic: erkennen, ob eingegebener Wert nach `sk-ant-oat01-…` (Abo-Token)
  aussieht statt `sk-ant-api03-…` (echter Key), und warnen/hart ablehnen),
  `providers/anthropic.toml` (Doku-Kommentar, wie auf `auth = "env:..."` oder
  einen zweiten Key-Pfad umgestellt wird).
- **Ziel**: Nutzer bekommt einen sicheren, geführten Pfad, um von Abo-OAuth
  auf einen Console-API-Key zu wechseln, inklusive Erkennung/Warnung bei
  Verwechslung.
- **Abhängigkeit**: keine; unabhängig von T1/T2 umsetzbar.

## T3b — Option (b): `[rate_limit]` für Anthropic aktivieren
- **Dateien**: `providers/anthropic.toml` (neuer `[rate_limit]`-Abschnitt,
  `enabled = true`, sinnvoller `safety_margin_pct`), Verdrahtungspunkt, an
  dem `ProviderToml.rate_limit` an `AnthropicMessagesProvider::configure_rate_limit`
  übergeben wird (Konstruktionsstelle des Providers finden — nicht Teil
  dieser reinen Planungsrecherche, im Umsetzungsauftrag zu lokalisieren).
- **Ziel**: `ProviderRateLimiter` pace proaktiv basierend auf beobachteten
  `anthropic-ratelimit-*`-Headern, bevor ein hartes 429 auftritt.
- **Abhängigkeit**: keine; kann parallel zu T3a laufen.
- **Hinweis**: wirkt nur gegen echte RPM/ITPM/OTPM-429s, NICHT gegen ein
  Policy-Block-429 (R1) oder ein erschöpftes 5h-Abo-Kontingent ohne
  Vorab-Header-Signal.

## T3c — Option (c): differenziertere Retry-Strategie für `RateLimited`
- Deckt sich inhaltlich mit T1 (Migration zu `Transient{status:Some(429)}`)
  plus einer Erweiterung: bei sehr langem `retry_after_secs` (> aktuelles
  `max_retry_after`) bewusst `GiveUp` mit einer aussagekräftigen Meldung statt
  eines stillen Abbruchs — Kopplung an T4.
- **Dateien**: siehe T1, zusätzlich `harw-provider-http/src/retry.rs:171`
  (`max_retry_after`-Schwelle, aktuell 60 s Default — für Anthropic-Abo-Fälle
  ggf. ein anthropic-spezifischer, höherer Wert nötig).

## T4 — UX: klare, handlungsleitende Meldung bei Anthropic-Rate-Limit/Policy-Block
- **Dateien**: `harw-tui/src/app.rs:3608-3617` (aktuelle Meldung
  `"⏱ Rate limit — provider busy; retry in {}s ({} attempts used)"`),
  ggf. neue Fallunterscheidung: liegt ein Abo-OAuth-Credential vor
  (`AnthropicCredential::OAuth`) und ist `retry_after_secs` groß → Hinweis
  "Abo-Kontingent gerade ausgeschöpft — nutzt du parallel Claude Code? Warte
  oder wechsle auf einen API-Key (`harw auth token anthropic`)."
- **Abhängigkeit**: T1 (verlässliche Fehlersemantik), T3a (Ziel-Handlungsempfehlung
  muss existieren, bevor man den Nutzer dorthin verweist).

## T5 — Warnhinweis-Text/Kommentar aktualisieren (Doku-Korrektur, kein Verhalten)
- **Dateien**: `harw-provider-http/src/anthropic.rs:53-72`
  (`ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING`) — Text nennt eine "pausierte
  Juni-2026-Abrechnungsänderung"; laut Recherche (siehe
  `dependency-research.md`, R1) ist die Durchsetzung seit April 2026 bereits
  aktiv scharf. Text sollte diese Verschärfung wiedergeben, damit Nutzer die
  Warnung nicht als "vorerst entschärft" missverstehen.
- **Abhängigkeit**: keine; unabhängig, aber inhaltlich verknüpft mit T3a/T4.

## Nicht Teil dieses Plans
- Kein `database-plan.md` — dieser Auftrag betrifft kein persistentes
  Datenmodell/Schema; alle betroffenen Zustände (Rate-Limit-Header,
  Retry-Zähler) sind bereits laufzeitinterner In-Memory-Zustand.
