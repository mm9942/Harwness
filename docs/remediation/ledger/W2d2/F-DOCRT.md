# W2d-2 — Fix-Agent Z2d-2 / F-DOCRT

Owned files:
- `harw-runtime/src/trace.rs` — nur Doku-Kommentar Zeile 8 (Block erweitert).
- `harw-runtime/src/sandbox.rs` — nur Doku-Kommentar Zeile 5 (Block erweitert).
- `harw-runtime/src/ceiling.rs` — nur Doku-Kommentare Zeile 10, 43, 61.
- `docs/remediation/ledger/W2d2/F-DOCRT.md` (diese Datei, neu).

NICHT angefasst: `harw-runtime/src/assembly.rs`, `harw-runtime/src/spec.rs`
(parallel bei Agent F-RT).

BUILD-POLICY eingehalten: kein `cargo`, kein `make`, keine git-Schreibbefehle.
Nur gelesen/gegrept und in den owned files Kommentare/Doku geändert — keine
Codeänderung. Verifikation ausschließlich durch Lesen + grep.

## Vorab-Grep (gesamter `harw-runtime/src`)

```
grep -rn "root_context\|new_local_root_trace\|build_local_spawn_context\|run_chat_tui\|STARTUP_MODE" harw-runtime/src/
```

Treffer:
- `harw-runtime/src/sandbox.rs:5` — `build_local_spawn_context`
- `harw-runtime/src/trace.rs:8` — `new_local_root_trace`
- `harw-runtime/src/ceiling.rs:10, 43, 61` — `root_context.rs` /
  `local_root_context_ceiling`

Keine Treffer für `run_chat_tui` oder `STARTUP_MODE` in `harw-runtime/src` —
an diesen beiden Stellen gab es nichts zu aktualisieren.

Gegengeprüft (nur gelesen, nicht editiert):
- `harw-cli/src/root_context.rs` existiert nicht mehr (`test -f` → nicht
  gefunden) — die Datei ist tatsächlich in W2d-2 entfernt.
- `harw-runtime/src/assembly.rs` ruft `root_sandbox` (sandbox.rs),
  `root_ceiling` (ceiling.rs) und `new_root_trace` (trace.rs) aus
  `RuntimeAssemblyBuilder::build` auf (Zeilen 797/805/808) — das ist die
  Montage, auf die die neuen Docs verweisen.
- `RuntimeAssembly` ist über `harw-runtime/src/lib.rs:22-24` als
  `pub use assembly::{RuntimeAssembly, ...}` am Crate-Root re-exportiert;
  `[`crate::RuntimeAssembly`]` ist damit ein gültiger Intra-Doc-Link von
  jedem der drei owned Module aus.

## Änderungen

### `trace.rs:8` — `new_local_root_trace`

Der Absatz beschreibt (im Präteritum, "bisher") die vier historischen
Erzeugerstellen. Ergänzt: ein Satz, dass alle vier Altstellen — inklusive
`new_local_root_trace` — seit W2d-2 entfernt sind und ihre Aufrufer jetzt
über die RuntimeAssembly-Montage laufen ([`crate::RuntimeAssembly`]), die
[`new_root_trace`] hier einmal je Lauf zieht.

### `sandbox.rs:5` — `build_local_spawn_context`

Gleiches Muster: nach dem historischen Absatz ergänzt, dass
`build_local_spawn_context` seit W2d-2 entfernt ist und sein Nachfolger
[`root_sandbox`] in diesem Modul ist, aufgerufen aus der
RuntimeAssembly-Montage ([`crate::RuntimeAssembly`]).

### `ceiling.rs:10, 43, 61` — `root_context.rs` / `local_root_context_ceiling`

Drei Stellen verwiesen auf die inzwischen gelöschte Datei
`harw-cli/src/root_context.rs`:
- Zeile 10 (Moduldoku, Aufzählungspunkt): ergänzt, dass die Datei seit
  W2d-2 entfernt ist und die Wurzeldecke seither ausschließlich in
  `ceiling.rs` lebt.
- Zeile 43 (Doku von `LOCAL_ROOT_BUDGET_TOTAL`): ergänzt "(entfernt in
  W2d-2)" hinter dem Dateiverweis.
- Zeile 61 (Doku von `LOCAL_ROOT_SECTIONS`): ergänzt, dass die Datei seit
  W2d-2 entfernt ist und die Wurzeldecke selbst seither ausschließlich hier
  lebt.

Keine der drei Stellen war ein rustdoc-Intra-Doc-Link (`[...]`) auf
`root_context.rs` — es waren Klartext-Codespannen (Backticks) in
historischer Beschreibung. Sie wurden nicht gelöscht (die Historie bleibt
für den G-044/W2b-Kontext wertvoll), sondern um den aktuellen Stand ergänzt,
damit kein Leser die Datei noch für vorhanden hält.

Keine neuen oder bestehenden Intra-Doc-Links (`[`Item`]`-Syntax) in den drei
owned Dateien zeigen auf nicht (mehr) existierende Items — geprüft per
Lesen aller `//!`- und `///`-Blöcke der drei Dateien.

## Vermerkt, nicht geändert (spec.rs — Agent F-RT, nicht owned)

`harw-runtime/src/spec.rs:266f` (Doc-Kommentar von
`RightsSnapshot::config_policy_tools`):

```
/// Werkzeuge, die die Konfigurationspolitik ohne Rückfrage erlaubt.
pub config_policy_tools: Vec<String>,
```

Das ist inhaltlich falsch/verdreht. Gegengeprüft per grep
(`harw-runtime/src/approval.rs:319,567,573`, `config.rs:71`,
`harw-cli/src/chat.rs:866-888`): `config_policy_tools` transportiert genau
die Werkzeuge aus `[policy].require_approval_for` — also Werkzeuge, für die
die Konfigurationspolitik eine **Rückfrage verlangt**, nicht solche, die
sie ohne Rückfrage erlaubt. Der Kommentar sollte etwa lauten: "Werkzeuge,
für die die Konfigurationspolitik (`require_approval_for`) eine Rückfrage
verlangt." Da `spec.rs` bei Agent F-RT liegt (parallel, nicht owned von
F-DOCRT), hier nur vermerkt, nicht geändert.
