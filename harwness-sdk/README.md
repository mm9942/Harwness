# harwness-sdk

Stabile, dokumentierte API, um Harwness in eigene Programme einzubetten.

Die SDK ist die **semver-Grenze** vor den internen `harw-*`-Crates: keine
öffentliche Signatur nennt einen internen Typ. JSON-Nutzdaten laufen über
`serde_json::Value` (re-exportiert als `harwness_sdk::serde_json`).

## Schnellstart

```rust,no_run
use harwness_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let harwness = Harwness::builder().cwd(".").build()?;
    let mut session = harwness.session()?;
    let report = session.send("Welche Tests gibt es hier?").await?;
    println!("{}", report.text.unwrap_or_default());
    Ok(())
}
```

Voraussetzungen: eine Tokio-Runtime und ein eingerichteter Root-Space
(`~/.harw` bzw. `HARW_HOME`) mit aktiver UIA und konfiguriertem Provider —
`harw` richtet beides beim ersten Start ein. Für Tests ohne Netz:
`HarwnessBuilder::offline_echo("…")`.

## API-Überblick

| Baustein | Zweck |
|---|---|
| `Harwness::builder()` → `HarwnessBuilder` | `home`, `cwd`, `model`, `provider`, `mode`, `reasoning_effort`, `agent`, `approval_policy`, `approval_handler`, `tool`, `context_source`, `ephemeral`, `scaffold_home`, `offline_echo` |
| `HarwnessBuilder::build()` | prüft Eingaben, lädt Konfiguration (mit Repo-Vertrauensprüfung), prüft UIA/Provider, öffnet den Verlaufsspeicher |
| `Harwness::session()` / `Harwness::resume(&id)` | neue bzw. gespeicherte Sitzung |
| `Session::send(text)` → `TurnReport` | fährt einen Turn inkl. Freigaben und Kind-Agenten zu Ende |
| `Session::events()` → `EventStream` | `SdkEvent`s: `TurnStarted`, `TextDelta`, `ReasoningDelta`, `Message`, `ToolCall`, `ToolResult`, `ChildSpawned`, `ChildCompleted`, `Usage`, `Context`, `Error`, `Finished`, `Lagged` |
| `Session::cancel_handle()` → `CancelHandle` | laufenden Turn von außen abbrechen |
| `Session::history()`, `Session::total_usage()` | Verlauf und Nutzung |
| `Tool`, `FnTool`, `ToolContext`, `ToolError` | eigene Werkzeuge (JSON rein, JSON raus) |
| `ContextSource`, `ContextItem` | eigener Kontext je Turn (Vertrauensklasse „Daten“) |
| `ApprovalHandler`, `approval_fn`, `AutoDeny`, `Decision`, `ApprovalPolicy` | Freigaben; Vorgabe `AutoDeny` + `Delegated` |
| `SdkError` | ein Fehlertyp, `#[non_exhaustive]` |

## Freigaben

Die Runtime hält Werkzeugaufrufe an, die ihre Politik nicht ohne Rückfrage
durchlässt (`ApprovalPolicy::Delegated`: Veränderndes und Ausführendes;
`AlwaysAsk`: alles; `FullAccess`: nichts). `Session::send` legt jede solche
Anfrage dem `ApprovalHandler` vor. Ohne eigenen Handler lehnt `AutoDeny`
ab; ohne Antwort innerhalb des konfigurierten Freigabe-Timeouts und bei
Abbruch des Turns wird ebenfalls abgelehnt.

## Entfernte Sitzungen (Feature `remote`)

Ein `harw gateway --session-socket` hostet Sitzungen. Mit dem Feature `remote`
verbindet sich die SDK damit und liefert dieselbe Fläche wie lokal: ein
`TurnReport`, dieselben `SdkEvent`, derselbe `ApprovalHandler`.

```rust,no_run
use harwness_sdk::prelude::*;
use harwness_sdk::RemoteHarwness;

# async fn demo() -> Result<(), SdkError> {
let harwness = RemoteHarwness::builder()
    .socket("/run/user/1000/harw/session.sock")
    .approval_handler(approval_fn(|r| async move {
        if r.tool.starts_with("fs.read") { Decision::Approve } else { Decision::deny("nein") }
    }))
    .turn_timeout(std::time::Duration::from_secs(600))
    .connect()
    .await?;
let mut session = harwness.create_session(Some("Demo")).await?;
let report = session.send("Welche Tests gibt es hier?").await?;
println!("{}", report.text.unwrap_or_default());
# Ok(()) }
```

`send` kapselt, was Clients sonst falsch bauen: den Idempotenzschlüssel eines
Prompts (überlebt einen Neustart), den erneuten Versuch bei veraltetem Stand,
das Beantworten der Freigabe, das Neuanhängen nach `Lagged`/`Resync` und das
Ausblenden von Wiedergabe-Frames.

Die Regeln dahinter stehen einmal in `harw-session-client` (Schlüssel,
Wiedergabe-Erkennung, Auswertung von Absenden und Freigabe, Turn-Zählung);
SDK, Handy-Client und TUI setzen darauf auf, statt sie je neu zu bauen.

Zwei Dinge, die die SDK nicht verstecken kann:

- Der **Host** entscheidet, wer freigeben darf. Hat er `approve` nicht erteilt
  (`RemoteSession::may_approve`), fragt `send` den Handler nicht; der Turn
  wartet auf ein anderes Gerät. Dafür gibt es `turn_timeout`.
- `send` setzt voraus, dass kein anderer Client gleichzeitig Turns derselben
  Sitzung fährt. Eine verlorene Verbindung beendet `send` mit einem Fehler;
  eine automatische Wiederverbindung gibt es noch nicht.

## Beispiele

```text
cargo run -p harwness-sdk --example minimal_chat -- "Was liegt hier?"
cargo run -p harwness-sdk --example streaming_events -- "Analysiere src/"
cargo run -p harwness-sdk --example custom_tool
```

## Features

- `browser` (Vorgabe): Browser-Werkzeuge der Wurzelsitzung, falls konfiguriert.
- `unstable-internals`: rohe Erweiterungspunkte (`raw_tool_provider`,
  `raw_context_provider`, `raw_model_provider`, `raw_secret_resolver`) —
  **ohne** semver-Zusage.

## Grenzen (Stand dieser Fassung)

- Der Einstieg nutzt das interaktive Runtime-Profil (`EntryKind::Tui`); eine
  aktive UIA ist deshalb Pflicht.
- Konfiguration wird aus den Dateien des Root-Space und des Projekts geladen;
  In-Memory-Overrides gibt es nur für Modell, Provider, Modus, Effort, Agent
  und Freigabepolitik.
- `auth = "secrets:…"` braucht einen Resolver (nur über
  `unstable-internals`).
- Verschachtelte Pausen von Kind-Agenten (eigene Freigaben/Übergaben) treibt
  die SDK nicht; das Kind endet dann mit einem Fehlerergebnis.
