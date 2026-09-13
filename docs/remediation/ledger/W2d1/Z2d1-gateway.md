# Z2d-1 Review Gateway (read-only, opus) — Verdict: needs-debug

| ID | Schwere | Ort | Befund | Fix |
|---|---|---|---|---|
| G1 | blocker | runtime_gateway.rs:65-66 | `GatewayEntry::Dream` nur in Tests konstruiert → dead_code unter -D warnings (Binary-Crate) | gateway.rs::run baut zweite Assembly `GatewayEntry::Dream` (Principal `channel_principal(Dream,"")`, Dream-State-Store, gleicher Resolver) und gibt deren model() an dream_scheduler |
| G2 | major | gateway.rs:1626-1645, runtime_gateway.rs:217-240 | kein Regressionstest für secrets:-Provider über Resolver | Test mit Temp-Home + KEK (`write_gateway_test_kek`/`SecretStore::create`): Mount scheitert nicht an sealed secret; Negativtest KEK fehlt → Err "gateway: enabled sealed-secret provider requires a configured KEK" |
| G3 | minor | gateway.rs:342,576-581 | Dream nutzt Telegram-Assembly (Audit/Trace ununterscheidbar) | durch G1 |
| G4 | minor | gateway.rs:436-444 | Doppel-Laden der Config; EntryKind hart `GatewayTelegram` | `entry.entry_kind()` verwenden; Resolver-Öffnung in gateway_assembly verlegen |
| G5 | minor | gateway.rs:70-73 | Doku "Fehler" unvollständig (Provider/Trust/KEK/cwd); Audit-Scheduler läuft bei Mount-Fehler nicht | Doku ergänzen |
| G6 | minor | gateway.rs:432 | Principal `telegram:gateway` erfundener Peer | als Daemon-Platzhalter dokumentieren (P1.6) |
| G7 | minor | gateway.rs:227-250,1455-1466 | Assembly nur Config/Provider-Fabrik; Turns ohne new_root_session | Folgearbeit W4a A-GW |
| G8 | ok | gateway.rs:1004-1185 | W1-12/Z1-F1 intakt | – |
