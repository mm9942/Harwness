# Z2d-1 Review Web/Jobs/Runtime (read-only, opus) — Verdict: needs-debug

| ID | Schwere | Ort | Befund | Entscheidung / Fix |
|---|---|---|---|---|
| W1 | blocker | runtime_entry.rs:32 | `harw_protocol` nicht in harw-cli/Cargo.toml | behoben durch Orchestrator (`cargo add -p harw-cli --path harw-protocol`) |
| W2 | blocker (Build-Ende) | runtime_entry.rs:163, runtime_jobs.rs:57,73,93,142,188 | `build_assembly`, `job_assembly`, `job_principal`, `submitter_is_configured`, `JobEntry::{Prompt,entry_kind}` ohne Prod-Aufrufer | Vertrag für W2d-2/D3b (job_worker über job_assembly); IA prüft |
| W3 | major | job_worker.rs:375-446,792-923 | Jobs laufen ohne RuntimeAssembly/Job-Principal | W2d-2/D3b |
| W4 | minor | web.rs:236-237 | zweite Root-Sandbox-Bindung an cwd statt project_root | F-W: `assembly.sandbox().clone()` |
| W5 | minor | spec.rs:119,179 | Web-Decke {R} gilt für alle Tiers | Entscheidung: {R} bleibt Decke bis W5/WB-COMP; Owner-Schreibrechte im Web nicht benötigt |
| W6 | minor | web.rs:512-665 | kein Test Routen `/api/approval-*` aus Assembly | F-W |
| W7 | minor | assembly.rs:1586 | Testname verspricht Weitergabe | F-M: umbenennen |
| W8 | minor | web.rs:239-243 | erstes Send+Sync-Erfordernis RuntimeAssembly | F-M: Compile-Zeit-Assertion in assembly.rs |
| W9 | minor | fmt | Zeilen >100 | cargo fmt in finaler Phase (rustfmt fehlt auf Pi) |
| W10 | minor | web.rs:188-190 | Doppel-Laden Config | Folgearbeit Runtime (W6 I-CONTRIB) |
| W11 | minor | job_worker.rs:1309-1316 | Prüfung vor Knotenart-Schnitt, fail-closed | F-M: dokumentieren |
| W12 | minor | runtime_entry.rs:94 / chat.rs:832 | Duplikat sessions_root | W2d-2/D6 |
