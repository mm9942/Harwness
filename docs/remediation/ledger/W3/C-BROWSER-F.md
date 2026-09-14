# W3 — Agent C-BROWSER-F: Doctest/Test-Nachzug + `deny_unknown_fields` auf `Selector`/`Target`/`ObservationMode`

Owned: `harw-browser/src/lib.rs` (nur Doctest ~:52-62), `harw-browser/tests/session_lifecycle.rs`
(~:173-195), `harw-browser/src/selector.rs` (`Selector`, `Target`), `harw-browser/src/observation.rs`
(`ObservationMode`), dieses Ledger. Folgeauftrag zu `ledger/W3/C-BROWSER.md`, Abschnitt „Orchestrator /
Eigentümer außerhalb W3/W5-Tabellen" (Punkte `lib.rs:52-62`, `tests/session_lifecycle.rs:173-195`,
`selector.rs`, `observation.rs`).

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/gelinted, kein `make`/`rustc`/
`rust-analyzer`, keine Git-Schreibbefehle, keine Manifest-Änderung. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1` (OK, `serde_json` bereits normale
Dependency von `harw-browser`, nicht nur `dev-dependencies` — Nutzung in `#[cfg(test)]`-Modulen
unproblematisch). Verifikation ausschließlich durch Lesen: Signaturen aus `harw-browser/src/policy.rs`
(`OriginPolicy::new`/`from_origins` → `Result<Self>`, `OriginPolicy::default`, `OpenBrowserRequest`-
Felder inkl. `authentication_origins`/`limits`, `BrowserLimits::default`) gegen den in `C-BROWSER.md`
dokumentierten Vertrag abgeglichen; `policy.rs` selbst **nicht** verändert (dort lag der Doctest
~:1000-1017 bereits auf der neuen API — C-BROWSER hatte ihn schon migriert).

## 1. `harw-browser/src/lib.rs` — Crate-Doctest migriert

`OriginPolicy::new(vec!["example.com".to_owned()], true)` (alte `(Vec<String>, bool) -> Self`-Signatur,
kein `?`) ersetzt durch `OriginPolicy::from_origins(["https://example.com"], true)?` (neue
`Result`-Signatur, Origin jetzt mit Scheme). Ergänzt: `authentication_origins: OriginPolicy::default()`
und `limits: BrowserLimits::default()` als neue Pflichtfelder von `OpenBrowserRequest`; `BrowserLimits`
zum `use harw_browser::policy::{...}`-Import hinzugefügt. Kein Upload/CustomScript im Beispiel
vorhanden — nichts zu entfernen.

## 2. `harw-browser/tests/session_lifecycle.rs` — Migration auf neue Felder/Konstruktoren

- Import: `BrowserLimits` zu `use harw_browser::policy::{...}` ergänzt.
- `let allowed_origins = OriginPolicy::new(vec!["erp.example.com".to_owned()], true);` →
  `OriginPolicy::from_origins(["https://erp.example.com"], true).expect("valid allowed-origins policy");`
  (Test-Funktion hat keinen `Result`-Rückgabetyp, daher `.expect(...)` statt `?`, konsistent mit dem
  übrigen `.expect(...)`-Stil dieser Datei).
- `OpenBrowserRequest`-Literal um `authentication_origins: OriginPolicy::default()` und
  `limits: BrowserLimits::default()` ergänzt.
- `is_allowed`-Aufruf (`open_request.allowed_origins.is_allowed(&open_request.start_url)`) unverändert
  gelassen — Signatur laut `C-BROWSER.md` gleich geblieben, Semantik nur verschärft (Origin jetzt mit
  Scheme `https://erp.example.com` statt bloßem Host, weiterhin erlaubt).
- Keine `Upload`/`CustomScript`-Nutzung in dieser Datei vorhanden — nichts zu entfernen.

## 3. `deny_unknown_fields` auf `Selector`/`Target`/`ObservationMode`

Beide Enums sind extern getaggt (Default-Serde-Repräsentation, kein `tag`/`content`, kein `untagged`),
`#[serde(deny_unknown_fields)]` daher direkt am Enum/Struct — kein separates Attribut je
Struct-Variante nötig: laut `serde_derive-1.0.228/src/de/struct_.rs` (bereits von C-BROWSER verifiziert,
siehe `C-BROWSER.md`-Kopf) gilt Container-`deny_unknown_fields` auch für Struct-Varianten.

- `harw-browser/src/selector.rs`: `#[serde(deny_unknown_fields)]` an `enum Selector` (deckt die
  Struct-Varianten `TagClass { tag, class }` und `Role { role, name }` ab) und an `struct Target`.
- `harw-browser/src/observation.rs`: `#[serde(deny_unknown_fields)]` an `enum ObservationMode` (deckt
  die Struct-Variante `DomSelection { selector }` ab).
- Kein `untagged`, kein `#[serde(other)]`, kein `flatten` eingeführt oder vorgefunden.

### Neue Tests (unbekanntes Feld wird abgelehnt)

- `selector::test_selector_deserialize_rejects_unknown_field_in_struct_variant` — `TagClass` mit
  zusätzlichem Feld `extra` via `serde_json::from_value::<Selector>` → `Err`.
- `selector::test_target_deserialize_rejects_unknown_field` — `Target`-Objekt mit zusätzlichem
  Top-Level-Feld `extra` → `Err`.
- `observation::test_observation_mode_deserialize_rejects_unknown_field_in_dom_selection` —
  `DomSelection` mit zusätzlichem Feld `extra` → `Err`.

Bestehende Serde-Round-Trip-Tests (`selector::test_target_serde_json_round_trip_with_struct_variant_selectors`,
`observation::test_observation_mode_dom_selection_serde_json_round_trip`, `observation::test_browser_observation_serde_json_round_trip`)
unverändert gelassen und durch Lesen gegengeprüft: keiner verwendet unbekannte Felder, alle bleiben mit
`deny_unknown_fields` gültig.

## Verifikation durch Lesen

- `policy.rs:580` (`OriginPolicy::new`), `:616` (`from_origins`), beide `-> Result<Self>`; `:838`
  (`impl Default for BrowserLimits`); `:1018-1029` (`OpenBrowserRequest`-Felder inkl.
  `authentication_origins`, `limits`) — Import- und Feldnamen in `lib.rs`/`session_lifecycle.rs` stimmen
  exakt überein.
- `error.rs:88-99` (`Error::OriginNotAllowed`, `Error::InvalidArgument`), `:162`
  (`impl From<url::ParseError> for Error`) — `?` im Doctest löst weiterhin auf.
- `harw-browser/Cargo.toml:10` (`serde_json = "1.0.150"`, normale Dependency) — neue Tests brauchen
  keinen Manifest-Change.
- Keine `unwrap`/`expect` außerhalb `#[cfg(test)]`/Doctest; kein `unsafe`; keine neuen `#[allow]`;
  keine Änderung an `policy.rs`/`action.rs`/`wait.rs` (dort liegen `Selector`/`Target`/`ObservationMode`
  nicht — nur importiert).

## Folgearbeit (unverändert aus `C-BROWSER.md`, hier nicht berührt)

W5 B-TOOL und W5 B-ADAPT wie in `C-BROWSER.md` beschrieben; diese Datei ergänzt dort nichts Neues.
