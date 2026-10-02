# SDK von unten nach oben verdrahten

Status: Entwurf (Draft-PR-Stapel). Stand: `consolidate/main` 2026-10-02.

Dieses Dokument wertet aus, welche **Zyklen** (wiederkehrende Abläufe mit
Zustand) das System heute hat, wo jeder lebt, was `harwness-sdk` davon
freilegt und was fehlt. Daraus folgt eine Reihenfolge: jede Schicht erst, wenn
die darunter trägt, jede als eigener Draft-PR.

Regel der SDK (`harwness-sdk/README.md`): sie ist die **semver-Grenze**. Keine
öffentliche Signatur nennt einen internen `harw-*`-Typ. Alles hier muss an
dieser Grenze in SDK-eigene Typen übersetzt werden.

## 1. Die Zyklen

| # | Zyklus | Ablauf | Wo er lebt (Beleg) |
|---|---|---|---|
| Z1 | **Turn** | `send` → Modellrunden → Werkzeugaufrufe → Pause (Freigabe oder Kind) → Wiederaufnahme (höchstens `MAX_RESUMES_PER_TURN` = 64) → Ende | `harwness-sdk/src/session.rs`; Kern: `harw-core/src/turn_loop.rs` |
| Z2 | **Freigabe** | Turn parkt → dauerhafter Datensatz, an den beim Parken gebundenen Akteur gebunden → Frame `ApprovalRequested` → `approval.respond` (erster Schreiber gewinnt) → Wiederaufnahme **mit dem gebundenen Akteur** (nicht dem Auflöser) → Frame `ApprovalResolved` | Host: `harw-session-host/src/durable_approvals.rs`; Treiber: `harw-session-driver/src/bridge.rs` (Fix `37d55af`); Kern: `harw-core/src/session.rs::resolve_approval`; SDK-Seite nur lokal: `harwness-sdk/src/approval.rs` |
| Z3 | **Anhängen und Wiedergabe** | `session.attach(from?)` → Wiedergabe ab Cursor → Live-Frames mit `Cursor{generation,durable,live}` → bei `Lagged`/`Resync` neu anhängen; bei Verbindungsverlust neu verbinden | `harw-protocol/src/session_wire.rs`; Client `harw-session-remote` (Wiederverbindung S07 liegt auf `ws/s07-reconnect-alias`, noch nicht in `consolidate/main`) |
| Z4 | **Absenden** | `turn.submit{expect_head, client_msg_id, force}` → `Accepted{position}` \| `Stale{head}` \| `QueueFull` \| `Denied`; Idempotenz über `client_msg_id` | Host: `harw-session-host/src/arbiter.rs`; Protokoll: `session_wire.rs::SubmitParams` |
| Z5 | **Container** | Quellen neu prüfen → `create` (liefert die ID) → `inspect(ID)` → Read-back-Urteil → erneut prüfen → `start(ID)` \| `rm(ID)` | `harw-tool-container` (PR #96 auf `dev`, #93 auf `consolidate/main`); noch ohne Ausführer |
| Z6 | **Zusammenbau** | `harw gateway --session-socket` → `Daemon::start` → `CoreTurnDriver::with_factory` → `GatewayCoreFactory` je gehosteter Sitzung | `harw-cli/src/session_serve.rs`, `harw-session-daemon/src/compose.rs` |

Z2 und Z3 sind die, die Einbettende am häufigsten falsch bauen (hängende
Freigabe, doppelte oder verlorene Prompts, veralteter Strom nach `Resync`).
Genau diese Fehler sind in den letzten Wochen gefunden und behoben worden;
die Schicht über dem Protokoll soll sie **einmal** richtig kapseln, statt dass
jeder Client sie neu baut (das Handy-Experiment `harw-mobile-core`, die TUI
`harw attach`, künftig die SDK).

## 2. Was die SDK heute davon freilegt

- Z1 vollständig, aber nur **im Prozess** (`Harwness::builder().build()` →
  `Session::send`). Die Freigabe wird über `ApprovalHandler` beantwortet.
- Z2 nur als lokaler Handler. Die Freigabe über die Control-Plane (anderer
  Prozess, mehrere Geräte, erster Schreiber gewinnt) kennt die SDK nicht.
- Z3, Z4: nicht vorhanden. Es gibt keinen Client für `harw.session.v1`.
- Z5: nicht vorhanden. Es gibt kein Container-Werkzeug in der SDK.
- Ereignisse: `SdkEvent` ist stabil und kennt schon die Übersetzung
  `map_turn_event(source, TurnEvent)` (`harwness-sdk/src/event.rs`). Das ist die
  Naht, an der Frames der Control-Plane zu denselben Ereignissen werden wie
  lokale Turns. Die Planung (`docs/planning/75-harness-patterns/codex.md`)
  sagt dasselbe: keine zweite Ereignis-Enum, sondern übersetzen.

## 3. Schichten, von unten nach oben

| Schicht | Inhalt | Stand |
|---|---|---|
| L0 | `harw-protocol`, `harw-types` (Frames, Cursor, Parameter) | vorhanden |
| L1 | Host, Daemon, Com-Layer, Zusammenbau (Z6), Freigabe-Fix (Z2) | in `consolidate/main` |
| L2 | Client-Transport: `harw-session-remote` (Verbinden, Hello, Anhängen, Frames) | vorhanden; Wiederverbindung S07 offen |
| **L2.5** | **`harw-session-client`**: die Regeln, die jeder Client braucht, einmal (Schlüssel, Wiedergabe-Erkennung, Auswertung von Absenden und Freigabe, Beschreibung einer Freigabe, Turn-Zählung, Zustandswörter); rein und synchron | **Draft-PR #101** |
| **L3** | **SDK: entfernte Sitzungen** (Z2, Z3, Z4 über `harw.session.v1`, Ereignisse als `SdkEvent`), gebaut auf L2.5 | **Draft-PR A (#100)** |
| L4 | SDK: Container-Werkzeug (Z5) als `Tool` | Draft-PR B, braucht erst einen Ausführer |
| L5 | Verbraucher: `harw-mobile-*` (#94), headless-Encoder (R5), `harw attach` | auf L3 umstellen, wenn A trägt |

Wichtig für die Reihenfolge: L3 hängt **nicht** von `harw-mobile-core` ab. Das
ist ein Experiment (#94); eine stabile SDK darf nicht von ihm abhängen. Beide,
SDK und Handy-Client, sitzen auf L2.5 (`harw-session-client`) und L2
(`harw-session-remote`). Der Handy-Client nutzt die gemeinsamen Schlüssel schon
(#94); #94 kann später ganz auf L3 wechseln.

Eine Regel bleibt bewusst getrennt: Die `SessionView` des Handy-Clients wendet
auch Frames an derselben Position an (der Host sendet jede offene Freigabe mit
demselben Stand) und stützt sich auf idempotente Schritte. Ein Client, der auf
**einen bestimmten** Turn wartet (SDK `send`), muss Wiedergabe dagegen
ausblenden (`StreamTracker`). Zwei Regeln, zwei Zwecke.

## 4. Draft-PR-Stapel

1. **PR 0 (dieses Dokument):** Analyse und Reihenfolge. Nur Doku.
1b. **PR #101: `harw-session-client`.** Die gemeinsame Schicht unter SDK und
   Handy-Client. Die Regeln, die jeder Client braucht, standen vorher doppelt
   (SDK-Remote und Handy-Client); jetzt stehen sie einmal, als benannte
   Funktionen mit Tests. Je mehr hier unten verdrahtet ist, desto weniger
   baut jeder Client darüber neu und desto weniger kann auseinanderlaufen.
2. **PR A (#100, Basis #101): `feat(sdk): entfernte Sitzungen über die
   Control-Plane`.** Neues
   Feature `remote` (Vorgabe aus): `RemoteHarwness`, `RemoteSession`,
   `RemoteSessionInfo`. `send` kapselt Z4 und Z2 und Z3; Ereignisse kommen als
   `SdkEvent`. Getestet gegen den echten Daemon über einen Unix-Socket.
3. **PR B: Container als SDK-Werkzeug (Z5).** Erst wenn es einen Ausführer
   gibt, der die `Stage`-Schritte fährt (Prozess starten, Inspect-JSON
   parsen). Bis dahin ist `harw-tool-container` reine Richtlinie.
4. **PR C: Wiederverbindung in `RemoteSession`** (Z3), sobald S07 in
   `consolidate/main` ist; kein eigener Backoff in der SDK.
5. **PR D: Verbraucher umstellen** (Handy-Client, `harw attach`, headless),
   je Verbraucher ein PR, nur wenn A bis C tragen.

Jeder PR gilt erst als fertig, wenn seine Schicht gegen die darunter getestet
ist (echter Daemon, echte Verbindung), nicht nur gegen Attrappen.

## 5. Bekannte Lücken, die die SDK nicht verstecken darf

- Gehostete Sitzungen laufen mit `EntryKind::Tui` (Shell- und Netz-Rechte der
  TUI). Für den lokalen Socket vertretbar, für Remote-Zugriff nicht: eigener
  Entry-Typ oder Ableitung aus `ClientIdentity`, bevor der Node-Listener
  zusammengebaut wird (siehe `harw-cli/src/session_serve.rs`).
- Freigaben über die Control-Plane verlangen das Recht `approve`. Remote-Geräte
  haben es nur per Opt-in. `RemoteSession` fragt den `ApprovalHandler` deshalb
  nur, wenn der Host `approve` erteilt hat; sonst wartet der Turn auf ein
  anderes Gerät. Das ist kein Fehler der SDK, aber sie muss es sagen.
- `SdkEvent` ist absichtlich nicht `Serialize` (`event.rs`); der geplante
  gemeinsame Encoder (R5) liegt in `harw-protocol`.
- Das Fenster zwischen Quellenprüfung und Mount der Engine
  (`harw-tool-container`, `hostpath.rs`) ist von dort aus nicht schließbar.

## 6. Prüfen

Verbindliche Reihenfolge laut `.github/copilot-instructions.md` (bei wenig
Platten: `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`):

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo run -q -p xtask -- gates`
5. `cargo deny check`

Während der Arbeit an einer Schicht genügt es, 1–3 auf die berührten Crates
zu beschränken (`-p <crate>`, bei Clippy `--no-deps`, solange eine
Abhängigkeit wie `harw-ops` ein bekanntes Lint hat); vor dem Übergang aus dem
Entwurf läuft die volle Reihenfolge. L3 zusätzlich ein Ende-zu-Ende-Test gegen
einen echten `harw-session-daemon`.

## 7. Basisbranch

Die Repo-Regel nennt `dev` als Basis. Der Stapel zielt vorerst auf
`consolidate/main`, weil dort die Integration liegt, auf der er aufbaut (`dev`
liegt über 100 Commits dahinter). Sobald `consolidate/main` nach `dev`
gemergt ist, werden die Entwurfs-PRs auf `dev` umgehängt.
