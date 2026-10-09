# h17-Verifikationsbericht — Lens-Indizes

**Datum:** 2026-10-08
**Abweichung:** Geplant war `research/h17-index-verify.md`; das Verzeichnis `research/` existiert nicht und `fs.write` legt Verzeichnisse nicht an. Daher liegt dieser Bericht hier unter `docs/plans/h17-index-verify-report.md`.

## Zweck

Sechs Integrationstests halten die Verifikationspflicht für Lens-Indizes fest:

1. Ein gebauter Index antwortet auf Abfragen (`build` → `ask` liefert Treffer).
2. Der Index-Status verrät, ob ein Index gebaut ist.
3. Rebuilds **ersetzen** Inhalte, statt Einträge anzuhängen (keine Duplikate, keine Altlasten).
4. Nicht-UTF-8-Pfade brechen Build und Query nicht (robuste Handhabung).
5. Parallele Queries gegen denselben Index sind fehlerfrei (keine Races, keine Korruption).
6. Out-of-Scope-Selektoren bleiben unter Parallelität als `IndexNotVisible` **abgelehnt** —
   d. h. es wird ein Fehler geliefert, **nie** `Ok(vec![])`.

## Abdeckungsliste (6 Tests)

| # | Testname | Verpflichtung |
|---|----------|---------------|
| 1 | `test_index_verify_build_then_ask_returns_hits` | gebauter Index antwortet mit Treffern |
| 2 | `status_reflects_built_index` | Status spiegelt den gebauten Index wider |
| 3 | `rebuild_updates_hits` | Rebuild ersetzt Inhalte statt anzuhängen |
| 4 | `non_utf8_document_path_is_handled` | Nicht-UTF-8-Pfade via `OsString::from_vec` (kein `unsafe`) handhabbar |
| 5 | `parallel_queries_same_index` | parallele Queries auf demselben Index fehlerfrei |
| 6 | `parallel_query_out_of_scope_is_rejected` | Out-of-Scope unter Parallelität → Fehler (`IndexNotVisible`), nie `Ok(vec![])` |

## Anker

- **facade.rs, use-Erweiterung:** Zeile 22
- **Einfügepunkt:** vor `test_ask_computes_nothing_itself`
- **Vorlage:** `docs/plans/h17-facade-test-insert.md`

## Verifikationsplan

- `cargo metadata` zur Prüfung der Target-Zugehörigkeit ist **jetzt zulässig**.
- Die zentrale Testausführung (z. B. `cargo test`) erfolgt **nach DEC-004 erst nach Abschluss
  aller Plan-Knoten** — angestoßen durch die Nutzerschnittstelle/Claude Code, nicht durch
  diesen Bericht.
