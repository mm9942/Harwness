# Structure Plan — betroffene Module (keine neuen Module nötig)

Dieser Plan erfordert **keine neue Modulstruktur**. Alle Änderungen aus
`parent-tasks.md` betreffen bestehende Dateien:

| Task | Crate | Datei |
|---|---|---|
| T1 | harw-core | `src/model.rs` (`ModelError::is_retryable`) |
| T1 | harw-provider-http | `src/anthropic.rs` (429-Konstruktion), `src/retry.rs` |
| T1 | harw-core | `src/turn_loop.rs` (`transition_after_turn_failure`) |
| T2, T4 | harw-tui | `src/app.rs` (`drive_turn_animated`, Konstanten) |
| T3a | harw-cli | `src/auth.rs` (`token()`) |
| T3b | Konfiguration | `providers/anthropic.toml`, Verdrahtungsstelle
  `AnthropicMessagesProvider::configure_rate_limit` (Aufrufer noch zu
  lokalisieren — vermutlich `harw-runtime/src/assembly.rs`, dort bereits als
  `M` in `git status` markiert, im Umsetzungsauftrag verifizieren) |
| T5 | harw-provider-http | `src/anthropic.rs` (Konstante
  `ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING`) |

Keine neuen Structs/Enums/Traits nötig für T2-T5. T1 erweitert ggf.
`ModelError::Transient` um einen Zuordnungsfall (429 → `Transient` statt
eigener `RateLimited`-Variante) — das ist eine **Migration einer bestehenden
Variante**, keine neue Architektur. Ob `RateLimited` als Variante ganz
entfernt oder nur `is_retryable()` erweitert wird, ist eine Entscheidung für
den Umsetzungsauftrag (Empfehlung: `is_retryable()` erweitern, Variante
belassen — kleinerer Blast-Radius, siehe `test-and-error-plan.md`).
