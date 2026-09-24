//! `/model` operation — zeigt aktives Modell und ermöglicht Operator-seitigen Wechsel.
//!
//! # Verantwortungsbereich
//! Liest den Live-Laufzeitzustand via [`harw_operations::SessionController::snapshot`]
//! und validiert Modell-IDs gegen den konfigurierten Modellkatalog.
//! Stellt Sub-Kommandos bereit, mit denen ein **Operator** (Mensch) das aktive Modell
//! einsehen, auflisten und über den [`harw_operations::SessionController`] wechseln kann.
//!
//! # Sicherheitsregel — kein ModelTool
//! Das laufende Sprachmodell darf sein eigenes Inference-Backend **niemals** selbst
//! wechseln.  Ein solcher Mechanismus würde Prompt-Injection-Angriffe ermöglichen,
//! die das Backend still austauschen.  Daher gilt:
//! - Diese Operation ist `permission = "operator"` und `visibility = "tui_only"`:
//!   erreichbar ausschließlich über die vom Menschen bediente TUI.
//! - Es darf **kein** `ModelTool`-Variant erstellt werden, der diesen Code-Pfad
//!   über eine automatische Tool-Call-Kette aufruft.
//! - `switch` löst den konfigurierten Provider des Ziel-Modells auf und delegiert
//!   vollständig an [`crate::provider::handle_switch_core`] (Welle 2, 2d) — das
//!   wechselt Provider+Modell **atomar** in einem Aufruf, auch wenn das
//!   Ziel-Modell zu einem anderen Provider gehört als der aktuell aktive.
//!   Gibt [`harw_operations::OpError::Execution`] zurück, wenn der Controller
//!   fehlt oder die Mutation scheitert.
//!
//! # Runtime-Wahrheit vs. Config-Default
//! `show` und `list` lesen zuerst den Live-Controller-Snapshot.  Nur wenn kein
//! Wert gesetzt ist, fällt der Pfad auf Config-Defaults zurück — mit klarem Label.
//!
//! # Schlüsseltypen
//! - [`ModelArgs`] — geparste Sub-Kommando-Argumente
//!
//! # Nebenläufigkeit
//! Der Handler ist `async`, führt jedoch keinen konkurrenten I/O aus.
//! Er ist `Send + Sync`-kompatibel.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: `SessionController` nicht verfügbar.
//! - [`harw_operations::OpError::InvalidArguments`]: Unbekanntes Sub-Kommando,
//!   unbekannte Modell-ID, oder der aufgelöste Zielprovider ist unbekannt,
//!   deaktiviert oder ohne Zugangsdaten (siehe [`crate::provider::handle_switch_core`]).
//!
//! # Spec
//! harwness Plan v2 — `/model`-Operation, Tasks A–E; Welle 2 (2d) — atomarer
//! Provider+Modell-Wechsel via Delegation an `harw-ops::provider`.
//!
//! # Beispiel
//! ```no_run
//! // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt nutzbar.
//! ```

use harw_macros::operation;
use harw_operations::session_control::UiaSelection;
use harw_operations::{OpContext, OpError, OpOutput};

// Öffentlicher Re-Export: `crate::config_util` ist `pub(crate)`, deshalb ist
// dieser `pub use` der öffentliche Pfad, über den `harw-tui`-Tests (und
// jeder andere Downstream-Crate) `SelectionPersistence` und
// `RecordingSelectionPersistence` erreichen — ohne `harw-ops/src/lib.rs`
// ändern zu müssen. Siehe `crate::config_util`-Moduldoc für den Trait selbst.
pub use crate::config_util::{
    FileSelectionPersistence, RecordedSelectionPersistCall, RecordingSelectionPersistence,
    SelectionPersistence,
};

/// Argumente für die `/model`-Operation.
///
/// # Beschreibung
/// Trägt den optionalen Sub-Kommando-String, den der Operator eingegeben hat.
/// Die Runtime deserialisiert die rohe TUI-Eingabe in dieses Struct, bevor
/// der Handler aufgerufen wird.
///
/// # Felder
/// - `action` (`Option<String>`): Eines von `"show"` (Standard), `"list"` oder
///   `"switch <model-id>"`.  Bei Abwesenheit fällt die Operation auf `"show"` zurück.
///
/// # Design-Doc-Referenz
/// `/model`-Operationsspezifikation — Args-Abschnitt.
#[derive(Default, serde::Deserialize)]
pub struct ModelArgs {
    /// Primäres Token: `"show"`, `"list"`, oder `"switch"`.
    ///
    /// Wird aus der TUI-Eingabe deserialisiert.  `None` wird als `"show"` behandelt.
    #[serde(default)]
    pub action: Option<String>,

    /// Optionales Ziel-Argument (z. B. Modell-ID bei `switch`).
    ///
    /// Zweites Token bei multi-Wort-Eingaben wie `/model switch <id>`.
    #[serde(default)]
    pub target: Option<String>,

    /// Reserviertes drittes Token-Feld (für zukünftige Verwendung).
    #[serde(default)]
    pub value: Option<String>,
}

impl harw_operations::FromRawArgs for ModelArgs {
    /// Parst rohe Token-Liste zu [`ModelArgs`].
    ///
    /// # Beschreibung
    /// Weist das erste Token `action` zu, das zweite `target`, das dritte `value`.
    /// Dadurch ist `switch <id>` nahtlos darstellbar ohne Join-Trick.
    ///
    /// # Fehler
    /// Nie — gibt immer `Ok`.
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Ok(Self {
            action: tokens.first().cloned(),
            target: tokens.get(1).cloned(),
            value: tokens.get(2).cloned(),
        })
    }
}

// ── Interne Hilfsfunktionen ───────────────────────────────────────────────────

/// Formatiert das `show`-Sub-Kommando mit Live-Laufzeitzustand.
///
/// # Beschreibung
/// Prüft zuerst den Controller-Snapshot für `active_model` und `active_provider`.
/// Fällt auf Config-Defaults zurück — mit klarem Label — wenn kein Live-Wert gesetzt ist.
///
/// # Argumente
/// - `snap` (`&harw_operations::SessionControlSnapshot`): Live-Zustand aus dem Controller.
/// - `config` (`&harw_config::ResolvedConfig`): Geladene Config (Fallback-Quelle).
///
/// # Rückgabe
/// Fertig formatierter `String` für [`OpOutput::text`].
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Zustandslos; sicher von mehreren Threads aus aufrufbar.
///
/// # Spec
/// harwness Plan v2 — Task A.
fn format_show(
    snap: &harw_operations::SessionControlSnapshot,
    config: &harw_config::ResolvedConfig,
) -> String {
    let model_line = match &snap.active_model {
        Some(id) => {
            // Optionaler Display-Name aus dem konfigurierten Katalog.
            let display = configured_model(config, id)
                .and_then(|model| model.name.as_deref())
                .map(str::to_owned)
                .unwrap_or_else(|| id.clone());
            format!("Modell   : {display} [{id}]  (live)")
        }
        None => {
            let cfg = config
                .harness
                .default_model
                .as_deref()
                .unwrap_or("(nicht gesetzt)");
            format!("Modell   : {cfg}  (default from config, not yet switched)")
        }
    };

    let provider_line = match &snap.active_provider {
        Some(id) => format!("Provider : {id}  (live)"),
        None => {
            let cfg = config
                .harness
                .default_provider
                .as_deref()
                .unwrap_or("(nicht gesetzt)");
            format!("Provider : {cfg}  (default from config, not yet switched)")
        }
    };

    format!("{model_line}\n{provider_line}")
}

/// Formatiert das `list`-Sub-Kommando kataloggestützt und provider-bewusst.
///
/// # Beschreibung
/// Enumeriert den konfigurierten Modellkatalog. Für jeden Eintrag wird geprüft,
/// ob er mit dem aktiven Provider kompatibel ist.
/// Das aktive Modell wird mit `*` markiert.  Mehr als 20 Einträge → kompaktes
/// Tabellenformat.
///
/// # Argumente
/// - `snap` (`&harw_operations::SessionControlSnapshot`): Live-Zustand.
/// - `config` (`&harw_config::ResolvedConfig`): Fallback-Config.
///
/// # Rückgabe
/// Fertig formatierter `String`.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Zustandslos; sicher von mehreren Threads aus aufrufbar.
///
/// # Spec
/// harwness Plan v2 — Task B.
fn format_list(
    snap: &harw_operations::SessionControlSnapshot,
    config: &harw_config::ResolvedConfig,
) -> String {
    let active_model = snap
        .active_model
        .as_deref()
        .or(config.harness.default_model.as_deref());

    let active_provider = snap
        .active_provider
        .as_deref()
        .or(config.harness.default_provider.as_deref());

    let mut models: Vec<_> = config.models.values().collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    let total = models.len();

    let mut lines: Vec<String> = Vec::with_capacity(total + 4);

    // Header
    let provider_label = active_provider.unwrap_or("(none)");
    let model_label = active_model.unwrap_or("(none)");
    lines.push(format!("Active model    : {model_label}"));
    lines.push(format!("Active provider : {provider_label}"));
    lines.push(String::new());

    if total > 20 {
        // Compact table: current, provider, model-id, compat
        lines.push(format!(
            "{:<2}  {:<14}  {:<38}  {}",
            "  ", "PROVIDER", "MODEL-ID", "COMPAT"
        ));
        lines.push("-".repeat(72).to_string());
        for model in &models {
            let marker = if active_model.is_some_and(|active| {
                configured_model(config, active).is_some_and(|configured| configured.id == model.id)
            }) {
                "*"
            } else {
                " "
            };
            let compat = match active_provider {
                None => "unfiltered",
                Some(provider) if model.provider == provider => "compatible",
                _ => "other-provider",
            };
            lines.push(format!(
                "{:<2}  {:<14}  {:<38}  {}",
                marker, model.provider, model.id, compat
            ));
        }
    } else {
        // Verbose list
        lines.push("Catalog:".to_owned());
        for model in &models {
            let marker = if active_model.is_some_and(|active| {
                configured_model(config, active).is_some_and(|configured| configured.id == model.id)
            }) {
                " *"
            } else {
                "  "
            };
            let compat = match active_provider {
                None => "unfiltered",
                Some(provider) if model.provider == provider => "compatible",
                _ => "other-provider",
            };
            lines.push(format!(
                "{marker} {} / {}  [{compat}]",
                model.provider, model.id
            ));
        }
        lines.push("  (* = current active model)".to_owned());
    }

    lines.join("\n")
}

/// Resolves a model ID or configured alias from the runtime configuration.
///
/// The resolved config is the operation layer's catalog authority.  The static
/// bootstrap catalog intentionally does not participate here: it can advertise
/// a model that the configured provider does not expose.
///
/// `pub(crate)` so [`crate::provider::handle_switch`] can validate an
/// optional `/provider switch <provider> <model>` model argument against the
/// same catalog authority instead of duplicating the lookup.
pub(crate) fn configured_model<'a>(
    config: &'a harw_config::ResolvedConfig,
    requested_id: &str,
) -> Option<&'a harw_config::ModelToml> {
    config.models.values().find(|model| {
        model.id == requested_id || model.aliases.iter().any(|alias| alias == requested_id)
    })
}

/// Liefert den effektiven UIA-Zustand aus Live-Auswahl und UIA-Config.
///
/// Generische Live-Felder und `default_*` sind absichtlich kein Fallback: Die
/// UIA-Auswahl muss unabhängig vom generischen Controller bleiben.
pub(crate) fn effective_uia_selection(
    controller: Option<&harw_operations::SharedSessionController>,
    config: &harw_config::ResolvedConfig,
) -> UiaSelection {
    let live = controller
        .map(|controller| controller.uia_selection())
        .unwrap_or_else(UiaSelection::empty);

    // Eine nicht-leere Live-Auswahl ist atomar. Insbesondere bedeutet
    // `provider: Some(..), model: None`, dass ein inkompatibler alter
    // Modell-Pin beim Providerwechsel bewusst gelöscht wurde — sie darf nicht
    // achsenweise aus der Konfiguration wieder ergänzt werden.
    if live.is_empty() {
        UiaSelection::new(
            config.harness.uia_provider.clone(),
            config.harness.uia_model.clone(),
        )
    } else {
        live
    }
}

// ── Operation Handler ─────────────────────────────────────────────────────────

/// Verarbeitet die `/model`-Operation.
///
/// # Beschreibung
/// Dispatcht anhand von [`ModelArgs::action`] auf das passende Sub-Kommando:
///
/// | Sub-Kommando | Verhalten |
/// |---|---|
/// | `show` (Standard) | Liest Live-Controller-Snapshot; fällt auf Config-Default zurück |
/// | `list` | Enumeriert den konfigurierten Katalog; markiert aktives Modell und Provider-Kompatibilität |
/// | `switch <id>` | Validiert ID gegen den konfigurierten Katalog; delegiert an [`crate::provider::handle_switch_core`] — wechselt Provider+Modell atomar |
/// | *(sonstiges)* | Gibt [`OpError::InvalidArguments`] zurück |
///
/// # Sicherheitsregel — kein ModelTool
/// Diese Funktion darf niemals als LLM-aufrufbares Tool exponiert werden.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Gemeinsamer Ausführungskontext.
/// - `args` (`ModelArgs`): Geparstes Sub-Kommando.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit menschenlesbarem Text.
///
/// # Fehler
/// - [`OpError::Execution`]: `SessionController` nicht verfügbar.
/// - [`OpError::InvalidArguments`]: Unbekannte ID, aufgelöster Zielprovider
///   unbekannt/deaktiviert/ohne Zugangsdaten, unbekanntes Sub-Kommando.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Liest den Session-Controller nur über `&self`. Hält selbst keinen geteilten Zustand.
///
/// # Spec
/// harwness Plan v2 — Tasks A–D.
///
/// # Beispiel
/// ```no_run
/// // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt aufrufbar.
/// ```
#[operation(
    name = "model",
    aliases = ["m"],
    summary = "Zeigt aktuelles Modell; wechselt Modell via `switch <id>`.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(
        path = "/model",
        visibility = "tui_only",
        busy = "staged",
        busy_subcommands = "show=immediate, list=immediate"
    ),
)]
async fn model(ctx: &OpContext, args: ModelArgs) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("show");

    // ── TASK D: Unbekannte Sub-Kommandos sofort ablehnen ─────────────────────
    match action {
        "show" | "list" | "switch" => {}
        other => {
            return Err(OpError::InvalidArguments(format!(
                "Unknown /model subcommand: '{other}'. \
                 Valid subcommands: show, list, switch <model-id>."
            )));
        }
    }

    // ── TASK C: switch — validiert + atomic mutiert ───────────────────────────
    if action == "switch" {
        let target = match args.target.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => {
                return Err(OpError::InvalidArguments(
                    "switch requires a model id: /model switch <id>".into(),
                ));
            }
        };

        let persistence = crate::config_util::selection_persistence(ctx);
        return handle_switch_core(ctx, target, move |provider, model| {
            persistence.persist_default_selection(provider, model)
        });
    }

    // ── TASK A/B: show + list read live state then fall back to config ────────
    let controller = ctx.service::<harw_operations::SharedSessionController>();
    let snap = controller
        .map(|c| c.snapshot())
        .unwrap_or_else(harw_operations::SessionControlSnapshot::empty);
    let config = crate::config_util::load_default_config("Config-Discovery fehlgeschlagen")?;

    let text = if action == "list" {
        format_list(&snap, &config)
    } else {
        format_show(&snap, &config)
    };

    Ok(OpOutput::from(text))
}

/// Shared core of the validated, atomic model switch.
///
/// # Beschreibung
/// Extracted from `/model switch` so both `/model switch` and
/// `/uia-model switch` share the exact same delegation — they differ only in
/// which config key the resulting selection persists to. `/model switch`
/// passes [`crate::config_util::persist_default_selection`]; `/uia-model
/// switch` passes [`crate::config_util::persist_uia_selection`].
///
/// Resolves `target` against the configured model catalog (never the static
/// bootstrap catalog) to find its canonical ID and configured provider, then
/// delegates **fully** to [`crate::provider::handle_switch_core`] — passing
/// the resolved provider and model together — so `/model switch <id>`
/// atomically switches provider+model in one call, even when the target
/// model belongs to a different provider than the one currently active. This
/// node (Welle 2, 2d) replaced the previous behaviour, which validated the
/// target model's provider against the *currently* active provider here and
/// rejected the switch on mismatch with a `/provider switch ... first` hint;
/// `provider::handle_switch_core` now performs that atomic switch itself.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext.
/// - `target` (`String`): die zu setzende Modell-ID oder ein konfigurierter Alias.
/// - `persist` (`impl FnOnce(Option<&str>, Option<&str>) -> Option<String>`):
///   wird nach erfolgreicher Controller-Mutation einmal mit
///   `(active_provider_nach_mutation, Some(canonical_model_id))` aufgerufen.
///   `None` bei Erfolg, `Some(note)` mit einer Fehlernotiz.
///
/// # Rückgabe
/// [`OpOutput`] mit Bestätigungstext inklusive Persistenz-Notiz.
///
/// # Fehler
/// - [`OpError::Execution`]: `SessionController` nicht verfügbar, Katalog leer,
///   oder Config-Discovery fehlgeschlagen.
/// - [`OpError::InvalidArguments`]: unbekannte Modell-ID, oder der aufgelöste
///   Zielprovider ist unbekannt/deaktiviert/ohne Zugangsdaten
///   ([`crate::provider::handle_switch_core`] validiert dies vollständig,
///   bevor irgendetwas mutiert wird).
///
/// # Spec
/// harwness Plan v2 — Task C; `/uia-model`-Folgeauftrag (UIA-spezifische gepinnte Auswahl);
/// Welle 2 (2d) — atomarer Provider+Modell-Wechsel via Delegation.
fn handle_switch_core(
    ctx: &OpContext,
    target: String,
    persist: impl FnOnce(Option<&str>, Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
    // Context-scoped-first resolution (see `crate::provider::resolved_config`
    // doc comment) — matches the authority `provider::handle_switch_core`
    // itself resolves against below, so a config injected into `ctx` for
    // testing or by the runtime is what both validation steps agree on.
    let config = crate::provider::resolved_config(ctx)?;
    if config.models.is_empty() {
        return Err(OpError::Execution(
            "configured model catalog is unavailable; refusing to switch models".into(),
        ));
    }

    // Resolve the target model against the configured catalog (id or alias,
    // never the static bootstrap catalog) to find its configured provider.
    let configured = configured_model(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {target}")))?;

    // Delegate fully to the provider switch core: validates the resolved
    // provider (existence, enabled, credentials) and the model together,
    // mutates the controller only once every check passes, then persists.
    crate::provider::handle_switch_core(
        ctx,
        configured.provider.clone(),
        Some(configured.id.clone()),
        persist,
    )
}

/// Wechselt atomar UIA-Provider+Modell: löst den konfigurierten Provider des
/// Ziel-Modells auf und delegiert vollständig an
/// [`crate::provider::handle_uia_switch_core`] (inklusive
/// [`crate::config_util::persist_uia_selection`] als `persist`-Abschluss) —
/// dieselbe Delegation wie [`handle_switch_core`] auf der generischen Achse.
///
/// # Beschreibung
/// Im Gegensatz zur vorherigen Implementierung validiert diese Funktion den
/// Zielprovider nicht mehr gegen den aktuell effektiven UIA-Provider und
/// lehnt bei Mismatch ab (mit einem `/uia-provider switch ... first`-Hinweis)
/// — stattdessen wechselt `provider::handle_uia_switch_core` Provider und
/// Modell gemeinsam, atomar, auch wenn das Ziel-Modell zu einem anderen
/// Provider gehört als der aktuell effektive UIA-Provider.
///
/// # Fehler
/// - [`OpError::Execution`]: Katalog leer oder Config-Discovery fehlgeschlagen.
/// - [`OpError::InvalidArguments`]: unbekannte Modell-ID, oder der aufgelöste
///   Zielprovider ist unbekannt/deaktiviert/ohne Zugangsdaten (siehe
///   [`crate::provider::handle_uia_switch_core`]).
///
/// # Spec
/// harwness Plan v2 — UIA-spezifische gepinnte Provider-/Modell-Auswahl;
/// Welle 2 (2d) — atomarer UIA-Provider+Modell-Wechsel via Delegation.
fn handle_uia_model_switch(ctx: &OpContext, target: String) -> Result<OpOutput, OpError> {
    let config = crate::provider::resolved_config(ctx)?;
    if config.models.is_empty() {
        return Err(OpError::Execution(
            "configured model catalog is unavailable; refusing to switch UIA models".into(),
        ));
    }

    // Resolve the target model against the configured catalog (id or alias)
    // to find its configured provider.
    let configured = configured_model(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {target}")))?;

    // Delegate fully to the UIA provider switch core: validates the resolved
    // provider and mutates provider+model together, atomically. `persist` is
    // passed explicitly (rather than hardcoded inside `handle_uia_switch_core`)
    // so tests can inject a `RecordingSelectionPersistence` via the
    // `ServiceMap` (see `crate::config_util::selection_persistence`) instead
    // of a bespoke no-op closure — production behavior is unchanged, this
    // resolves to `persist_uia_selection` (via `FileSelectionPersistence`)
    // whenever no service is injected.
    let persistence = crate::config_util::selection_persistence(ctx);
    crate::provider::handle_uia_switch_core(
        ctx,
        configured.provider.clone(),
        Some(configured.id.clone()),
        move |provider, model| persistence.persist_uia_selection(provider, model),
    )
}

/// Formatiert das `/uia-model show`-Sub-Kommando: zeigt das für die UIA
/// gepinnte Modell (`harness.uia_model`), unabhängig von `default_model`.
///
/// # Beschreibung
/// Liest `config.harness.uia_model` direkt (persistenter Config-Wert, nicht
/// Laufzeit-umgeschalteter Zustand — der UIA-Pin wird durch `/model switch`
/// nicht mutiert). Fehlt der Wert, wird der Fallback auf `default_model`
/// gemeldet.
///
/// # Argumente
/// - `config` (`&harw_config::ResolvedConfig`): geladene Config.
///
/// # Rückgabe
/// Fertig formatierter `String`.
///
/// # Spec
/// harwness Plan v2 — UIA-spezifische gepinnte Provider-/Modell-Auswahl.
fn format_uia_show(selection: &UiaSelection, config: &harw_config::ResolvedConfig) -> String {
    let model_line = match selection.model() {
        Some(id) => {
            let display = configured_model(config, id)
                .and_then(|model| model.name.as_deref())
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_owned());
            format!("UIA model    : {display} [{id}]")
        }
        None => "UIA model    : (nicht gesetzt)".to_owned(),
    };
    let provider_line = selection
        .provider()
        .map(|provider| format!("UIA provider : {provider}"))
        .unwrap_or_else(|| "UIA provider : (nicht gesetzt)".to_owned());
    format!("{model_line}\n{provider_line}")
}

fn format_uia_list(selection: &UiaSelection, config: &harw_config::ResolvedConfig) -> String {
    let active_model = selection.model();
    let active_provider = selection.provider();
    let mut models: Vec<_> = config.models.values().collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));

    let mut lines = vec![
        format!("UIA model      : {}", active_model.unwrap_or("(none)")),
        format!("UIA provider   : {}", active_provider.unwrap_or("(none)")),
        String::new(),
        "Catalog:".to_owned(),
    ];
    for model in models {
        if active_provider.is_some_and(|provider| model.provider != provider) {
            continue;
        }
        let marker = if active_model == Some(model.id.as_str()) {
            " *"
        } else {
            "  "
        };
        let compat = match active_provider {
            None => "unfiltered",
            Some(provider) if model.provider == provider => "compatible",
            Some(_) => "other-provider",
        };
        lines.push(format!(
            "{marker} {} / {}  [{compat}]",
            model.provider, model.id
        ));
    }
    lines.push("  (* = current UIA model)".to_owned());
    lines.join("\n")
}

/// Verarbeitet die `/uia-model`-Operation — zeigt/wechselt das für die UIA
/// gepinnte Modell (`uia_model`), unabhängig von `default_model`.
///
/// # Beschreibung
/// Wiederverwendet [`ModelArgs`] und dieselbe `show|list|switch`-Grammatik
/// wie `/model`:
///
/// | Sub-Kommando | Verhalten |
/// |---|---|
/// | `show` (Standard) | Meldet die effektive UIA-Auswahl |
/// | `list` | Listet den Katalog mit UIA-Kompatibilität |
/// | `switch <id>` | Wechselt nur das UIA-Modell |
/// | *(sonstiges)* | [`OpError::InvalidArguments`] |
///
/// # Sicherheitsregel — kein ModelTool
/// Diese Funktion darf niemals als LLM-aufrufbares Tool exponiert werden.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext.
/// - `args` (`ModelArgs`): geparstes Sub-Kommando.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit menschenlesbarem Text.
///
/// # Fehler
/// - [`OpError::Execution`]: `SessionController` nicht verfügbar (nur `switch`),
///   Config-Discovery fehlgeschlagen.
/// - [`OpError::InvalidArguments`]: unbekannte ID, Provider-Mismatch, unbekanntes Sub-Kommando.
///
/// # Spec
/// harwness Plan v2 — UIA-spezifische gepinnte Provider-/Modell-Auswahl.
#[operation(
    name = "uia-model",
    summary = "Zeigt/wechselt das für die UIA gepinnte Modell (uia_model), unabhängig vom Default.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(
        path = "/uia-model",
        visibility = "tui_only",
        busy = "staged",
        busy_subcommands = "-=immediate, show=immediate, list=immediate"
    )
)]
async fn uia_model(ctx: &OpContext, args: ModelArgs) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("show");

    match action {
        "show" | "list" | "switch" => {}
        other => {
            return Err(OpError::InvalidArguments(format!(
                "Unknown /uia-model subcommand: '{other}'. \
                 Valid subcommands: show, list, switch <model-id>."
            )));
        }
    }

    if action == "switch" {
        let target = match args.target.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => {
                return Err(OpError::InvalidArguments(
                    "switch requires a model id: /uia-model switch <id>".into(),
                ));
            }
        };

        return handle_uia_model_switch(ctx, target);
    }

    let controller = ctx.service::<harw_operations::SharedSessionController>();

    let config = crate::config_util::load_default_config("Config-Discovery fehlgeschlagen")?;

    let selection = effective_uia_selection(controller, &config);
    let text = if action == "list" {
        format_uia_list(&selection, &config)
    } else {
        format_uia_show(&selection, &config)
    };

    Ok(OpOutput::from(text))
}

/// Wechselt ausschließlich das für UIA-Worker-Sitzungen gepinnte Modell
/// (`uia_worker_model`) und validiert es gegen den EFFEKTIVEN UIA-Provider
/// (Live-Auswahl, sonst `harness.uia_provider` — via
/// [`effective_uia_selection`], genau wie [`handle_uia_model_switch`]). Es
/// gibt bewusst **kein** eigenes `uia_worker_provider`-Konzept: der Worker
/// teilt sich den Provider mit der UIA.
///
/// # Beschreibung
/// Im Gegensatz zu [`handle_uia_model_switch`] mutiert dieser Pfad **keinen**
/// Live-`SessionController`-Zustand — `uia_worker_model` wird ausschließlich
/// über [`crate::config_util::persist_uia_worker_model`] in der
/// Profil-`config.toml` verankert und wirkt erst beim nächsten
/// Sitzungsstart, analog zu anderen `internal_models.*`-Punkten.
///
/// `persist` mirrors [`handle_switch_core`]'s injected-closure design: the
/// production caller ([`uia_worker_model`]) always passes
/// [`crate::config_util::persist_uia_worker_model`], so runtime behavior is
/// unchanged from a hardcoded call — the injection exists purely so tests
/// can supply a no-op closure and never touch the real, `HARW_HOME`-resolving
/// persistence path (this crate declares `#![forbid(unsafe_code)]`, so a
/// testing-only `HARW_HOME` env-isolation helper, which would need `unsafe
/// fn std::env::set_var`/`remove_var`, is not available here).
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext (nur zum Lesen der effektiven
///   UIA-Provider-Auswahl über einen optionalen `SharedSessionController` —
///   keine Mutation).
/// - `target` (`String`): die zu setzende Modell-ID oder ein konfigurierter Alias.
/// - `persist` (`impl FnOnce(Option<&str>) -> Option<String>`): wird nach
///   erfolgreicher Validierung einmal mit `Some(canonical_model_id)`
///   aufgerufen. `None` bei Erfolg, `Some(note)` mit einer Fehlernotiz.
///
/// # Rückgabe
/// [`OpOutput`] mit Bestätigungstext ("… für die nächste Sitzung
/// gespeichert"), inklusive Persistenz-Notiz.
///
/// # Fehler
/// - [`OpError::Execution`]: Config-Discovery fehlgeschlagen, oder der
///   konfigurierte Modellkatalog ist leer.
/// - [`OpError::InvalidArguments`]: unbekannte Modell-ID, oder das Ziel-Modell
///   gehört zu einem anderen Provider als dem effektiven UIA-Provider.
///
/// # Spec
/// harwness Plan v2 — Welle 2 (2d), Teil 2 — `/uia-worker-model`.
fn handle_uia_worker_model_switch(
    ctx: &OpContext,
    target: String,
    persist: impl FnOnce(Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
    // Context-scoped-first resolution — see `crate::provider::resolved_config`
    // doc comment; same authority `handle_switch_core`/`handle_uia_model_switch`
    // already resolve against.
    let config = crate::provider::resolved_config(ctx)?;
    if config.models.is_empty() {
        return Err(OpError::Execution(
            "configured model catalog is unavailable; refusing to switch the UIA worker model"
                .into(),
        ));
    }

    let controller = ctx.service::<harw_operations::SharedSessionController>();
    let selection = effective_uia_selection(controller, &config);
    let configured = configured_model(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {target}")))?;

    if let Some(uia_provider) = selection.provider() {
        let model_provider = config
            .providers
            .iter()
            .find(|(key, provider)| {
                key.as_str() == configured.provider || provider.name == configured.provider
            })
            .map(|(_, provider)| provider.name.as_str())
            .unwrap_or(configured.provider.as_str());
        if model_provider != uia_provider {
            return Err(OpError::InvalidArguments(format!(
                "UIA worker model {target} requires provider {model_provider}, but effective UIA provider is {uia_provider}; \
                 use `/uia-provider switch {model_provider}` first"
            )));
        }
    }

    let configured_id = configured.id.clone();
    let mut text = format!("UIA worker model set to {configured_id}");
    match persist(Some(configured_id.as_str())) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str("\n(gespeichert für die nächste Sitzung)"),
    }

    Ok(OpOutput::from(text))
}

/// Formatiert das `/uia-worker-model show`-Sub-Kommando: zeigt das für
/// UIA-Worker-Sitzungen gepinnte Modell (`config.harness.uia_worker_model`)
/// und den effektiven UIA-Provider (geteilt mit `/uia-model`).
///
/// # Beschreibung
/// Im Unterschied zu [`format_uia_show`] liest "aktiv" hier **direkt** aus
/// `config.harness.uia_worker_model` statt aus einer Live-Selection — der
/// UIA-Worker-Pin hat keinen Live-`SessionController`-Gegenpart.
///
/// # Argumente
/// - `uia_provider` (`Option<&str>`): effektiver UIA-Provider (aus
///   [`effective_uia_selection`]), oder `None`, wenn keiner konfiguriert ist.
/// - `config` (`&harw_config::ResolvedConfig`): geladene Config.
///
/// # Rückgabe
/// Fertig formatierter `String`.
fn format_uia_worker_show(
    uia_provider: Option<&str>,
    config: &harw_config::ResolvedConfig,
) -> String {
    let model_line = match config.harness.uia_worker_model.as_deref() {
        Some(id) => {
            let display = configured_model(config, id)
                .and_then(|model| model.name.as_deref())
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_owned());
            format!("UIA worker model     : {display} [{id}]")
        }
        None => "UIA worker model     : (nicht gesetzt)".to_owned(),
    };
    let provider_line = uia_provider
        .map(|provider| format!("UIA provider (shared): {provider}"))
        .unwrap_or_else(|| "UIA provider (shared): (nicht gesetzt)".to_owned());
    format!("{model_line}\n{provider_line}")
}

/// Formatiert das `/uia-worker-model list`-Sub-Kommando: Katalog gefiltert
/// auf den effektiven UIA-Provider, "aktiv" markiert anhand
/// `config.harness.uia_worker_model` (siehe [`format_uia_worker_show`]).
fn format_uia_worker_list(
    uia_provider: Option<&str>,
    config: &harw_config::ResolvedConfig,
) -> String {
    let active_model = config.harness.uia_worker_model.as_deref();
    let mut models: Vec<_> = config.models.values().collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));

    let mut lines = vec![
        format!(
            "UIA worker model      : {}",
            active_model.unwrap_or("(none)")
        ),
        format!(
            "UIA provider (shared) : {}",
            uia_provider.unwrap_or("(none)")
        ),
        String::new(),
        "Catalog:".to_owned(),
    ];
    for model in models {
        if uia_provider.is_some_and(|provider| model.provider != provider) {
            continue;
        }
        let marker = if active_model == Some(model.id.as_str()) {
            " *"
        } else {
            "  "
        };
        let compat = match uia_provider {
            None => "unfiltered",
            Some(provider) if model.provider == provider => "compatible",
            Some(_) => "other-provider",
        };
        lines.push(format!(
            "{marker} {} / {}  [{compat}]",
            model.provider, model.id
        ));
    }
    lines.push("  (* = current UIA worker model)".to_owned());
    lines.join("\n")
}

/// Verarbeitet die `/uia-worker-model`-Operation — zeigt/wechselt das für
/// UIA-Worker-Sitzungen gepinnte Modell (`uia_worker_model`), unabhängig von
/// `default_model`/`uia_model`.
///
/// # Beschreibung
/// Wiederverwendet [`ModelArgs`] und dieselbe `show|list|switch`-Grammatik
/// wie `/model`/`/uia-model`:
///
/// | Sub-Kommando | Verhalten |
/// |---|---|
/// | `show` (Standard) | Meldet `uia_worker_model` + effektiven UIA-Provider |
/// | `list` | Listet den Katalog, gefiltert auf den effektiven UIA-Provider |
/// | `switch <id>` | Persistiert `uia_worker_model` — **kein** Live-Wechsel |
/// | *(sonstiges)* | [`OpError::InvalidArguments`] |
///
/// # Sicherheitsregel — kein ModelTool
/// Diese Funktion darf niemals als LLM-aufrufbares Tool exponiert werden.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext.
/// - `args` (`ModelArgs`): geparstes Sub-Kommando.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit menschenlesbarem Text.
///
/// # Fehler
/// - [`OpError::Execution`]: Config-Discovery fehlgeschlagen, konfigurierter
///   Modellkatalog leer (nur `switch`).
/// - [`OpError::InvalidArguments`]: unbekannte ID, Provider-Mismatch gegen
///   den effektiven UIA-Provider, unbekanntes Sub-Kommando.
///
/// # Spec
/// harwness Plan v2 — Welle 2 (2d), Teil 2 — `/uia-worker-model`.
#[operation(
    name = "uia-worker-model",
    summary = "Zeigt/setzt das für UIA-Worker-Sitzungen gepinnte Modell (uia_worker_model); wirkt erst ab der nächsten Sitzung.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(
        path = "/uia-worker-model",
        visibility = "tui_only",
        busy = "staged",
        busy_subcommands = "-=immediate, show=immediate, list=immediate"
    )
)]
async fn uia_worker_model(ctx: &OpContext, args: ModelArgs) -> Result<OpOutput, OpError> {
    let action = args.action.as_deref().unwrap_or("show");

    match action {
        "show" | "list" | "switch" => {}
        other => {
            return Err(OpError::InvalidArguments(format!(
                "Unknown /uia-worker-model subcommand: '{other}'. \
                 Valid subcommands: show, list, switch <model-id>."
            )));
        }
    }

    if action == "switch" {
        let target = match args.target.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => {
                return Err(OpError::InvalidArguments(
                    "switch requires a model id: /uia-worker-model switch <id>".into(),
                ));
            }
        };

        let persistence = crate::config_util::selection_persistence(ctx);
        return handle_uia_worker_model_switch(ctx, target, move |model| {
            persistence.persist_uia_worker_model(model)
        });
    }

    let controller = ctx.service::<harw_operations::SharedSessionController>();
    // Context-scoped-first resolution — see `crate::provider::resolved_config`
    // doc comment; used consistently across every branch of this brand-new
    // operation (unlike `model`/`uia_model`, whose show/list paths predate
    // this node and were intentionally left untouched).
    let config = crate::provider::resolved_config(ctx)?;

    let selection = effective_uia_selection(controller, &config);
    let text = if action == "list" {
        format_uia_worker_list(selection.provider(), &config)
    } else {
        format_uia_worker_show(selection.provider(), &config)
    };

    Ok(OpOutput::from(text))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModelArgs, configured_model};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, SessionController,
        SharedSessionController, context::ServiceMap,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;

    #[test]
    fn configured_model_resolves_configured_id_and_alias_without_bootstrap_catalog() {
        let mut config = harw_config::ResolvedConfig::default();
        config.models.insert(
            "private-model".to_owned(),
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
                id: "private-model-2026".to_owned(),
                name: Some("Private Model".to_owned()),
                provider: "private-provider".to_owned(),
                aliases: vec!["private-latest".to_owned()],
                context_window: None,
                max_tokens: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
                prompt_caching: None,
                default_reasoning_effort: None,
            },
        );

        assert_eq!(
            configured_model(&config, "private-model-2026").map(|model| model.provider.as_str()),
            Some("private-provider")
        );
        assert_eq!(
            configured_model(&config, "private-latest").map(|model| model.id.as_str()),
            Some("private-model-2026")
        );
        assert!(configured_model(&config, "nonexistent-model-xyz").is_none());
    }

    // ── Test-Kontext-Builder ──────────────────────────────────────────────────

    /// Erstellt einen minimalen `OpContext` mit optionalem `SharedSessionController`
    /// und optionaler context-gescopter `Arc<ResolvedConfig>`.
    ///
    /// # Description
    /// Erzeugt ein temporäres Workspace-Verzeichnis und bindet es in eine
    /// [`SandboxSpec`] ein. Falls `ctrl` Some ist, wird der Controller in die
    /// `ServiceMap` eingetragen. Falls `config` Some ist, wird sie ebenfalls
    /// eingetragen — [`crate::provider::resolved_config`] (und damit
    /// [`handle_switch_core`]/[`handle_uia_model_switch`]) liest sie dann
    /// bevorzugt statt echter `HARW_HOME`-Config-Discovery, genau wie es die
    /// Laufzeit (`harw-tui::command_exec::build_services`) für `/model`- und
    /// `/provider`-Ops tut.
    ///
    /// # Spec
    /// harwness Plan v2 — Tests Task E; Welle 2 (2d) — atomarer Provider+Modell-Wechsel.
    fn make_test_ctx(
        ctrl: Option<SharedSessionController>,
        config: Option<Arc<harw_config::ResolvedConfig>>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-model-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry::build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(c) = ctrl {
            services.insert(c);
        }
        if let Some(config) = config {
            services.insert(config);
        }
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((ctx, tmp))
    }

    /// Baut eine [`harw_config::ResolvedConfig`] mit zwei vollständig
    /// konfigurierten, aktivierten Providern (je Zugangsdaten via
    /// [`harw_config::SecretRef::Env`]) und je einem Modell — die
    /// Testgrundlage für die atomaren Provider+Modell-Wechsel-Tests unten
    /// (Welle 2, 2d, Teil 1).
    fn two_provider_config() -> harw_config::ResolvedConfig {
        fn provider(name: &str, env_key: &str, model_id: &str) -> harw_config::ProviderToml {
            harw_config::ProviderToml {
                stream: None,
                name: name.to_owned(),
                api: "openai-chat".to_owned(),
                base_url: format!("https://api.example.test/{name}"),
                auth: Some(harw_config::SecretRef::Env(env_key.to_owned())),
                auth_header: None,
                api_key: None,
                headers: Default::default(),
                models: vec![model_id.to_owned()],
                enabled: true,
                origin_allowlist: Default::default(),
                rate_limit: None,
                max_concurrency: None,
                originator: None,
                default_reasoning_effort: None,
                gateway_identity_headers: false,
            }
        }
        fn model(model_id: &str, provider_name: &str) -> harw_config::ModelToml {
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
                id: model_id.to_owned(),
                name: None,
                provider: provider_name.to_owned(),
                aliases: Vec::new(),
                context_window: None,
                max_tokens: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
                prompt_caching: None,
                default_reasoning_effort: None,
            }
        }

        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "provider-a".to_owned(),
            provider("provider-a", "HARW_TEST_PROVIDER_A_KEY", "model-a"),
        );
        config.providers.insert(
            "provider-b".to_owned(),
            provider("provider-b", "HARW_TEST_PROVIDER_B_KEY", "model-b"),
        );
        config
            .models
            .insert("model-a".to_owned(), model("model-a", "provider-a"));
        config
            .models
            .insert("model-b".to_owned(), model("model-b", "provider-b"));
        config
    }

    // ── FromRawArgs ───────────────────────────────────────────────────────────

    #[test]
    fn test_model_args_from_raw_args_list_sets_action() -> TestResult {
        let args = ModelArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => assert_eq!(a.action.as_deref(), Some("list")),
            Err(e) => return Err(TestError::Unexpected(format!("Unexpected error: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_model_args_from_raw_args_empty_tokens_sets_action_none() -> TestResult {
        let args = ModelArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.action.is_none()),
            Err(e) => return Err(TestError::Unexpected(format!("Unexpected error: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_model_args_switch_with_id_sets_target() -> TestResult {
        let args = ModelArgs::from_raw_args(&toks(&["switch", "gpt-5"]))
            .map_err(ctx("from_raw_args must not fail"))?;
        assert_eq!(args.action.as_deref(), Some("switch"));
        assert_eq!(args.target.as_deref(), Some("gpt-5"));
        Ok(())
    }

    #[test]
    fn test_model_args_bare_switch_has_no_target() -> TestResult {
        let args = ModelArgs::from_raw_args(&toks(&["switch"]))
            .map_err(ctx("from_raw_args must not fail"))?;
        assert_eq!(args.action.as_deref(), Some("switch"));
        assert!(
            args.target.is_none(),
            "bare 'switch' must produce no target"
        );
        Ok(())
    }

    // ── Task D: Unknown subcommand rejected ───────────────────────────────────

    /// Verifies that an unknown subcommand yields `OpError::InvalidArguments`
    /// listing the valid subcommands.
    ///
    /// # Spec
    /// harwness Plan v2 — Task D.
    #[tokio::test]
    async fn model_unknown_subcommand_rejected() -> TestResult {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl), None)?;
        let args = ModelArgs {
            action: Some("frobnicate".to_owned()),
            target: None,
            value: None,
        };
        let result = super::model(&ctx, args).await;
        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("frobnicate"),
                    "message must include the bad token: {msg}"
                );
                assert!(
                    msg.contains("show"),
                    "message must list valid subcommands: {msg}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Task A: show reports live active model ────────────────────────────────

    /// When a model is set via the controller, `/model show` must report it
    /// with "(live)" label, not the config default.
    ///
    /// # Spec
    /// harwness Plan v2 — Task A, Task E test 3.
    #[tokio::test]
    async fn model_show_reports_active_when_set() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_model("gpt-5".to_owned())
            .map_err(ctx("set_active_model must succeed"))?;
        ctrl.set_active_provider("openai".to_owned())
            .map_err(ctx("set_active_provider must succeed"))?;

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), None)?;

        let args = ModelArgs {
            action: Some("show".to_owned()),
            target: None,
            value: None,
        };
        let result = super::model(&ctx, args)
            .await
            .map_err(crate::test_support::ctx("show must not fail"))?;
        assert!(
            result.text.contains("gpt-5"),
            "show output must contain active model id: {}",
            result.text
        );
        assert!(
            result.text.contains("live"),
            "show output must mark live state: {}",
            result.text
        );
        assert!(
            result.text.contains("openai"),
            "show output must contain active provider: {}",
            result.text
        );
        Ok(())
    }

    // ── Task B: list marks current and compatibility ──────────────────────────

    /// When active_model is set and active_provider is set, `/model list` must:
    /// - mark the active model with `*`
    /// - label models from the active provider as "compatible"
    /// - label models from other providers as "other-provider"
    ///
    /// # Spec
    /// harwness Plan v2 — Task B, Task E test 4.
    #[test]
    fn model_list_marks_current_and_compatibility() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_model("gpt-5".to_owned())
            .map_err(ctx("set_active_model must succeed"))?;
        ctrl.set_active_provider("openai".to_owned())
            .map_err(ctx("set_active_provider must succeed"))?;

        let mut config = harw_config::ResolvedConfig::default();
        for (key, id, provider) in [
            ("gpt-5", "gpt-5", "openai"),
            ("private-model", "private-model-2026", "private-provider"),
        ] {
            config.models.insert(
                key.to_owned(),
                harw_config::ModelToml {
                    stream: None,
                    rate_limit: None,
                    id: id.to_owned(),
                    name: None,
                    provider: provider.to_owned(),
                    aliases: Vec::new(),
                    context_window: None,
                    max_tokens: None,
                    reasoning: false,
                    input_types: Vec::new(),
                    capabilities: harw_config::ModelCapabilitiesToml::default(),
                    prompt_caching: None,
                    default_reasoning_effort: None,
                },
            );
        }

        let text = super::format_list(&ctrl.snapshot(), &config);

        // Active model must be flagged.
        assert!(
            text.contains('*'),
            "list output must contain '*' marker for active model: {text}"
        );

        // The configured active-provider model must be labelled compatible.
        assert!(
            text.contains("compatible"),
            "list output must contain 'compatible' label: {text}"
        );

        // The configured catalog includes a model from another provider.
        assert!(
            text.contains("other-provider"),
            "list output must contain 'other-provider' label: {text}"
        );
        Ok(())
    }

    #[test]
    fn effective_uia_selection_prefers_live_uia_and_never_generic_state() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_provider("generic-provider".to_owned())
            .map_err(ctx("generic provider selection must succeed"))?;
        ctrl.set_active_model("generic-model".to_owned())
            .map_err(ctx("generic model selection must succeed"))?;

        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("default-provider".to_owned());
        config.harness.default_model = Some("default-model".to_owned());
        config.harness.uia_provider = Some("config-uia-provider".to_owned());
        config.harness.uia_model = Some("config-uia-model".to_owned());

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let selection = super::effective_uia_selection(Some(&shared), &config);
        assert_eq!(selection.provider(), Some("config-uia-provider"));
        assert_eq!(selection.model(), Some("config-uia-model"));

        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("live-uia-provider".to_owned()),
            Some("live-uia-model".to_owned()),
        ))
        .map_err(ctx("UIA selection must succeed"))?;
        let live = super::effective_uia_selection(Some(&shared), &config);
        assert_eq!(live.provider(), Some("live-uia-provider"));
        assert_eq!(live.model(), Some("live-uia-model"));
        assert_ne!(live.provider(), Some("generic-provider"));
        assert_ne!(live.model(), Some("generic-model"));
        Ok(())
    }

    #[test]
    fn effective_uia_selection_does_not_revive_a_cleared_model_after_provider_switch() -> TestResult
    {
        let ctrl = Arc::new(NullSessionController::new());
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.uia_provider = Some("kimi".to_owned());
        config.harness.uia_model = Some("@cf/zai-org/glm-5.3-flash".to_owned());

        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("fireworks".to_owned()),
            None,
        ))
        .map_err(ctx("UIA provider switch must succeed"))?;

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let selection = super::effective_uia_selection(Some(&shared), &config);
        assert_eq!(selection.provider(), Some("fireworks"));
        assert_eq!(selection.model(), None);
        Ok(())
    }

    // ── Welle 2 (2d), Teil 1: atomarer Provider+Modell-Wechsel via Delegation ──
    //
    // Die beiden `*_switches_both_atomically`-Tests unten rufen die private
    // `handle_switch_core`/`handle_uia_switch_core`-Kernfunktion direkt mit
    // einem No-op-`persist`-Abschluss auf, statt über `super::model`/
    // `super::uia_model` zu gehen (die den echten, `HARW_HOME`-auflösenden
    // `persist_default_selection`/`persist_uia_selection` fest verdrahten).
    // Diese Crate deklariert `#![forbid(unsafe_code)]`, daher steht eine
    // `unsafe fn std::env::set_var`-basierte `HARW_HOME`-Isolation (wie sie
    // `config_util`/`permissions` bewusst vermeiden, siehe deren
    // Modul-Kommentare) hier nicht zur Verfügung — der No-op-`persist` prüft
    // exakt die unter Test stehende Eigenschaft (die atomare
    // Controller-Mutation) ohne jemals das Dateisystem zu berühren.

    /// `/model switch <id>` muss Provider+Modell atomar wechseln, auch wenn
    /// das Ziel-Modell zu einem anderen Provider gehört als der aktuell
    /// aktive — die zentrale Verhaltensänderung dieses Knotens gegenüber der
    /// vorherigen Provider-Mismatch-Ablehnung.
    #[test]
    fn model_switch_to_different_provider_switches_both_atomically() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_provider("provider-a".to_owned())
            .map_err(ctx("seed active provider"))?;
        ctrl.set_active_model("model-a".to_owned())
            .map_err(ctx("seed active model"))?;

        let config = Arc::new(two_provider_config());
        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let output = super::handle_switch_core(&ctx, "model-b".to_owned(), |_, _| None).map_err(
            crate::test_support::ctx("switching to a different provider's model must succeed"),
        )?;
        assert!(
            output.text.contains("model-b"),
            "confirmation must mention the new model: {}",
            output.text
        );

        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_provider.as_deref(),
            Some("provider-b"),
            "provider must have switched atomically alongside the model"
        );
        assert_eq!(
            snap.active_model.as_deref(),
            Some("model-b"),
            "model must have switched to the requested target"
        );
        Ok(())
    }

    /// UIA-Achse von [`model_switch_to_different_provider_switches_both_atomically`]:
    /// `/uia-model switch <id>` muss die UIA-Auswahl (Provider+Modell) atomar
    /// wechseln, ohne die generische `active_*`-Achse zu berühren.
    #[test]
    fn uia_model_switch_to_different_provider_switches_both_atomically() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("provider-a".to_owned()),
            Some("model-a".to_owned()),
        ))
        .map_err(ctx("seed UIA selection"))?;

        let config = Arc::new(two_provider_config());
        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let output = crate::provider::handle_uia_switch_core(
            &ctx,
            "provider-b".to_owned(),
            Some("model-b".to_owned()),
            |_, _| None,
        )
        .map_err(crate::test_support::ctx(
            "switching the UIA to a different provider's model must succeed",
        ))?;
        assert!(
            output.text.contains("model-b"),
            "confirmation must mention the new UIA model: {}",
            output.text
        );

        let after = ctrl.uia_selection();
        assert_eq!(
            after.provider.as_deref(),
            Some("provider-b"),
            "UIA provider must have switched atomically alongside the UIA model"
        );
        assert_eq!(after.model.as_deref(), Some("model-b"));
        Ok(())
    }

    /// Ein `/model switch` auf ein Modell, dessen Provider deaktiviert ist,
    /// muss vollständig fehlschlagen (`InvalidArguments`) und darf **nichts**
    /// mutiert haben — der Controller-Snapshot vor und nach dem Aufruf muss
    /// identisch sein. Die Ablehnung geschieht in
    /// `provider::handle_switch_core`s Step 2 (enabled-Prüfung), bevor der
    /// Controller überhaupt berührt wird — kein `HARW_HOME`-Zugriff nötig,
    /// weil `persist` nie erreicht wird.
    #[tokio::test]
    async fn model_switch_to_disabled_target_provider_is_atomic_on_failure() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_provider("provider-a".to_owned())
            .map_err(ctx("seed active provider"))?;
        ctrl.set_active_model("model-a".to_owned())
            .map_err(ctx("seed active model"))?;

        let mut config = two_provider_config();
        config
            .providers
            .get_mut("provider-b")
            .ok_or(TestError::Missing(
                "provider-b must exist in the test fixture",
            ))?
            .enabled = false;
        let config = Arc::new(config);

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let before = ctrl.snapshot();

        let args = ModelArgs {
            action: Some("switch".to_owned()),
            target: Some("model-b".to_owned()),
            value: None,
        };
        let result = super::model(&ctx, args).await;

        match result {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected InvalidArguments for a disabled target provider, got: {other:?}"
                )));
            }
        }

        let after = ctrl.snapshot();
        assert_eq!(
            before.active_provider, after.active_provider,
            "a failed switch must not change the active provider"
        );
        assert_eq!(
            before.active_model, after.active_model,
            "a failed switch must not change the active model"
        );
        Ok(())
    }

    // ── Welle 2 (2d), Teil 2: `/uia-worker-model` ──────────────────────────────

    /// Ein Ziel-Modell, dessen konfigurierter Provider nicht dem effektiven
    /// UIA-Provider entspricht, muss `InvalidArguments` liefern. Validation
    /// fails before any persistence is attempted, so no `HARW_HOME` isolation
    /// is needed.
    #[test]
    fn handle_uia_worker_model_switch_rejects_a_model_from_a_different_provider() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("provider-a".to_owned()),
            None,
        ))
        .map_err(ctx("seed effective UIA provider"))?;

        let config = Arc::new(two_provider_config());
        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let result = super::handle_uia_worker_model_switch(&ctx, "model-b".to_owned(), |_| None);

        match result {
            Err(OpError::InvalidArguments(msg)) => {
                assert!(
                    msg.contains("model-b") && msg.contains("provider-a"),
                    "message must name both the rejected model and the effective UIA provider: {msg}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Expected InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// A compatible switch must persist `uia_worker_model` and confirm with
    /// the "next session" wording — while leaving **every** live
    /// `SessionController` field untouched (no `set_uia_model`/
    /// `set_uia_selection` call exists on this path, unlike `/uia-model
    /// switch`). Uses a no-op `persist` closure (see the `handle_uia_worker_model_switch`
    /// doc comment) rather than the real, `HARW_HOME`-resolving
    /// `persist_uia_worker_model`, so this test never touches the filesystem.
    #[test]
    fn handle_uia_worker_model_switch_persists_and_confirms() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("provider-a".to_owned()),
            Some("model-a".to_owned()),
        ))
        .map_err(ctx("seed UIA selection"))?;

        let config = Arc::new(two_provider_config());
        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let before = ctrl.uia_selection();

        let output = super::handle_uia_worker_model_switch(&ctx, "model-a".to_owned(), |_| None)
            .map_err(crate::test_support::ctx(
                "switching to a compatible UIA worker model must succeed",
            ))?;

        assert!(
            output.text.contains("für die nächste Sitzung"),
            "confirmation must state the pin only takes effect next session, not \
             'wirkt ab dem nächsten Turn': {}",
            output.text
        );
        assert!(
            output.text.contains("model-a"),
            "confirmation must mention the switched model: {}",
            output.text
        );

        let after = ctrl.uia_selection();
        assert_eq!(
            before, after,
            "handle_uia_worker_model_switch must never mutate live SessionController state"
        );
        Ok(())
    }

    /// `/uia-worker-model show` must report `config.harness.uia_worker_model`
    /// — not any live selection. A live generic UIA model ("model-b") is
    /// seeded on the controller to prove it does not leak into the
    /// worker-model output, which has no live counterpart of its own.
    #[tokio::test]
    async fn uia_worker_model_show_reports_config_value_without_a_live_override() -> TestResult {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("provider-a".to_owned()),
            Some("model-b".to_owned()),
        ))
        .map_err(ctx(
            "seed live UIA selection (must not leak into the worker-model show)",
        ))?;

        let mut config = two_provider_config();
        config.harness.uia_worker_model = Some("model-a".to_owned());
        let config = Arc::new(config);

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared), Some(config))?;

        let args = ModelArgs {
            action: Some("show".to_owned()),
            target: None,
            value: None,
        };
        let result = super::uia_worker_model(&ctx, args)
            .await
            .map_err(crate::test_support::ctx("show must not fail"))?;

        assert!(
            result.text.contains("model-a"),
            "show must report config.harness.uia_worker_model: {}",
            result.text
        );
        assert!(
            !result.text.contains("model-b"),
            "show must not leak the live generic UIA model selection: {}",
            result.text
        );
        Ok(())
    }
}
