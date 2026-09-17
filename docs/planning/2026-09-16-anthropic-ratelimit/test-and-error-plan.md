# Test- und Fehlerplan — Anthropic-Rate-Limit-Sonderplan

## Fehlerbild-Matrix (für den Umsetzungsauftrag)

| Ursache | Erkennbar an | Aktuelles Verhalten | Soll-Verhalten (nach Empfehlung) |
|---|---|---|---|
| Policy-Block (Abo-OAuth für Drittanbieter gesperrt) | 429 sofort, ggf. ohne `retry-after`, evtl. beim allerersten Request | `ModelError::RateLimited`, TUI versucht 3× bis 120s, dann Fehlermeldung | Nutzer erkennt an T4-Meldung, dass dies kein normales Kontingent-429 ist; Empfehlung zu API-Key |
| Echtes RPM/ITPM/OTPM-429 (API-Key oder Abo) | `retry-after` klein (Sekunden), Anthropic-Header vorhanden | im TUI-Pfad: retried bis 3×/120s (meist ausreichend); im generischen `RetryingProvider`-Pfad: **kein** Retry (Bug, T1) | nach T1: auch im generischen Pfad retried |
| Erschöpftes 5h-Abo-Kontingent | `retry_after_secs` groß (Minuten-Stunden) | TUI kappt auf 120s, gibt nach 3 Versuchen auf | nach T2/T4: sofortige klare Meldung mit echter Wartezeit statt sinnlosem Kurz-Retry |
| Spend-Cap (nur bei echtem API-Key relevant) | 429 ohne `retry-after`, `error_code: enforced_spend_limit_reached` | `parse_retry_after`-Fallback-Verhalten ungeprüft | im Umsetzungsauftrag: Verhalten von `parse_retry_after` bei fehlendem Header explizit testen |

## Für den Umsetzungsauftrag vorgeschlagene Tests (nicht in diesem
Planungsschritt geschrieben — Rust-Test-Policy verlangt Delegation an
`rust-test-designer`)

- `test_model_error_is_retryable_table` (`harw-core/src/model.rs`) um Fall
  `RateLimited` → `true` erweitern (aktuell explizit `false` getestet,
  Zeile 1078-1081 — muss mitgeändert werden, sonst bricht der bestehende
  Test).
- `test_retry_decision_table` (`harw-provider-http/src/retry.rs:452-491`):
  `RateLimited` aus der `GiveUp`-Erwartungsliste entfernen bzw. in eine neue
  `Retry`-Erwartung verschieben.
- Neuer Test für `harw-tui/src/app.rs`: `retry_after_secs` oberhalb eines
  Schwellwerts (z. B. > `RATE_LIMIT_AUTO_RETRY_CAP_SECS * RATE_LIMIT_MAX_ATTEMPTS`)
  löst sofort die neue klare Meldung aus, statt drei nutzlose Warteschleifen
  zu durchlaufen.
- Neuer Test für `harw-cli/src/auth.rs`: Eingabe eines Strings mit Präfix
  `sk-ant-oat01-` im `token()`-Pfad für `anthropic` erzeugt eine Warnung
  analog zur bestehenden OpenAI-JWT-Prüfung.
- Bestehende Tests in `harw-provider-http/src/rate_limiter.rs` bleiben
  unverändert gültig (T3b ändert nur Konfiguration, nicht den Pacer-Code).

## Fehlerbehandlung-Policy (Anschluss an bestehende Projektregeln)

- Keine neuen `unwrap()`/`expect()` in Produktionscode.
- `ModelError`-Erweiterung bleibt im bestehenden Hand-Error-Enum-Stil (kein
  `thiserror`/`anyhow`), passend zu den globalen Rust-Vorgaben.
- Jede neue `Err(...)`-Stelle, falls im Umsetzungsauftrag eine neue
  Fehlervariante nötig wird, geht über `rust-error-designer`
  (Enforcement aus globalen Anweisungen), nicht über manuelles Schreiben.
