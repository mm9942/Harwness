# F-M — W7, W8, W11 (Z2d-1-Review: assembly.rs, job_worker.rs)

Welle W2d-1, Remediation `/home/mia/projects/harwness`. Rolle: focused-bug-fix
(Sonnet, mechanisch/dokumentarisch — Test würde jeden Fehler sofort fangen).
Build-Policy eingehalten: **nichts kompiliert/getestet**, nur
`cargo metadata --offline --no-deps --format-version 1` (Exit 0). Keine
git-Schreibbefehle.

Owned files: `harw-runtime/src/assembly.rs` (nur die genannten Stellen),
`harw-cli/src/job_worker.rs` (nur Kommentar an der Vertragsforderungsprüfung),
`docs/remediation/ledger/W2d1/F-M.md` (neu).

## W7 — Testname `assembly.rs`

Befund (Z2d1-web-jobs.md, Zeile W7): `test_builder_secret_resolver_is_used_for_configured_model`
(`harw-runtime/src/assembly.rs:1586`) prüft nur `builder.secret_resolver.is_some()`
— also dass `RuntimeAssemblyBuilder::secret_resolver(..)` das Feld setzt — nicht,
dass der Resolver tatsächlich für ein `ModelSource::Configured`-Modell
*verwendet* wird (das würde `RuntimeStores` + ggf. `SessionManager` brauchen,
siehe C2a.md, außerhalb der Read-list/owned files).

Fix: umbenannt in `test_builder_secret_resolver_sets_field`. Assertion und
Testkörper unverändert (keine Logikänderung). Doc-Kommentar über dem Test um
einen Absatz ergänzt, der die Umbenennung mit Bezug auf W7 begründet.

Verifikation:
- `grep -rn "test_builder_secret_resolver_is_used_for_configured_model" --include="*.rs"` →
  keine Treffer mehr (Test war nirgends sonst per Name referenziert, z. B. kein
  `#[test]`-Aufruf von außen, kein String-Match in anderen Modulen).
- `grep -rn "test_builder_secret_resolver_sets_field" --include="*.rs"` → genau
  ein Treffer, die neue Definition.
- Alter Name bleibt in `docs/remediation/ledger/W2d1/C2a.md` stehen (Ledger
  dürfen den alten Namen behalten, siehe Auftrag) — Ledger nicht verändert.

## W8 — Compile-Zeit-Test `Send + Sync` für `RuntimeAssembly`

Befund (Z2d1-web-jobs.md, Zeile W8): `web.rs:239-243` ist die erste
Aufrufstelle, die `RuntimeAssembly` über eine `Send + Sync`-Grenze trägt, ohne
dass es je einen Compile-Zeit-Beleg dafür gab, dass der Typ diese Auto-Traits
hält.

Fix: neuer Test im bestehenden `mod tests` (`use super::*;` bereits vorhanden,
Zeile 1391 — `RuntimeAssembly` ist damit ohne weiteren Import erreichbar), ganz
am Ende des Moduls ergänzt, exakt wie im Auftrag vorgegeben:

```rust
#[test]
fn test_runtime_assembly_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuntimeAssembly>();
}
```

Kein Produktionscode geändert. Reiner Compile-Zeit-Test (kein Assert zur
Laufzeit nötig — `assert_send_sync::<RuntimeAssembly>()` schlägt beim
Kompilieren fehl, falls `RuntimeAssembly` künftig ein `!Send`/`!Sync`-Feld
bekommt).

Verifikation:
- `grep -n "mod tests" harw-runtime/src/assembly.rs` → Zeile 1390, `use super::*;`
  auf der Folgezeile bestätigt.
- `grep -n "^pub struct RuntimeAssembly" harw-runtime/src/assembly.rs` → Zeile
  977, im selben Modul wie `mod tests` — über `use super::*;` sichtbar.
- Test manuell gelesen: kein `unwrap()`/`expect()`, keine Netz-/Prozess-
  Abhängigkeit, deterministisch, eine konkrete Aussage (Typ ist `Send + Sync`).

## W11 — Dokumentations-Kommentar `job_worker.rs`

Befund (Z2d1-web-jobs.md, Zeile W11; vertieft in B2.md): die Prüfung
„Vertragsforderung ⊆ geerbte Sandbox" in `derive_plan_node_sandbox`
(`harw-cli/src/job_worker.rs:1309-1316`, der `ceiling`-Block) läuft bewusst vor
dem Knotenart-Schnitt (`job_sandbox`/`PlanNodeKind`-Tabelle, Zeilen 1318-1322)
und ist fail-closed: ein Research-Knoten mit `allowed_paths` unter einer nur
lesenden geerbten Sandbox scheitert hier mit einem Fehler, statt später still
auf `{Read}` zurückgeschnitten zu werden.

Fix: reiner Doc-Kommentar unmittelbar vor dem `ceiling`-Block eingefügt (vor
`let ceiling = contract_permission_ceiling(contract);`, unmittelbar nach dem
`leaves_workspace`-Block). Keine Codeänderung, keine Verhaltensänderung.

Verifikation: Datei gelesen, Kommentar sitzt exakt an der im Auftrag genannten
Stelle (~1309 vor der Änderung; die Funktion selbst — Reihenfolge
`leaves_workspace` → Vertragsforderung ⊆ inherited → `job_sandbox` — ist
unverändert gegenüber B2.md's Beschreibung).

## Verifikation gesamt

- `cargo metadata --offline --no-deps --format-version 1` — Exit 0.
- Keine Signatur, kein Testkörper (außer Umbenennung W7), kein Produktionspfad
  in `job_worker.rs` verändert.
- `assembly.rs`: Testmodul-Struktur (`mod tests { use super::*; ... }`)
  unverändert bis auf die zwei beschriebenen Stellen (W7-Umbenennung + W8-Test
  angehängt).

## Stubbed imports

Keine.

## Annahmen

Keine über die in C2a.md/B2.md bereits dokumentierten hinaus.
