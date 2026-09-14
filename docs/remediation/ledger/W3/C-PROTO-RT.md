# W3 — Agent C-PROTO-RT / Wurzeldecke auf `harw_context::ceiling::ROOT_CONTEXT_SECTIONS`

Owned: `harw-runtime/src/ceiling.rs`, dieses Ledger.
Nicht angefasst: `assembly.rs`, `spec.rs`.
BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet, keine git-Schreibbefehle, keine Manifest-Änderung.
Verifikation ausschließlich durch Lesen + grep + `cargo metadata --offline --no-deps --format-version 1`.

## Auftrag

Folgearbeit aus `docs/remediation/ledger/W3/C-PROTO.md` §5.5 (F-163): die Wurzeldecke in
`harw-runtime/src/ceiling.rs` führte bisher ein crate-privates Duplikat der Sektionsliste
(`LOCAL_ROOT_SECTIONS`, vier Sektionen, ohne `legacy.v1`). Die eine Quelle ist seit W3 C-PROTO
`harw_context::ceiling::ROOT_CONTEXT_SECTIONS` (fünf Sektionen, inkl. `legacy.v1`,
`harw-context/src/ceiling.rs:93-99`). `harw-runtime/src/ceiling.rs` sollte darauf umgestellt werden.

## Vorab-Prüfung: Dependency

`harw-runtime/Cargo.toml:8` führt bereits `harw-context = { path = "../harw-context" }` als
Dependency (Zeile davor kommentiert für einen anderen Zweck, aber der Eintrag selbst ist
bestehend und ungebunden nutzbar). Der bisherige Code importierte bereits
`harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass}` — die Abhängigkeit
war also schon vorhanden. **Kein dep-request nötig.**

Gegengeprüft mit `cargo metadata --offline --no-deps --format-version 1`: `harw-context` erscheint
als Dependency von `harw-runtime` im Metadata-Graphen.

`harw-context/src/lib.rs:82` exportiert `pub mod ceiling;`, `harw-context/src/ceiling.rs:93` führt
`pub const ROOT_CONTEXT_SECTIONS: &[&str] = &[...]` (inkl. `LEGACY_V1_SECTION`) — der Pfad
`harw_context::ceiling::ROOT_CONTEXT_SECTIONS` ist also von `harw-runtime` aus erreichbar.

## Änderungen (`harw-runtime/src/ceiling.rs`)

1. **Import**: neue Zeile `use harw_context::ceiling::ROOT_CONTEXT_SECTIONS;` (Z. 51) neben dem
   bestehenden `use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};`.
2. **Entfernt**: die Konstante `LOCAL_ROOT_SECTIONS: [&str; 4]` samt ihres Doc-Blocks
   (ehemals Z. 58-87) — das crate-private Duplikat existiert nicht mehr.
3. **`root_ceiling`** (`CeilingPolicy::LocalRoot`-Zweig): `sections` wird jetzt aus
   `ROOT_CONTEXT_SECTIONS.iter().copied().map(SectionName::try_new).collect::<Result<_,_>>()`
   gebaut statt aus `LOCAL_ROOT_SECTIONS.into_iter()...` — gleiches Muster (`.expect(...)` über
   alle Namen auf einmal), nur andere Quelle. Verhalten sonst unverändert: `max_trust`
   weiterhin `TrustClass::Instruction`, Budget weiterhin `LOCAL_ROOT_BUDGET_TOTAL` (unverändert,
   nicht Teil dieses Auftrags).
4. **Moduldoku** (Kopf, `# Bekannte Lücke`, `# Fehler`): aktualisiert — verweist jetzt auf
   `harw_context::ceiling::ROOT_CONTEXT_SECTIONS` als die eine Quelle, mit einem neuen Absatz
   "Seit C-PROTO-RT (W3)", der F-163 und die Behebung der Duplizierung nennt. Historischer
   Kontext (zwei Wurzeldecken vor W2b, `harw-cli/src/root_context.rs`, `harw-tui`) unverändert
   stehen gelassen — nur ergänzt, nicht gelöscht (gleiches Prinzip wie F-DOCRT).
5. **`root_ceiling`-Doc-Kommentar**: Verweis von `LOCAL_ROOT_SECTIONS` (siehe dort) auf
   `[`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`]` umgestellt.

## Test (~:190, jetzt `local_root_ceiling_carries_exactly_root_context_sections_including_legacy_v1`)

Umbenannt von `local_root_ceiling_carries_history_tail_and_the_three_task_sections`, da er jetzt
mehr prüft als der alte Name sagt. Verschärft laut Auftrag ("prüft, dass die Decke genau
`ROOT_CONTEXT_SECTIONS` (inkl. `legacy.v1`) zulässt"):

- `assert_eq!(ceiling.sections, expected)` mit `expected` = `ROOT_CONTEXT_SECTIONS` als
  `BTreeSet<SectionName>` — exakte Mengengleichheit statt nur "enthält" (strenger als vorher).
- Expliziter `assert!` auf `harw_context::ceiling::LEGACY_V1_SECTION` (neu — das ist der
  eigentliche Verhaltensunterschied: v1-Fragmente sind jetzt zulässig).
- Bestehende Assertions (`HISTORY_TAIL_SECTION`, `task.objective`, `task.read_scope`,
  `new.trigger_return`, Längenvergleich, `max_trust`, Budget) beibehalten, Längenvergleich auf
  `ROOT_CONTEXT_SECTIONS.len()` statt `LOCAL_ROOT_SECTIONS.len()` (jetzt 5 statt 4).

`local_root_ceiling_excludes_credentials_and_foreign_transcripts` unverändert — `legacy.v1` steht
nicht in der Verbotsliste, kein Konflikt.

`closed_is_a_reduction_of_local_root` und `every_entry_gets_exactly_one_of_the_two_ceilings`
unverändert — beide vergleichen nur gegen das Ergebnis von `root_ceiling`, nicht gegen die
Sektionsliste selbst.

## Verhaltensänderung (für Integrations-Audit)

Jede lokal-vertraute Wurzelsitzung (`CeilingPolicy::LocalRoot`) lässt ab hier zusätzlich die
Sektion `legacy.v1` zu (`TrustClass::Data`-Fragmente aus der v1-Kontext-Brücke,
`harw-extension-api/src/v1_compat.rs`). Das behebt F-163: v1-Fragmente wurden bisher in jeder
Kindsitzung als `BelowCeiling` verworfen, weil keine Wurzeldecke die Sektion führte, mit der die
Brücke sie stempelt. Kein anderer Unterschied — `max_trust`, Budget, die übrigen vier Sektionen
identisch zu vorher.

## dep-request

Keine — `harw-context` war bereits Dependency von `harw-runtime`.

## Nicht angefasst (außerhalb dieses Auftrags)

- `LOCAL_ROOT_BUDGET_TOTAL` (Budgetherleitung, P1.2-Folgearbeit laut Kommentar).
- `harw-tui/src/app.rs:1364` (`local_tui_root_context_ceiling`) — eigene, separate
  Wurzeldecken-Implementierung außerhalb `harw-runtime`, nicht Teil des Briefs.
- `harw-runtime/src/assembly.rs`, `harw-runtime/src/spec.rs` — explizit nicht owned.
