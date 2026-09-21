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
//! - `switch` ruft [`harw_operations::SessionController::set_active_model`] auf und gibt
//!   [`harw_operations::OpError::Execution`] zurück, wenn der Controller fehlt oder scheitert.
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
//!   unbekannte Modell-ID, Modell inkompatibel mit aktivem Provider.
//!
//! # Spec
//! harwness Plan v2 — `/model`-Operation, Tasks A–E.
//!
//! # Beispiel
//! ```no_run
//! // Wird vom harw-ops-Dispatch-Layer aufgerufen — nicht direkt nutzbar.
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, SessionController};
use harw_operations::session_control::UiaSelection;

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
/// | `switch <id>` | Validiert ID gegen den konfigurierten Katalog + Provider-Kompatibilität; mutiert Session-Zustand |
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
/// - [`OpError::InvalidArguments`]: Unbekannte ID, Provider-Mismatch, unbekanntes Sub-Kommando.
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
    command(path = "/model", visibility = "tui_only"),
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

        return handle_switch_core(ctx, target, crate::config_util::persist_default_selection);
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
/// `/uia-model switch` share the exact same validation/mutation sequence —
/// they differ only in which config key the resulting selection persists to.
/// `/model switch` passes [`crate::config_util::persist_default_selection`];
/// `/uia-model switch` passes [`crate::config_util::persist_uia_selection`].
///
/// Validates `target` against the configured model catalog (never the static
/// bootstrap catalog), checks provider compatibility against the live active
/// provider (falling back to `default_provider`), mutates the controller only
/// once both checks pass, then persists via `persist`.
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
/// - [`OpError::InvalidArguments`]: unbekannte Modell-ID, Provider-Mismatch.
///
/// # Spec
/// harwness Plan v2 — Task C; `/uia-model`-Folgeauftrag (UIA-spezifische gepinnte Auswahl).
fn handle_switch_core(
    ctx: &OpContext,
    target: String,
    persist: impl FnOnce(Option<&str>, Option<&str>) -> Option<String>,
) -> Result<OpOutput, OpError> {
    let config = crate::config_util::load_default_config("Config-Discovery fehlgeschlagen")?;
    if config.models.is_empty() {
        return Err(OpError::Execution(
            "configured model catalog is unavailable; refusing to switch models".into(),
        ));
    }

    // Step a: validate against the configured catalog, never the static bootstrap.
    let configured = configured_model(&config, &target)
        .ok_or_else(|| OpError::InvalidArguments(format!("unknown model: {target}")))?;
    let configured_id = configured.id.clone();

    // Step b: check provider compatibility if an active provider is set.
    let controller = ctx
        .service::<harw_operations::SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;

    let snap = controller.snapshot();
    if let Some(active_p) = snap
        .active_provider
        .as_deref()
        .or(config.harness.default_provider.as_deref())
    {
        if configured.provider != active_p {
            let model_provider = configured.provider.as_str();
            return Err(OpError::InvalidArguments(format!(
                "model {target} requires provider {model_provider}, \
                 but active provider is {active_p}; \
                 use `/provider switch {model_provider}` first"
            )));
        }
    }

    // Step c: only mutate if compatible.
    controller
        .set_active_model(configured_id.clone())
        .map_err(|e| OpError::Execution(e.to_string()))?;

    // Step d: persist, best-effort. The active provider is read back from the
    // controller (rather than re-derived) so a still-unknown provider never
    // gets written as a stale default.
    let snap_after = controller.snapshot();
    let mut text = format!("model switched to {configured_id}; next turn will use it");
    match persist(
        snap_after.active_provider.as_deref(),
        Some(configured_id.as_str()),
    ) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str("\n(als Standard für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput::from(text))
}

/// Wechselt ausschließlich das UIA-Modell und validiert gegen den effektiven
/// UIA-Provider (Live-Auswahl, sonst `harness.uia_provider`).
fn handle_uia_model_switch(ctx: &OpContext, target: String) -> Result<OpOutput, OpError> {
    let config = crate::config_util::load_default_config("Config-Discovery fehlgeschlagen")?;
    if config.models.is_empty() {
        return Err(OpError::Execution(
            "configured model catalog is unavailable; refusing to switch UIA models".into(),
        ));
    }

    let controller = ctx
        .service::<harw_operations::SharedSessionController>()
        .ok_or_else(|| OpError::Execution("SessionController not available".into()))?;
    let selection = effective_uia_selection(Some(controller), &config);
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
                "UIA model {target} requires provider {model_provider}, but effective UIA provider is {uia_provider}; \
                 use `/uia-provider switch {model_provider}` first"
            )));
        }
    }

    let configured_id = configured.id.clone();
    controller
        .set_uia_model(configured_id.clone())
        .map_err(|e| OpError::Execution(e.to_string()))?;

    let after = controller.uia_selection();
    let mut text = format!(
        "UIA model switched to {configured_id}; next UIA turn will use it"
    );
    match crate::config_util::persist_uia_selection(
        after.provider.as_deref().or(selection.provider()),
        Some(configured_id.as_str()),
    ) {
        Some(note) => {
            text.push('\n');
            text.push_str(&note);
        }
        None => text.push_str("\n(UIA-Auswahl für künftige Sitzungen gespeichert)"),
    }

    Ok(OpOutput::from(text))
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
fn format_uia_show(
    selection: &UiaSelection,
    config: &harw_config::ResolvedConfig,
) -> String {
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

fn format_uia_list(
    selection: &UiaSelection,
    config: &harw_config::ResolvedConfig,
) -> String {
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
    command(path = "/uia-model", visibility = "tui_only"),
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModelArgs, configured_model};
    use crate::testutil::toks;
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, SessionController,
        SharedSessionController, context::ServiceMap,
    };
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;

    #[test]
    fn configured_model_resolves_configured_id_and_alias_without_bootstrap_catalog() {
        let mut config = harw_config::ResolvedConfig::default();
        config.models.insert(
            "private-model".to_owned(),
            harw_config::ModelToml {
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

    /// Erstellt einen minimalen `OpContext` mit optionalem `SharedSessionController`.
    ///
    /// # Description
    /// Erzeugt ein temporäres Workspace-Verzeichnis und bindet es in eine
    /// [`SandboxSpec`] ein. Falls `ctrl` Some ist, wird der Controller in die
    /// `ServiceMap` eingetragen.
    ///
    /// # Spec
    /// harwness Plan v2 — Tests Task E.
    fn make_test_ctx(ctrl: Option<SharedSessionController>) -> (OpContext, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("harw-model-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws")).unwrap();
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("WorkspaceRegistry::build");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(c) = ctrl {
            services.insert(c);
        }
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        (ctx, tmp)
    }

    // ── FromRawArgs ───────────────────────────────────────────────────────────

    #[test]
    fn test_model_args_from_raw_args_list_sets_action() {
        let args = ModelArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => assert_eq!(a.action.as_deref(), Some("list")),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn test_model_args_from_raw_args_empty_tokens_sets_action_none() {
        let args = ModelArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.action.is_none()),
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn test_model_args_switch_with_id_sets_target() {
        let args = ModelArgs::from_raw_args(&toks(&["switch", "gpt-5"]))
            .expect("from_raw_args must not fail");
        assert_eq!(args.action.as_deref(), Some("switch"));
        assert_eq!(args.target.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn test_model_args_bare_switch_has_no_target() {
        let args =
            ModelArgs::from_raw_args(&toks(&["switch"])).expect("from_raw_args must not fail");
        assert_eq!(args.action.as_deref(), Some("switch"));
        assert!(
            args.target.is_none(),
            "bare 'switch' must produce no target"
        );
    }

    // ── Task D: Unknown subcommand rejected ───────────────────────────────────

    /// Verifies that an unknown subcommand yields `OpError::InvalidArguments`
    /// listing the valid subcommands.
    ///
    /// # Spec
    /// harwness Plan v2 — Task D.
    #[tokio::test]
    async fn model_unknown_subcommand_rejected() {
        let ctrl: SharedSessionController = Arc::new(NullSessionController::new());
        let (ctx, _tmp) = make_test_ctx(Some(ctrl));
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
            other => panic!("Expected InvalidArguments, got: {other:?}"),
        }
    }

    // ── Task A: show reports live active model ────────────────────────────────

    /// When a model is set via the controller, `/model show` must report it
    /// with "(live)" label, not the config default.
    ///
    /// # Spec
    /// harwness Plan v2 — Task A, Task E test 3.
    #[tokio::test]
    async fn model_show_reports_active_when_set() {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_model("gpt-5".to_owned())
            .expect("set_active_model must succeed");
        ctrl.set_active_provider("openai".to_owned())
            .expect("set_active_provider must succeed");

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let (ctx, _tmp) = make_test_ctx(Some(shared));

        let args = ModelArgs {
            action: Some("show".to_owned()),
            target: None,
            value: None,
        };
        let result = super::model(&ctx, args).await.expect("show must not fail");
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
    fn model_list_marks_current_and_compatibility() {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_model("gpt-5".to_owned())
            .expect("set_active_model must succeed");
        ctrl.set_active_provider("openai".to_owned())
            .expect("set_active_provider must succeed");

        let mut config = harw_config::ResolvedConfig::default();
        for (key, id, provider) in [
            ("gpt-5", "gpt-5", "openai"),
            ("private-model", "private-model-2026", "private-provider"),
        ] {
            config.models.insert(
                key.to_owned(),
                harw_config::ModelToml {
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
    }

    #[test]
    fn effective_uia_selection_prefers_live_uia_and_never_generic_state() {
        let ctrl = Arc::new(NullSessionController::new());
        ctrl.set_active_provider("generic-provider".to_owned())
            .expect("generic provider selection must succeed");
        ctrl.set_active_model("generic-model".to_owned())
            .expect("generic model selection must succeed");

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
        .expect("UIA selection must succeed");
        let live = super::effective_uia_selection(Some(&shared), &config);
        assert_eq!(live.provider(), Some("live-uia-provider"));
        assert_eq!(live.model(), Some("live-uia-model"));
        assert_ne!(live.provider(), Some("generic-provider"));
        assert_ne!(live.model(), Some("generic-model"));
    }

    #[test]
    fn effective_uia_selection_does_not_revive_a_cleared_model_after_provider_switch() {
        let ctrl = Arc::new(NullSessionController::new());
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.uia_provider = Some("kimi".to_owned());
        config.harness.uia_model = Some("@cf/zai-org/glm-5.3-flash".to_owned());

        ctrl.set_uia_selection(harw_operations::session_control::UiaSelection::new(
            Some("fireworks".to_owned()),
            None,
        ))
        .expect("UIA provider switch must succeed");

        let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
        let selection = super::effective_uia_selection(Some(&shared), &config);
        assert_eq!(selection.provider(), Some("fireworks"));
        assert_eq!(selection.model(), None);
    }
}
