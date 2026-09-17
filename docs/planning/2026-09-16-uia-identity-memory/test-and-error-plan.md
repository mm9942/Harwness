# Test- und Fehlerplan (für den Umsetzungsauftrag)

## Loader (`harw-config/src/loader.rs`)
- `test_load_uia_identity_missing_file_returns_empty`
- `test_load_uia_identity_present_returns_content`
- `test_load_uia_identity_rejects_path_traversal` (Analogie zu bestehenden
  `configured_file_path`-Tests)
- `test_load_uia_personalization_includes_identity_fragment_when_present`
- `test_load_uia_personalization_omits_identity_fragment_when_absent`
- Fehlerpfad: `ConfigError::Invalid` bei Traversal, `ConfigError::ReadFailed`
  bei nicht-lesbarer Datei — beide bereits vorhandene Varianten, keine
  neue Error-Variante nötig.

## `agent_definition_tools.rs`
- `test_build_agent_toml_no_longer_embeds_identity_field` (Regression
  gegen die dokumentierte Abweichung).
- `test_commit_uia_bundle_writes_identity_md_when_present`
- `test_commit_uia_bundle_omits_identity_md_when_absent`
- `test_propose_uia_includes_identity_in_diff`
- Neuer `ToolProvider` für `uia_self.update_document`:
  - `test_update_document_rejects_non_uia_role`
  - `test_update_document_rejects_secret_pattern`
  - `test_update_document_writes_identity_target`
  - `test_update_document_writes_user_target`
  - `test_update_document_writes_personality_target`
  - `test_update_document_never_escapes_agent_dir`
  - `test_update_document_not_in_auto_approved_tools` (Policy-Level-Test,
    analog zu bestehenden `AUTO_APPROVED_TOOLS`-Coverage-Tests in
    `harw-registry-defaults/src/lib.rs`)

## `uia_bootstrap.rs`
- Bestehende Datei-Set-Assertion (Zeile ~637) um `"identity.md"` erweitern.
- `test_write_generated_uia_includes_identity_md`

## Pro-Agent-Memory-Root
- `test_agent_memory_root_isolated_from_global_root` — zwei
  `FileMemoryStore`-Instanzen mit unterschiedlichen Agent-Roots dürfen sich
  nicht überschneiden (Regressionsschutz gegen versehentliches Teilen
  einer globalen Root).
- `test_project_date_layout_creates_expected_path` — für die neue
  Rohnotiz-Organisationsschicht (`warm/<projekt>/<datum>/NN-<slug>.md`).
- `test_consolidation_promotes_daily_notes_to_memory_md` (sofern M1 der
  Umsetzung diese Schicht schon einschließt; sonst als Folgeauftrag
  markieren).

## Integrationstest
- Ein Integrationstest, der ein vollständiges UIA-Bundle
  (`definition.toml`, `agent.toml`, `identity.md`, `Personality.md`,
  `USER.md`, `memory/`) end-to-end über `agents.write_uia` →
  `agents.commit_proposal` erzeugt und anschließend über
  `load_uia_identity`/`load_uia_personalization` und
  `FileMemoryStore::open` wieder liest — schließt den Kreis
  Schreiben→Lesen, den die heutige `identity_md`-Abweichung offen lässt.

## Fehlerbehandlung (Leitplanken für die Umsetzung)
- Kein `unwrap()`/`expect()` in Produktionscode (globale Vorgabe).
- Keine neue Error-Variante erzwingen, wo bestehende (`ConfigError::Invalid`,
  `ConfigError::ReadFailed`, `MemoryError::*`) ausreichen — nur bei echtem
  neuen Fehlerfall (z. B. "Ziel ist keine UIA-Rolle") eine neue Variante mit
  vollem Kontext hinzufügen.
- Rust-Auto-Agenten-Pflicht bei Umsetzung: `rust-test-designer`,
  `rust-doc-writer`, ggf. `rust-error-designer` (nur falls `error.rs`
  tatsächlich angefasst wird) parallel spawnen, sobald die betroffenen
  Rust-Dateien geschrieben sind und `cargo check` grün ist.
