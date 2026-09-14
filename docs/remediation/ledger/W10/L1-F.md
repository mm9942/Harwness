# W10-L1-F – Aufrufer von `FlatIndex::build`/`FlatIndex::load` nach der
Dimensionsprüfung (Folgeauftrag zu W9-C4-W10-L1)

Rolle: focused-coding-task (Sonnet). Kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/
`rust-analyzer`, keine `git`-Schreibbefehle. Verifikation durch Lesen.

Pflichtlektüre: `docs/remediation/AGENT-BRIEF.md`; Ledger `docs/remediation/ledger/W9/W9-C4-W10-L1.md`
(§3 „API-Änderungen und externe Aufrufer" — dort bereits der Caller-Grep dieses Knotens vorbereitet).
Zusätzlich gelesen (read-only, außerhalb Owned): `harw-lens-source/src/error.rs` (Bestätigung der
`#[from] Index(harw_lens_index::IndexError)`-Variante), `harw-lens-index/src/flat.rs` (aktuelle Signaturen
von `build`/`load`), `harw-macros/src/error.rs` + `harw-macros/src/lib.rs` (Bestätigung: `#[from]` erzeugt
einen echten `impl From<Inner> for Enum`, `?` funktioniert also ohne `map_err`), `harw-lens-query/src/resolve.rs`,
`harw-lens/src/lib.rs`, `harw-lens-index/src/lib.rs`, `harw-lens-query/src/lib.rs`.

## 0. Ausgangslage

`FlatIndex::build` liefert seit W9-C4-W10-L1 `Result<Self, IndexError>` statt `Self`
(`harw-lens-index/src/flat.rs:201`). `FlatIndex::load` liefert **bereits vor** diesem Knoten
`Result<Self, IndexError>` (I/O-bedingt) — die neue Dimensionsprüfung fügt dort nur eine zusätzliche
`IndexError`-Variante zu einem bereits vorhandenen `Result`-Rückgabetyp hinzu, ändert also die *Form* des
Rückgabetyps an keiner Aufrufstelle. Migration war daher ausschließlich für `FlatIndex::build`-Aufrufer nötig.

## 1. Vollständiger Caller-Grep (`FlatIndex::build`, `FlatIndex::load`, workspace-weit)

| Fundstelle | Art | Migration nötig? |
|---|---|---|
| `harw-lens-source/src/build.rs:418` | Produktionscode, `FlatIndex::build(...)` | Ja — **behoben** (§2) |
| `harw-lens-source/src/build.rs:835,898,933` | Testcode, `FlatIndex::load(...).expect(...)` | Nein — `load` war schon `Result`, `.expect` bereits vorhanden und weiterhin korrekt |
| `harw-lens-query/src/query.rs:370` | Test-Helfer `build_flat_index(...) -> FlatIndex`, letzter Ausdruck `FlatIndex::build(...)` | Ja — **behoben** (§2), Owned laut Brief („nur Test-Helfer") |
| `harw-lens-query/src/resolve.rs:89` | Produktionscode, `FlatIndex::load(&store, ...)?` | Nein — `load`-Rückgabetyp unverändert, `?` bereits korrekt |
| `harw-lens-query/src/query.rs:68` | Moduldoc (`//!`), echter ```rust-Fence-Doctest, `let index = FlatIndex::build(manifest, vec![...]);` ohne `?`/`.expect` | **Nicht behoben** — außerhalb Owned-Scope, siehe §3 |
| `harw-lens-query/src/lib.rs:92` | Moduldoc (`//!`), echter ```rust-Fence-Doctest, identisches Muster wie oben | **Nicht behoben** — Datei nicht Owned, siehe §3 |
| `harw-lens-index/src/lib.rs:62` | Moduldoc, bereits an neue Signatur angepasst (Vorknoten) | Kein Handlungsbedarf (in `harw-lens-index`, nicht Owned, aber bereits korrekt) |
| `harw-lens-index/src/flat.rs` (mehrere) | Definition + eigene Tests/Doctests | Kein Handlungsbedarf (in `harw-lens-index`, nicht Owned, bereits vom Vorknoten korrekt umgestellt) |
| `harw-lens/src/lib.rs:82` | reine Prosa-Erwähnung des Methodennamens, kein Code | Kein Handlungsbedarf |
| `docs/remediation/ledger/W9/W9-C4-W10-L1.md` | Ledger-Text, kein Code | Kein Handlungsbedarf |

## 2. Geänderte Dateien (Owned)

| Datei | Änderung |
|---|---|
| `harw-lens-source/src/build.rs:418` | `let index = FlatIndex::build(manifest.clone(), entries);` → `let index = FlatIndex::build(manifest.clone(), entries)?;`. Propagiert über die bereits vorhandene `#[from] Index(harw_lens_index::IndexError)`-Variante in `SourceError` (`harw-lens-source/src/error.rs:124-125`) — kein `map_err` nötig, `?` konvertiert automatisch, weil `harw_macros::HarwError`s `#[from]`-Attribut einen echten `impl From<harw_lens_index::IndexError> for SourceError` erzeugt (verifiziert in `harw-macros/src/error.rs:192` `from_impl` und `harw-macros/src/lib.rs:109`). Die umgebende Funktion `build_visibility_bucket` gibt bereits `SourceResult<IndexBuildReport>` zurück und nutzt `?` an mehreren anderen Stellen derselben Funktion — kein Signaturwechsel nötig, minimaler Diff. |
| `harw-lens-query/src/query.rs:370` (Test-Helfer `build_flat_index`, `#[cfg(test)] mod tests`) | `FlatIndex::build(manifest(), entries)` → `FlatIndex::build(manifest(), entries).expect("test embeddings share a single dimension")`. `.expect()` statt Umbau der Helfer-Signatur (Vorgabe des Auftrags), da die Fixture ausschließlich mit einem `DeterministicEmbedder` fester Dimension arbeitet — ein Dimensionsfehler an dieser Stelle wäre immer ein Testaufbaufehler, kein erwarteter Fall. Stil konsistent mit den bestehenden `.expect("deterministic embedder never fails")` / `.expect("one vector")` in derselben Funktion. |

## 3. Nicht behoben (außerhalb Owned-Scope, Folgearbeit)

Zwei echte (nicht `no_run`/`ignore` markierte) ```rust-Doctests enthalten weiterhin
`let index = FlatIndex::build(manifest, vec![(chunk, embedding)]);` ohne `?`/`.expect` und binden das
Ergebnis anschließend per `&index` an `query(...)`, das `&FlatIndex` (über `VectorIndex`) erwartet:

- `harw-lens-query/src/query.rs:68` (Moduldoc von `query.rs` — **nicht** Teil des mir zugewiesenen
  „nur Test-Helfer"-Scopes derselben Datei; der Brief grenzt meine Schreibrechte in dieser Datei
  ausdrücklich auf den Test-Helfer ein, nicht auf die ganze Datei).
- `harw-lens-query/src/lib.rs:92` (andere Datei, nicht in meiner Owned-Liste).

Beide sind Kompilierfehler bei `cargo test --doc -p harw-lens-query` bzw. `cargo doc --no-deps`, sobald
der Workspace wieder baubar ist (siehe unten). Bereits im Vorknoten-Ledger als Verdachtsfall vermerkt
(„vermutlich unkritisch … vom nächsten Bearbeiter zu verifizieren") — hiermit verifiziert: **es sind
echte Doctests, keine reine Prosa**, und sie brechen. Empfohlene Korrektur für den nächsten Bearbeiter
(gleiches Muster wie in `build.rs`/`query.rs` hier verwendet): Doctest-Codeblock um
`# Ok::<(), Box<dyn std::error::Error>>(())`-Konvention erweitern und `FlatIndex::build(...)?` schreiben
(Muster bereits vorhanden im selben `query.rs`-Moduldoc-Block, dort aber bei `save`/`load`-Beispielen mit
`,no_run` — bei `query.rs:68`/`lib.rs:92` fehlt sowohl `?` als auch `,no_run`).

**Blockierender Nebenbefund (aus dem Vorknoten-Ledger übernommen, weiterhin gültig):** `cargo metadata`
schlägt workspace-weit fehl, weil das Workspace-Mitglied `harw-egress` in der Root-`Cargo.toml` gelistet
ist, das Verzeichnis aber nicht existiert. Dieser Knoten hat daher — wie vorgeschrieben — ausschließlich
per Lektüre verifiziert, nicht per `cargo check`.

## 4. Verifikation (durch Lesen, kein Build erlaubt)

- `harw-lens-source/src/build.rs`: `build_visibility_bucket` gibt `SourceResult<IndexBuildReport>`
  zurück (Zeile 341); `?` an der geänderten Zeile ist syntaktisch identisch zu den übrigen `?`-Aufrufen
  in derselben Funktion (z. B. `store.has_chunk(&chunk.digest)?`, `embedder.embed(&new_texts)?`).
  `SourceError::Index` (`error.rs:124-125`) ist als `#[from]`-Variante deklariert → `From`-Impl existiert,
  `?` löst auf.
- `harw-lens-query/src/query.rs`: `build_flat_index` ist die letzte Ausdrucksform einer Funktion mit
  Rückgabetyp `FlatIndex` (Zeile 357); `.expect(...)` liefert `FlatIndex`, Typ stimmt weiterhin.
  Funktion liegt innerhalb `#[cfg(test)] mod tests` (Testcode) — `.expect()` in Tests ist laut Brief §4
  erlaubt.
- Keine weiteren Aufrufer von `FlatIndex::build` im Workspace gefunden (vollständiger Grep, §1).

## 5. Ergebnis

Nicht BLOCKED. Beide Owned-Aufrufer (`harw-lens-source/src/build.rs:418`,
`harw-lens-query/src/query.rs:370`) migriert; vollständiger Caller-Grep dokumentiert; zwei nicht in
meinem Scope liegende Doctest-Bruchstellen (`harw-lens-query/src/query.rs:68`, `harw-lens-query/src/lib.rs:92`)
identifiziert und für den nächsten Bearbeiter konkretisiert statt stillschweigend übergangen.
