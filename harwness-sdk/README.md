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
