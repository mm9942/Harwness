# Z2d-1 Review TUI (read-only, opus) — Verdict: needs-debug

| ID | Schwere | Ort | Befund | Entscheidung / Fix |
|---|---|---|---|---|
| T1 | blocker (Build-Ende) | runtime_commands.rs:50,67 | `caller_tier`, `slash_service_map` pub(crate) ohne Prod-Aufrufer | Vertrag für W2d-2/D5 (app.rs verdrahtet); bleibt bis dahin, IA prüft |
| T2 | major | tools_command.rs:284-285,473-491 | `/tools reset` / `reset <name>` kann über Basis hinaus erweitern | F-T: reset stellt `ceiling` wieder her; reset <name> übernimmt Tool-Zustand aus ceiling |
| T3 | major | runtime_commands.rs:146, tools_command.rs:458 | `on`/`profile` prüfen nur Basis, nicht Basis∩Modus | F-C: `AgentSession::mode_ceiling()`; F-T: bounded-Pfad validiert/schneidet gegen ceiling |
| T4 | major (Tracking) | app.rs:2707 | Prod nutzt noch unbeschränktes `dispatch_tools_command` | W2d-2/D5 |
| T5 | minor | session_controller.rs:210-214 | Semantik korrekt; abhängig von T3/T4 | Doku `/mode <gleich>` in W2d-2 |
| T6 | minor | command_exec.rs:99-100 | Rustdoc beschreibt alten Shell-Text | F-T |
| T7 | minor | tools_command.rs:457-459 | Bestätigung `profile set to X` obwohl geschnitten | F-T |
| T8 | minor | tools_command.rs:234,399 | Intra-Doc-Link auf pub(crate)-Item | F-T |
| T9 | minor | tools_command.rs Tests | keine Tests reset/Modus-Grenze | F-T |
