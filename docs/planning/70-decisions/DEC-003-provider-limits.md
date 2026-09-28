---
id: DEC-003
title: Provider-Limits respektieren statt umgehen
status: accepted
date: 2026-09-27
tags: [decision, work-driver, rate-limits, provider]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-config/src/provider_toml.rs
  - ../../harw-provider-http/src/rate_limiter.rs
  - ../../harw-provider-http/src/retry.rs
  - ../../harw-plan-bridge/src/work_driver.rs
---

# DEC-003 — Provider-Limits respektieren statt umgehen

## Entscheidung
Der Work Driver hält sich an die vom Nutzer konfigurierten Provider-Grenzen
(`max_concurrency`, `rate_limit.max_concurrent`) und an die vom Provider zur
Laufzeit gemeldeten Kontingente (Rate-Limit-Header, HTTP 429). Alle Worker
eines Providers teilen sich dessen In-Process-Instanz inklusive
`ProviderRateLimiter`; die tatsächlich zulässige Parallelität einer Welle
wird zusätzlich über `effective_parallel` in `WorkDriveInput` gedeckelt. Ein
HTTP 429 ist kein Worker-Fehler, sondern ein Warte-Signal mit eigenem
Backoff-Budget.

## Warum
- TPM-/RPM-Kontingente sind vom Nutzer vertraglich vereinbarte Grenzen; sie
  großzügig zu interpretieren oder durch mehrere Provider-Instanzen zu
  umgehen, verletzt die ToS und wirkt de facto wie eine DDoS gegen den
  eigenen Account.
- `max_concurrency` (Provider-weit) und `rate_limit.max_concurrent`
  (Budget-Bucket je Modell) sind unabhängige, gleichzeitig geltende
  Grenzen — die kleinere gewinnt; keine dieser Grenzen darf durch parallele
  Work-Driver-Wellen überschritten werden.
- Ein proaktiver Pacer (`ProviderRateLimiter`) ist billiger als reaktives
  429-Handling: er liest die vom Provider gemeldeten Kontingent-Header und
  wartet kurz, bevor ein Kontingent aufgebraucht ist, statt erst gegen die
  Wand zu laufen.
- 429 bedeutet „warte etwas“, nicht „der Auftrag ist gescheitert“; ein
  eigenes Warte-Budget (Default 5 min, exponentieller Backoff bis 60 s)
  verhindert sowohl vorzeitiges Aufgeben als auch endloses Warten.
- `effective_parallel` bildet stets das Minimum aus Spec-Limit und dem vom
  Aufrufer gemeldeten Provider-Limit — der Work Driver kann eine Welle also
  nicht breiter fahren, als der Provider es zulässt, selbst wenn die Spec
  mehr Parallelität erlauben würde.

## Folgen
- Jeder Provider-Client wird pro Prozess genau einmal instanziiert und von
  allen Workern gemeinsam genutzt; kein Worker darf einen eigenen,
  ungedrosselten Client aufmachen.
- Wellen-Planung muss `effective_parallel` vor dem Enqueue kennen, sonst
  droht Überzeichnung des Provider-Kontingents durch gleichzeitig
  gestartete Worker.
- Trade-off: striktes Respektieren der Limits kostet Durchsatz gegenüber
  einem aggressiveren Scheduling — akzeptiert, weil Kontingentüberschreitung
  harte Fehler, Sperren oder Mehrkosten provoziert.
- Zukünftige Provider-Integrationen müssen ihre Rate-Limit-Header-Familie
  (Anthropic-, OpenAI/DashScope-Stil, `retry-after`) an `ProviderRateLimiter`
  anschließen, sonst bleibt der Pacer für sie wirkungslos und es bleibt nur
  reaktives 429-Backoff.

## Umsetzung R15
Proaktives Pacing läuft jetzt über die Trait-Methode
`ModelProvider::pacing_wait()` mit Default `None`, sodass bestehende
Provider unverändert bleiben. HTTP-Provider melden das Maximum aus der
Wartezeit ihres `ProviderRateLimiter` (Kontingent-Header bzw.
429-Cooldown) und der Vorschau des konfigurierten Budgets
(`preview_wait`). Wrapper-Provider reichen den Wert durch, der Router fragt
seinen Default-Backend-Provider. Der Work Driver pausiert vor jedem
Wellen-Chunk um die gemeldete Wartezeit, statt erst auf ein 429 zu
reagieren.

## Wo im Code
- `harw-config/src/provider_toml.rs` — `ProviderToml::max_concurrency`,
  Validierung gegen `max_concurrency = 0`; `rate_limit.max_concurrent`.
- `harw-provider-http/src/rate_limiter.rs` — `ProviderRateLimiter`,
  `wait_for_slot`, `wait_for_slot_with_estimate`, `observe_headers`.
- `harw-provider-http/src/retry.rs` — `RetryPolicy::rate_limit_budget`,
  `rate_limit_backoff_delay`, `rate_limit_decision`, `ModelError::RateLimited`.
- `harw-plan-bridge/src/work_driver.rs` — `WorkDriveInput::effective_parallel`.
- `harw-core/src/model.rs` — `ModelProvider::pacing_wait` (Default `None`).

## Verwandt
- [DEC-004 Keine parallelen Builds](DEC-004-no-parallel-builds.md)
- [DEC-005 Kleine Scopes, viele Wellen](DEC-005-small-scopes-waves.md)
- [DEC-007 Worker-Rechte](DEC-007-worker-rights.md)
