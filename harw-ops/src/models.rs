//! `/models` — Modellübersicht und -auswahl je Rolle (UIA, Worker, Orchestrator, interne Stellen).
//!
//! # Verantwortungsbereich
//! Bündelt die bislang über mehrere Befehle verstreute Modellwahl
//! (`/uia-model`, `/uia-worker-model`, `[internal_models.*]`) in **einer**
//! Operation mit einheitlicher Grammatik:
//!
//! | Eingabe | Wirkung |
//! |---|---|
//! | `/models` / `/models show` | Tabelle aller Rollen (Provider, Modell, Quelle, Effort) plus Live-Modell der Sitzung |
//! | `/models set <rolle> <ziel>` | Persistiert das Modell der Rolle in der Profil-`config.toml` |
//! | `/models reset <rolle>` | Entfernt die explizite Wahl der Rolle (zurück auf ihren Standard) |
//! | `/models image <ziel>` | Wählt das Bildmodell (`image-understanding`) im Profil; bestätigt Bildfähigkeit vor dem Speichern |
//! | `/models image [show\|reset]` | Zeigt die Bildmodellwahl oder entfernt sie; ohne Wahl nur bildfähiges Hauptmodell, kein externer Standard |
//! | `/models worker` | Wahl je UIA-Worker-Rolle („wie UIA“ oder Provider/Modell), Runde 5 Teil G |
//! | `/models worker <rolle\|all> <uia\|ziel>` | Setzt die Wahl einer bzw. aller UIA-Worker-Rollen |
//! | `/models add <provider>/<modell>` | Nimmt das Katalogmodell in `models` der Provider-Datei des Profils auf (sortiert, ohne Duplikate) |
//! | `/models remove <provider>/<modell>` | Entfernt es wieder und löscht die zugehörige Modelldatei (nur reguläre Datei, kein Symlink) |
//! | `/models scan [provider]` | Delegiert an den URL-Guard; bis dahin `NotAvailable` |
//!
//! `<ziel>` ist entweder eine bekannte Modell-ID (Schlüssel, `id` oder Alias
//! aus `config.models`; der Provider wird aus dem Katalog übernommen) oder
//! `provider/modell`, getrennt am **ersten** `/`, sofern das Präfix ein
//! aktivierter Provider ist (z. B. `openrouter/nvidia/nemotron-…`).
//!
//! Änderungen an Kind-Rollen (Orchestrator, Sub-Orchestrator, Worker,
//! Explorer, Recherche, Gedächtnis) gelten in einer TUI-Sitzung sofort für
//! **neu gestartete** Agenten (Live-Modellwechsel über
//! [`crate::live_model::LiveModelControl`]); alle übrigen Rollen wirken ab
//! der nächsten Sitzung. `/model` wechselt das Modell der laufenden Sitzung
//! live.
//!
//! # Sicherheitsregel — kein ModelTool
//! `permission = "operator"`, `visibility = "tui_only"`: das Modell darf die
//! Modellwahl (auch die seiner Kinder) nicht selbst verändern.
//!
//! # Schlüsseltypen
//! - [`ModelsArgs`] — positionale Argumente `action role target`.
//! - `ModelsOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Datenvertrag (`OpOutput::data`)
//! `show` → `{"roles":[{"role","label","provider","model","source","effort"}],"live":{"provider","model"}}`;
//! `set`/`reset` → `{"action","role","provider","model","persisted","note"}`.
//!
//! # Nebenläufigkeit
//! Zustandslos; Persistenz über den austauschbaren
//! [`crate::config_util::SelectionPersistence`]-Dienst (bestes Bemühen, nie `Err`).
//!
//! # Lebenszyklus der Provider-Angebote
//! `set` und `worker` prüfen die ausdrücklich gewählte Provider-/Modell-ID
//! mit [`crate::live_model::enforce_lifecycle`], **bevor** etwas persistiert
//! oder ein Provider-Client gebaut wird: ein abgeschaltetes (retired) Modell
//! wird mit dem empfohlenen Ersatz abgelehnt und die Profil-`config.toml`
//! bleibt unverändert; ein abgekündigtes (deprecated) Modell wird gespeichert
//! und die Bestätigung trägt die Warnung. Geprüft und persistiert wird stets
//! die kanonische Katalog-ID: auch `provider/alias` wird vorher auf die
//! konfigurierte Modell-ID **dieses Providers** aufgelöst (`resolve_target`);
//! ein Alias oder Schlüssel, der nur zu einem anderen Provider gehört, wird
//! abgelehnt, bevor etwas persistiert wird.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando, unbekannte Rolle,
//!   fehlendes/unauflösbares Ziel, Worker-Modell eines fremden Providers,
//!   laut Provider-Lebenszyklus abgeschaltetes Modell (`set`, `worker`).
//! - [`OpError::Execution`]: Config-Discovery fehlgeschlagen.

use harw_config::{ModelRole, ResolvedConfig, RoleModelRow};
use harw_macros::operation;
use harw_operations::operation::{RemoteEffect, RemotePolicy, RemoteVerb};
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput, SharedSessionController};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Remote-Allowlist von `/models` (Plan 4.2). Lese-Formen zuerst; der erste
/// Treffer gilt. `pick` (interaktiver Picker) und `--allow-insecure-lan`
/// sind an keiner Position remote erlaubt.
pub const REMOTE: RemotePolicy = RemotePolicy {
    path: "/models",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["show", "-"]),
        RemoteVerb::read(&["list", "-"]),
        RemoteVerb::read(&["image", "-"]),
        RemoteVerb::read(&["image", "show", "-"]),
        RemoteVerb::read(&["image", "list", "-"]),
        RemoteVerb::read(&["worker", "-"]),
        RemoteVerb::global(&["set", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["reset", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["image", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["worker", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["add", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["remove", "*"], RemoteEffect::NextSession),
        RemoteVerb::global(&["scan", "*"], RemoteEffect::NextSession),
    ],
    denied_tokens: &["--allow-insecure-lan", "pick"],
};

/// Hinweis, der jeder Anzeige und jeder Änderung beigefügt wird.
pub(crate) const EFFECTIVE_NEXT_SESSION: &str =
    "Änderungen wirken ab der nächsten Sitzung (nur /model wechselt live).";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Argumente der `/models`-Operation.
///
/// # Felder
/// - `action`: `show` (Standard), `set` oder `reset` (Token 0).
/// - `role`: Rollenschlüssel, z. B. `uia`, `worker-simple` (Token 1).
/// - `target`: Modell-ID/Alias oder `provider/modell` (Token 2, nur `set`).
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ModelsArgs {
    /// Sub-Kommando; `None` wird als `show` behandelt.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Rollenschlüssel (siehe [`ModelRole::key`]).
    #[serde(default)]
    #[raw(nth = 1)]
    pub role: Option<String>,
    /// Ziel für `set`.
    #[serde(default)]
    #[raw(nth = 2)]
    pub target: Option<String>,
    /// Optionales Reasoning-Gewicht für `set` (minimal|low|medium|high|xhigh|max).
    #[serde(default)]
    #[raw(nth = 3)]
    pub effort: Option<String>,
}

/// Parst ein optionales Effort-Argument gegen [`harw_types::ReasoningEffort`]
/// und akzeptiert nur Rollen mit eigenem Gewichtsfeld (siehe
/// `harw-config::resolve_effort`).
pub(crate) fn parse_role_effort(
    raw: Option<&str>,
    role_key: &str,
) -> Result<Option<harw_types::ReasoningEffort>, OpError> {
    let raw = match raw.map(str::trim) {
        Some("") | None => return Ok(None),
        Some(value) => value,
    };
    use std::str::FromStr;
    let level = harw_types::ReasoningEffort::from_str(raw).map_err(|_| {
        OpError::InvalidArguments(format!(
            "Unbekanntes Effort-Level: '{raw}'. \
             Erwartet: minimal | low | medium | high | xhigh | max."
        ))
    })?;
    if role_effort_field(role_key).is_none() {
        return Err(OpError::InvalidArguments(format!(
            "Rolle '{role_key}' hat kein eigenes Reasoning-Gewicht. \
             Gültig: uia, root-orchestrator, sub-orchestrator, \
             worker-simple, worker-complex."
        )));
    }
    Ok(Some(level))
}

// ── Hilfsfunktionen ──────────────────────────────────────────────────────────

/// Kommagetrennte Liste aller gültigen Rollenschlüssel (für Fehlermeldungen).
fn known_roles() -> String {
    ModelRole::ALL
        .iter()
        .map(|role| role.key())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parst den Rollenschlüssel oder liefert eine deutsche Fehlermeldung.
fn parse_role(raw: Option<&str>, usage: &str) -> Result<ModelRole, OpError> {
    let raw = match raw.map(str::trim) {
        Some(value) if !value.is_empty() => value,
        _ => {
            return Err(OpError::InvalidArguments(format!(
                "Rolle fehlt. Aufruf: {usage}. Bekannte Rollen: {}.",
                known_roles()
            )));
        }
    };
    ModelRole::parse(raw).ok_or_else(|| {
        OpError::InvalidArguments(format!(
            "Unbekannte Rolle: '{raw}'. Bekannte Rollen: {}.",
            known_roles()
        ))
    })
}

/// Liefert den kanonischen Provider-Namen zu `name` (Schlüssel oder `name`
/// eines konfigurierten Providers), sofern dieser aktiviert ist.
fn enabled_provider_name(config: &ResolvedConfig, name: &str) -> Option<String> {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .filter(|(_, provider)| provider.enabled)
        .map(|(_, provider)| provider.name.clone())
}

/// Normalisiert einen Provider-Verweis aus dem Modellkatalog auf den
/// kanonischen Provider-Namen (Schlüssel → `name`); unbekannt bleibt unverändert.
pub(crate) fn canonical_provider_name(config: &ResolvedConfig, name: &str) -> String {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .map(|(_, provider)| provider.name.clone())
        .unwrap_or_else(|| name.to_owned())
}

/// Gehört der Katalogeintrag `entry` zum kanonischen Provider `provider`?
fn model_belongs_to(
    config: &ResolvedConfig,
    entry: &harw_config::ModelToml,
    provider: &str,
) -> bool {
    canonical_provider_name(config, &entry.provider) == provider
}

/// Löst eine Modellkennung (Katalog-Schlüssel, `id` oder Alias) **innerhalb
/// des Providers `provider`** zur kanonischen Modell-ID des Katalogs auf.
///
/// # Argumente
/// - `provider`: kanonischer Provider-Name (aus [`enabled_provider_name`]);
///   nur Katalogeinträge dieses Providers werden berücksichtigt.
/// - `model`: Modellteil hinter dem Provider-Präfix (bereits getrimmt).
///
/// # Beschreibung
/// Rangfolge, jeweils nur unter den Modellen von `provider`: Katalog-Schlüssel,
/// dann `id`, dann Alias. Die Auflösung ist deterministisch (keine Abhängigkeit
/// von der Iterationsreihenfolge der Konfiguration): derselbe Alias bei zwei
/// Providern liefert je Provider dessen eigene ID. Eine Kennung, die **kein**
/// Provider als Schlüssel oder Alias führt, bleibt unverändert (frei wählbare
/// Modell-ID des Providers); das gilt auch für die reine `id` eines Modells
/// eines anderen Providers, die dabei nicht umgeschrieben wird.
///
/// # Fehler
/// [`OpError::InvalidArguments`] (fail-closed, vor jeder Persistenz), wenn
/// - `model` bei `provider` mehrere verschiedene Modelle als Alias benennt
///   (mehrdeutige Konfiguration), oder
/// - `model` bei `provider` unbekannt ist, aber Katalog-Schlüssel oder Alias
///   eines Modells eines **anderen** Providers ist: sonst entstünde ein
///   Paar aus Provider A und dem Modell von Provider B.
fn canonical_model_id(
    config: &ResolvedConfig,
    provider: &str,
    model: &str,
) -> Result<String, OpError> {
    if let Some(entry) = config
        .models
        .get(model)
        .filter(|entry| model_belongs_to(config, entry, provider))
    {
        return Ok(entry.id.clone());
    }
    let own: Vec<&harw_config::ModelToml> = config
        .models
        .values()
        .filter(|entry| model_belongs_to(config, entry, provider))
        .collect();
    if own.iter().any(|entry| entry.id == model) {
        return Ok(model.to_owned());
    }
    let aliased: BTreeSet<&str> = own
        .iter()
        .filter(|entry| entry.aliases.iter().any(|alias| alias == model))
        .map(|entry| entry.id.as_str())
        .collect();
    let mut aliased = aliased.into_iter();
    match (aliased.next(), aliased.next()) {
        (Some(id), None) => return Ok(id.to_owned()),
        (Some(first), Some(second)) => {
            return Err(OpError::InvalidArguments(format!(
                "Alias '{model}' ist bei Provider '{provider}' mehrdeutig \
                 (z. B. '{first}' und '{second}'). Bitte die Modell-ID angeben."
            )));
        }
        (None, _) => {}
    }
    let others: BTreeSet<String> = config
        .models
        .iter()
        .filter(|(key, entry)| {
            (key.as_str() == model || entry.aliases.iter().any(|alias| alias == model))
                && !model_belongs_to(config, entry, provider)
        })
        .map(|(_, entry)| canonical_provider_name(config, &entry.provider))
        .collect();
    let Some(first) = others.first() else {
        return Ok(model.to_owned());
    };
    let list = others
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Err(OpError::InvalidArguments(format!(
        "'{model}' ist ein Alias bzw. Katalog-Schlüssel von Provider {list}, aber kein Modell \
         von '{provider}'. Bitte '{first}/{model}' oder eine Modell-ID von '{provider}' angeben."
    )))
}

/// Löst `target` zu `(provider, modell)` auf.
///
/// # Beschreibung
/// 1. Bekannte Modell-ID: Schlüssel in `config.models`, `id` oder Alias →
///    Provider aus dem Katalog, Modell = kanonische `id`. Passt die Kennung
///    auf mehrere verschiedene (Provider, `id`)-Paare — derselbe Alias bei
///    zwei Providern oder bei zwei Modellen eines Providers —, ist sie
///    mehrdeutig und wird abgelehnt (sonst entschiede die Iterationsreihenfolge
///    der Konfiguration); `provider/modell` löst die Mehrdeutigkeit auf.
/// 2. Sonst `provider/modell`, getrennt am ersten `/`, wenn das Präfix ein
///    aktivierter Provider ist. Der Modellteil wird **providergenau** (nur
///    Katalogmodelle dieses Providers) auf die kanonische Katalog-`id`
///    normalisiert (Schlüssel oder Alias, z. B. `mistral/legacy-ocr` →
///    `mistral`, `mistral-ocr-4-0`); eine allen Providern unbekannte Kennung
///    (frei wählbare Modell-ID des Providers) bleibt unverändert. Nur so
///    sieht die Lebenszyklus-Prüfung vor der Persistenz dieselbe ID wie die
///    spätere Laufzeitprüfung.
///
/// # Fehler
/// [`OpError::InvalidArguments`] mit deutscher Meldung, wenn beides scheitert,
/// sowie fail-closed für eine mehrdeutige unpräfixierte Kennung (die
/// Meldung nennt die Kandidaten als sortierte `provider/id`-Liste), für einen
/// Alias/Schlüssel, der nur zu einem anderen Provider gehört, und für einen
/// bei dem Provider mehrdeutigen Alias (siehe `canonical_model_id`).
pub(crate) fn resolve_target(
    config: &ResolvedConfig,
    target: &str,
) -> Result<(String, String), OpError> {
    // Alle Katalogeinträge, auf die `target` als Schlüssel, `id` oder Alias
    // passt — als (Provider, Modell-ID)-Paare, sortiert und ohne Dubletten
    // (dasselbe Modell unter zwei Schlüsseln ist eindeutig).
    let candidates: Vec<(String, String)> = config
        .models
        .iter()
        .filter(|(key, entry)| {
            key.as_str() == target
                || entry.id == target
                || entry.aliases.iter().any(|alias| alias == target)
        })
        .map(|(_, entry)| {
            (
                canonical_provider_name(config, &entry.provider),
                entry.id.clone(),
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    match candidates.as_slice() {
        [] => {}
        [(provider, model)] => return Ok((provider.clone(), model.clone())),
        several => {
            let list = several
                .iter()
                .map(|(provider, model)| format!("{provider}/{model}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(OpError::InvalidArguments(format!(
                "Mehrdeutiges Modell: '{target}' passt auf mehrere Katalogmodelle ({list}). \
                 Bitte 'provider/modell' angeben."
            )));
        }
    }
    let prefixed = target.split_once('/').and_then(|(prefix, rest)| {
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        enabled_provider_name(config, prefix.trim()).map(|provider| (provider, rest))
    });
    if let Some((provider, rest)) = prefixed {
        let model = canonical_model_id(config, &provider, rest)?;
        return Ok((provider, model));
    }
    Err(OpError::InvalidArguments(format!(
        "Unbekanntes Modell: '{target}'. Erwartet wird eine bekannte Modell-ID bzw. ein Alias \
         aus der Konfiguration oder 'provider/modell' mit einem aktivierten Provider."
    )))
}

/// Liefert den effektiven UIA-Provider: Live-/Config-UIA-Auswahl, sonst
/// `default_provider` (die UIA erbt ihn ohne eigenen Pin).
pub(crate) fn effective_uia_provider(
    controller: Option<&SharedSessionController>,
    config: &ResolvedConfig,
) -> Option<String> {
    let selection = crate::model::effective_uia_selection(controller, config);
    selection
        .provider()
        .map(str::to_owned)
        .or_else(|| config.harness.default_provider.clone())
        .map(|provider| canonical_provider_name(config, &provider))
}

/// Hängt eine Persistenz-Notiz an bzw. meldet den Erfolg.
pub(crate) fn finish_text(mut text: String, note: Option<&str>) -> String {
    match note {
        Some(note) => {
            text.push('\n');
            text.push_str(note);
        }
        None => {
            text.push_str(" — gespeichert. ");
            text.push_str(EFFECTIVE_NEXT_SESSION);
        }
    }
    text
}

/// Wie [`finish_text`], meldet aber eine live übernommene Rollenwahl.
///
/// # Argumente
/// - `live`: `true`, wenn die laufende Sitzung die Wahl bereits für neu
///   gestartete Agenten übernommen hat
///   ([`crate::live_model::LiveModelControl::set_internal_model`]).
fn finish_role_text(text: String, note: Option<&str>, live: bool) -> String {
    if !live {
        return finish_text(text, note);
    }
    let mut text = text;
    match note {
        Some(note) => {
            text.push_str(" — gilt ab sofort für neu gestartete Agenten.\n");
            text.push_str(note);
        }
        None => text.push_str(
            " — gespeichert; gilt ab sofort für neu gestartete Agenten \
             (laufende behalten ihr Modell).",
        ),
    }
    text
}

/// Zellinhalt für die Tabelle: `-` statt leer.
fn cell(value: Option<&str>) -> &str {
    value.filter(|value| !value.is_empty()).unwrap_or("-")
}

/// Baut Text und `data` für `show`.
///
/// # Argumente
/// - `rows`: aufgelöste Rollenzeilen ([`harw_config::resolve_role_models`]).
/// - `live_provider`/`live_model`: Live-Auswahl der Sitzung (Controller-Snapshot).
fn render_show(
    rows: &[RoleModelRow],
    live_provider: Option<&str>,
    live_model: Option<&str>,
) -> OpOutput {
    let headers = ["Rolle", "Provider", "Modell", "Quelle", "Effort"];
    let table: Vec<[String; 5]> = rows
        .iter()
        .map(|row| {
            [
                row.role.key().to_owned(),
                cell(row.provider.as_deref()).to_owned(),
                cell(row.model.as_deref()).to_owned(),
                row.source.label().to_owned(),
                cell(row.reasoning_effort.as_deref()).to_owned(),
            ]
        })
        .collect();

    let mut widths = headers.map(|header| header.chars().count());
    for line in &table {
        for (width, value) in widths.iter_mut().zip(line.iter()) {
            *width = (*width).max(value.chars().count());
        }
    }
    let format_line = |values: [&str; 5]| -> String {
        values
            .iter()
            .zip(widths.iter())
            .map(|(value, width)| {
                let pad = width.saturating_sub(value.chars().count());
                format!("{value}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };

    let heading = if live_provider.is_some() || live_model.is_some() {
        format!("Live (diese Sitzung): {} / {}", cell(live_provider), cell(live_model))
    } else {
        "Konfigurierte Modellwahl".to_owned()
    };
    let mut lines = vec![heading, String::new(), format_line(headers)];
    for line in &table {
        lines.push(format_line([
            line[0].as_str(),
            line[1].as_str(),
            line[2].as_str(),
            line[3].as_str(),
            line[4].as_str(),
        ]));
    }
    lines.push(String::new());
    lines.push(format!(
        "{EFFECTIVE_NEXT_SESSION} Kind-Rollen (Orchestrator, Worker, Explorer, Recherche, \
         Gedächtnis) gelten sofort für neu gestartete Agenten. \
         Ändern: /models set <rolle> <modell|provider/modell>, \
         /models reset <rolle>."
    ));

    let roles: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "role": row.role.key(),
                "label": row.role.label(),
                "provider": row.provider,
                "model": row.model,
                "source": row.source.label(),
                "effort": row.reasoning_effort,
            })
        })
        .collect();
    let data = json!({
        "roles": roles,
        "live": { "provider": live_provider, "model": live_model },
        "note": EFFECTIVE_NEXT_SESSION,
    });
    OpOutput {
        text: lines.join("\n"),
        data: Some(data),
    }
}

/// Explicit image choices require declared vision capability before persistence.
fn validate_image_target(
    config: &ResolvedConfig,
    provider: &str,
    model: &str,
) -> Result<(), OpError> {
    let configured = config.models.values().find(|entry| {
        entry.id == model && canonical_provider_name(config, &entry.provider) == provider
    });
    let image_input = configured
        .and_then(|entry| entry.capabilities.image_input)
        .or_else(|| {
            configured
                .filter(|entry| entry.input_types.iter().any(|kind| kind == "image"))
                .map(|_| true)
        })
        .or_else(|| {
            harw_model_catalog::descriptor::bootstrap_descriptors()
                .into_iter()
                .find(|entry| entry.provider.as_str() == provider && entry.model.as_str() == model)
                .map(|entry| entry.capabilities.image_input)
        });
    if image_input != Some(true) {
        return Err(OpError::InvalidArguments(format!(
            "{provider}/{model} ist kein bestätigtes Bildmodell. Wähle ein Modell mit image_input = true; GLM-5.3 unterstützt nur Text."
        )));
    }
    Ok(())
}

/// Führt `set <rolle> <ziel> [effort]` aus.
fn handle_set(
    ctx: &OpContext,
    config: &ResolvedConfig,
    role: ModelRole,
    target: &str,
    effort_level: Option<harw_types::ReasoningEffort>,
) -> Result<OpOutput, OpError> {
    let (provider, model) = resolve_target(config, target)?;
    if role == ModelRole::ImageUnderstanding {
        validate_image_target(config, &provider, &model)?;
    }
    // Lebenszyklus der ausdrücklichen Wahl, noch vor Provider-Bau und
    // Persistenz: ein abgeschaltetes Modell wird abgelehnt und lässt Profil-
    // Config und laufende Montage unverändert, ein abgekündigtes wird mit
    // Warnung gespeichert.
    let lifecycle_warning = crate::live_model::enforce_lifecycle(&provider, &model)?;
    let persistence = crate::config_util::selection_persistence(ctx);
    // Live-Übernahme: nur eine Stelle, deren Provider jetzt erreichbar ist,
    // wird überhaupt gespeichert (sonst bliebe eine unbrauchbare Wahl stehen).
    if role.internal_point().is_some() {
        crate::live_model::ensure_provider_ready(ctx, &provider)?;
    }
    let mut live = false;

    let note = match role {
        ModelRole::Uia => {
            persistence.persist_uia_selection(Some(provider.as_str()), Some(model.as_str()))
        }
        // Runde 5, Teil G: keine Kopplung an den UIA-Provider mehr — die
        // Rolle `uia-worker` bekommt eine eigene Wahl mit eigenem Provider.
        ModelRole::UiaWorker => persistence.persist_uia_worker_role_model(
            "uia-worker",
            Some(format!("{provider}/{model}").as_str()),
        ),
        other => {
            let point = other.internal_point().ok_or_else(|| {
                OpError::InvalidArguments(format!(
                    "Für die Rolle '{}' ist keine eigene Modellwahl vorgesehen.",
                    other.key()
                ))
            })?;
            let note = persistence.persist_internal_model(
                point,
                Some(provider.as_str()),
                Some(model.as_str()),
            );
            live = crate::live_model::apply_internal_model(
                ctx,
                point,
                Some(harw_config::InternalModelChoice {
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                }),
            );
            note
        }
    };

    // Optionales Reasoning-Gewicht zusätzlich verankern (bestes Bemühen);
    // nur Rollen mit Gewichtsfeld erreichen diese Stelle überhaupt.
    let effort_note = effort_level.and_then(|level| {
        persistence.persist_role_reasoning_effort(role.key(), Some(level.to_string()).as_deref())
    });

    let note = match (note, effort_note) {
        (_, Some(extra)) => Some(extra),
        (base, None) => base,
    };

    let mut text = finish_role_text(
        format!("Modell für {} gesetzt: {provider}/{model}", role.label()),
        note.as_deref(),
        live,
    );
    if let Some(warning) = &lifecycle_warning {
        text.push_str("\nWarnung: ");
        text.push_str(warning);
    }
    Ok(OpOutput {
        text,
        data: Some(json!({
            "action": "set",
            "ui_feedback": {"kind":"role_model", "action":"set", "role":role.key(), "provider":provider, "model":model, "live":live, "effective":if live {"new_agents"} else {"next_session"}, "warning":lifecycle_warning, "note":note},
            "source": "explizit gewählt",
            "role": role.key(),
            "provider": provider,
            "model": model,
            "persisted": note.is_none(),
            "note": note,
        })),
    })
}

/// TOML-Feldname des Reasoning-Gewichts für eine Rolle (siehe
/// `harw-config::resolve_effort`); `None` für Rollen ohne Gewichtsfeld.
fn role_effort_field(role_key: &str) -> Option<&'static str> {
    match role_key {
        "uia" => Some("reasoning.uia"),
        "root-orchestrator" => Some("reasoning.root_orchestrator"),
        "sub-orchestrator" => Some("reasoning.sub_orchestrator"),
        "worker-simple" => Some("reasoning.worker_simple"),
        "worker-complex" => Some("reasoning.worker_complex"),
        _ => None,
    }
}

/// Führt `reset <rolle>` aus.
fn handle_reset(ctx: &OpContext, role: ModelRole) -> Result<OpOutput, OpError> {
    let persistence = crate::config_util::selection_persistence(ctx);
    let mut live = false;
    let note = match role {
        ModelRole::Uia => persistence.clear_uia_selection(),
        // Runde 5, Teil G: eigener Eintrag und alter Familien-Pin weg → die
        // Rolle folgt wieder der UIA.
        ModelRole::UiaWorker => {
            let role_note = persistence.persist_uia_worker_role_model("uia-worker", None);
            let legacy_note = persistence.persist_uia_worker_model(None);
            role_note.or(legacy_note)
        }
        other => {
            let point = other.internal_point().ok_or_else(|| {
                OpError::InvalidArguments(format!(
                    "Für die Rolle '{}' gibt es keine eigene Modellwahl zum Zurücksetzen.",
                    other.key()
                ))
            })?;
            let note = persistence.persist_internal_model(point, None, None);
            live = crate::live_model::apply_internal_model(ctx, point, None);
            note
        }
    };

    let text = finish_role_text(
        format!("Modellwahl für {} zurückgesetzt", role.label()),
        note.as_deref(),
        live,
    );
    Ok(OpOutput {
        text,
        data: Some(json!({
            "action": "reset",
            "ui_feedback": {"kind":"role_model", "action":"reset", "role":role.key(), "provider":null, "model":null, "live":live, "effective":if live {"new_agents"} else {"next_session"}, "warning":null, "note":note},
            "role": role.key(),
            "provider": Value::Null,
            "model": Value::Null,
            "persisted": note.is_none(),
            "note": note,
        })),
    })
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Zeigt oder ändert das Modell je Rolle.
///
/// # Beschreibung
/// Siehe Moduldoku für Grammatik und Datenvertrag. `show` liest die
/// aufgelöste Config (context-scoped zuerst, siehe
/// [`crate::provider::resolved_config`]) und den Live-Snapshot des
/// [`SharedSessionController`] (falls registriert).
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: unbekanntes Sub-Kommando, Rolle oder Ziel.
/// - [`OpError::Execution`]: Config-Discovery fehlgeschlagen.
///
/// # Panics
/// Nie.
#[operation(
    name = "models",
    summary = "Zeigt/setzt das Modell je Rolle (UIA, Worker, Orchestrator, interne Stellen); wirkt ab der nächsten Sitzung.",
    domain = "catalog_config",
    permission = "operator",
    category = "model",
    command(path = "/models", visibility = "tui_only", busy = "immediate")
)]
async fn models(ctx: &OpContext, args: ModelsArgs) -> Result<OpOutput, OpError> {
    let action = args
        .action
        .as_deref()
        .map(str::trim)
        .filter(|action| !action.is_empty())
        .unwrap_or("show");

    match action {
        "image" => {
            if args.target.is_some() || args.effort.is_some() {
                return Err(OpError::InvalidArguments(
                    "Aufruf: /models image [show|reset|modell|provider/modell]".to_owned(),
                ));
            }
            match args
                .role
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some("reset" | "clear") => handle_reset(ctx, ModelRole::ImageUnderstanding),
                None | Some("show" | "list") => {
                    let config = crate::provider::resolved_config(ctx)?;
                    let row =
                        harw_config::resolve_role_model(&config, ModelRole::ImageUnderstanding);
                    Ok(render_show(&[row], None, None))
                }
                Some(target) => {
                    let config = crate::provider::resolved_config(ctx)?;
                    handle_set(ctx, &config, ModelRole::ImageUnderstanding, target, None)
                }
            }
        }
        "show" | "list" | "profile" => {
            let config = crate::provider::resolved_config(ctx)?;
            let rows = harw_config::resolve_role_models(&config);
            // The local shell has no real chat controller. The remote allowlist
            // does not expose this profile-only presentation verb.
            let snapshot = if action == "profile" { None } else {
                ctx.service::<SharedSessionController>().map(|controller| controller.snapshot())
            };
            let live_provider = snapshot
                .as_ref()
                .and_then(|snap| snap.active_provider.as_deref());
            let live_model = snapshot
                .as_ref()
                .and_then(|snap| snap.active_model.as_deref());
            Ok(render_show(&rows, live_provider, live_model))
        }
        "set" => {
            let usage = "/models set <rolle> <modell|provider/modell> [effort]";
            let role = parse_role(args.role.as_deref(), usage)?;
            let target = match args.target.as_deref().map(str::trim) {
                Some(target) if !target.is_empty() => target.to_owned(),
                _ => {
                    return Err(OpError::InvalidArguments(format!(
                        "Ziel fehlt. Aufruf: {usage}."
                    )));
                }
            };
            // Optionales Reasoning-Gewicht: nur Rollen mit eigenem
            // Gewichtsfeld (siehe harw-config resolve_effort) akzeptieren
            // einen Wert; das Level wird wie in /effort validiert.
            let effort_level = parse_role_effort(args.effort.as_deref(), role.key())?;
            let config = crate::provider::resolved_config(ctx)?;
            handle_set(ctx, &config, role, &target, effort_level)
        }
        "reset" | "clear" => {
            let role = parse_role(args.role.as_deref(), "/models reset <rolle>")?;
            handle_reset(ctx, role)
        }
        // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
        "worker" | "workers" => {
            let config = crate::provider::resolved_config(ctx)?;
            crate::models_workers::handle_worker(
                ctx,
                &config,
                args.role.as_deref(),
                args.target.as_deref(),
            )
        }
        "add" => {
            let config = crate::provider::resolved_config(ctx)?;
            handle_add(ctx, &config, args.role.as_deref())
        }
        "remove" => {
            let config = crate::provider::resolved_config(ctx)?;
            handle_remove(ctx, &config, args.role.as_deref())
        }
        // Delegiert an den URL-Guard von manage-guard (Vertrag O-1); bis dahin
        // nicht verfügbar.
        "scan" => Err(OpError::NotAvailable(
            "scan folgt mit dem URL-Guard".to_owned(),
        )),
        other => Err(OpError::InvalidArguments(format!(
            "Unbekanntes /models-Sub-Kommando: '{other}'. Gültig: show, image [show|reset|modell|provider/modell], \
             set <rolle> <modell|provider/modell>, reset <rolle>, \
             worker [<rolle|all> <uia|modell|provider/modell>]."
        ))),
    }
}

// ── Katalog-Sicht für `catalog.get` (Scheibe 1) ──────────────────────────────

/// Geschwärzte Sicht auf Provider, Modelle, Rollen und Live-Modell.
///
/// Enthält nie: `api_key`, `headers` (Namen und Werte), `auth_header`,
/// Secret-Werte, Dateipfade von `SecretRef::File`/`FileJson`, Userinfo, Query
/// oder Fragment der Basis-URL.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CatalogView {
    /// Provider, sortiert nach Name.
    pub providers: Vec<ProviderView>,
    /// Modelle, sortiert nach (Provider, Id).
    pub models: Vec<ModelView>,
    /// Rollen in der Reihenfolge von [`harw_config::resolve_role_models`].
    pub roles: Vec<RoleView>,
    /// Live-Modell der Sitzung.
    pub live: LiveView,
    /// Hinweis [`EFFECTIVE_NEXT_SESSION`].
    pub note: &'static str,
}

/// Ein Provider ohne Geheimnisse.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderView {
    /// Provider-Name.
    pub name: String,
    /// API-Familie.
    pub api: String,
    /// `scheme://host[:port]/path`; nicht parsebar: leer.
    pub base_url: String,
    /// Aktiviert?
    pub enabled: bool,
    /// Art der Anmeldung (ohne Wert).
    pub auth_kind: AuthKind,
    /// Nur bei `Env`, `Keyring`, `Secrets`: Name, Eintrag bzw. Id.
    pub auth_name: Option<String>,
    /// `auth.is_some() || api_key.is_some()`.
    pub has_secret: bool,
    /// Ausgewählte Modell-Ids.
    pub models: Vec<String>,
    /// Obergrenze gleichzeitiger Requests.
    pub max_concurrency: Option<usize>,
}

/// Art der Anmeldung eines Providers; nie der Wert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// Keine Anmeldung.
    None,
    /// `env:`-Referenz.
    Env,
    /// `file:`-Referenz (Pfad wird nie gezeigt).
    File,
    /// `keyring:`-Referenz.
    Keyring,
    /// `secrets:`-Referenz.
    Secrets,
    /// `file-json:`-Referenz (Pfad wird nie gezeigt).
    FileJson,
    /// Veralteter Klartext-`api_key`.
    InlineKey,
}

/// Ein Katalogmodell.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ModelView {
    /// Schlüssel in `ResolvedConfig::models`.
    pub key: String,
    /// Modell-Id beim Provider.
    pub id: String,
    /// Anzeigename.
    pub name: Option<String>,
    /// Kanonischer Provider-Name.
    pub provider: String,
    /// Aliase.
    pub aliases: Vec<String>,
    /// Kontextfenster.
    pub context_window: Option<u64>,
    /// Maximale Ausgabe-Tokens.
    pub max_tokens: Option<u64>,
    /// Reasoning-fähig?
    pub reasoning: bool,
    /// Eingabearten.
    pub input_types: Vec<String>,
    /// `"active" | "deprecated" | "retirement_window" | "retired" | "unknown"`.
    pub lifecycle: &'static str,
}

/// Eine Rolle mit aufgelöstem Modell.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RoleView {
    /// Rollenschlüssel.
    pub role: &'static str,
    /// Anzeigename der Rolle.
    pub label: &'static str,
    /// Provider.
    pub provider: Option<String>,
    /// Modell.
    pub model: Option<String>,
    /// Herkunft der Wahl.
    pub source: &'static str,
    /// Reasoning-Gewicht.
    pub effort: Option<String>,
}

/// Live-Modell der Sitzung.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LiveView {
    /// Aktiver Provider.
    pub provider: Option<String>,
    /// Aktives Modell.
    pub model: Option<String>,
}

/// Schwärzt eine Basis-URL auf `scheme://host[:port]/path`.
///
/// Ohne neue Abhängigkeit von Hand: Schema bis `://`, Autorität bis zum
/// ersten `/`, `?` oder `#`; Userinfo (bis zum letzten `@`) entfällt; der Pfad
/// endet vor `?`/`#`. Fehlt `://` oder der Host, ist das Ergebnis leer.
fn redact_base_url(raw: &str) -> String {
    let Some((scheme, rest)) = raw.trim().split_once("://") else {
        return String::new();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if scheme.is_empty() || host.is_empty() {
        return String::new();
    }
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    format!("{scheme}://{host}{}", &tail[..path_end])
}

fn auth_view(provider: &harw_config::ProviderToml) -> (AuthKind, Option<String>) {
    use harw_config::SecretRef;
    match (&provider.auth, &provider.api_key) {
        (Some(SecretRef::Env(name)), _) => (AuthKind::Env, Some(name.clone())),
        (Some(SecretRef::Keyring(name)), _) => (AuthKind::Keyring, Some(name.clone())),
        (Some(SecretRef::Secrets(id)), _) => (AuthKind::Secrets, Some(id.clone())),
        (Some(SecretRef::File(_)), _) => (AuthKind::File, None),
        (Some(SecretRef::FileJson { .. }), _) => (AuthKind::FileJson, None),
        (None, Some(_)) => (AuthKind::InlineKey, None),
        (None, None) => (AuthKind::None, None),
    }
}

fn lifecycle_label(
    entries: &[harw_model_catalog::lifecycle::LifecycleEntry],
    provider: &str,
    model: &str,
    now: harw_model_catalog::lifecycle::OffsetDateTime,
) -> &'static str {
    use harw_model_catalog::lifecycle::{LifecycleStatus, lookup};
    match lookup(entries, provider, model).map(|entry| entry.lifecycle.status(now)) {
        Some(LifecycleStatus::Active) => "active",
        Some(LifecycleStatus::Deprecated) => "deprecated",
        Some(LifecycleStatus::RetirementWindow) => "retirement_window",
        Some(LifecycleStatus::Retired) => "retired",
        None => "unknown",
    }
}

/// Baut die geschwärzte Katalogsicht. Rein; `now` bestimmt den Lebenszyklus,
/// die Live-Werte liefert der Aufrufer (Controller-Snapshot).
#[must_use]
pub fn catalog_view(
    config: &ResolvedConfig,
    live_provider: Option<&str>,
    live_model: Option<&str>,
    now: harw_model_catalog::lifecycle::OffsetDateTime,
) -> CatalogView {
    let mut providers: Vec<ProviderView> = config
        .providers
        .values()
        .map(|provider| {
            let (auth_kind, auth_name) = auth_view(provider);
            ProviderView {
                name: provider.name.clone(),
                api: provider.api.clone(),
                base_url: redact_base_url(&provider.base_url),
                enabled: provider.enabled,
                auth_kind,
                auth_name,
                has_secret: provider.auth.is_some() || provider.api_key.is_some(),
                models: provider.models.clone(),
                max_concurrency: provider.max_concurrency,
            }
        })
        .collect();
    providers.sort_by(|a, b| a.name.cmp(&b.name));

    let entries = harw_model_catalog::lifecycle::bundled_lifecycles();
    let mut models: Vec<ModelView> = config
        .models
        .iter()
        .map(|(key, entry)| {
            let provider = canonical_provider_name(config, &entry.provider);
            let lifecycle = lifecycle_label(&entries, &provider, &entry.id, now);
            ModelView {
                key: key.clone(),
                id: entry.id.clone(),
                name: entry.name.clone(),
                provider,
                aliases: entry.aliases.clone(),
                context_window: entry.context_window,
                max_tokens: entry.max_tokens,
                reasoning: entry.reasoning,
                input_types: entry.input_types.clone(),
                lifecycle,
            }
        })
        .collect();
    models.sort_by(|a, b| (&a.provider, &a.id, &a.key).cmp(&(&b.provider, &b.id, &b.key)));

    let roles = harw_config::resolve_role_models(config)
        .into_iter()
        .map(|row| RoleView {
            role: row.role.key(),
            label: row.role.label(),
            provider: row.provider,
            model: row.model,
            source: row.source.label(),
            effort: row.reasoning_effort,
        })
        .collect();

    CatalogView {
        providers,
        models,
        roles,
        live: LiveView {
            provider: live_provider.map(str::to_owned),
            model: live_model.map(str::to_owned),
        },
        note: EFFECTIVE_NEXT_SESSION,
    }
}

/// Bequemer Einstieg: aufgelöste Config, Controller-Snapshot und aktuelle Zeit.
///
/// # Fehler
/// Wie [`crate::provider::resolved_config`].
pub fn catalog_view_for(ctx: &OpContext) -> Result<CatalogView, OpError> {
    let config = crate::provider::resolved_config(ctx)?;
    let snapshot = ctx
        .service::<SharedSessionController>()
        .map(|controller| controller.snapshot());
    Ok(catalog_view(
        &config,
        snapshot
            .as_ref()
            .and_then(|snap| snap.active_provider.as_deref()),
        snapshot
            .as_ref()
            .and_then(|snap| snap.active_model.as_deref()),
        harw_model_catalog::lifecycle::OffsetDateTime::now_utc(),
    ))
}

// ── Scheibe 3: add / remove ──────────────────────────────────────────────────

/// Kanonischer Step-up-Text einer Änderung, Grundlage für Hash/Nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelsChange {
    /// `"set" | "reset" | "image" | "worker" | "add" | "remove"`.
    pub action: &'static str,
    /// `"models.{action} {ziel}: {alt} -> {neu}"`.
    pub canonical: String,
}

fn change(action: &'static str, target: &str, old: &str, new: &str) -> ModelsChange {
    ModelsChange {
        action,
        canonical: format!("models.{action} {target}: {old} -> {new}"),
    }
}

fn role_value(config: &ResolvedConfig, role: ModelRole) -> String {
    let row = harw_config::resolve_role_model(config, role);
    match (row.provider, row.model) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        _ => "-".to_owned(),
    }
}

/// Prüft `set`/`image` wie die Ausführung (Ziel, Bildfähigkeit, Lebenszyklus),
/// ohne zu schreiben; liefert `(provider, modell)`.
fn check_set_target(
    config: &ResolvedConfig,
    role: ModelRole,
    target: &str,
) -> Result<(String, String), OpError> {
    let (provider, model) = resolve_target(config, target)?;
    if role == ModelRole::ImageUnderstanding {
        validate_image_target(config, &provider, &model)?;
    }
    crate::live_model::enforce_lifecycle(&provider, &model)?;
    Ok((provider, model))
}

/// Plant eine Änderung rein, ohne Schreiben.
///
/// Prüft wie `handle_set`/`resolve_target`/`enforce_lifecycle` und liefert den
/// kanonischen Text, z. B. `"models.set uia: prov-a/m1 -> prov-b/m2"`,
/// `"models.add prov-a/m3: absent -> present"`. Fehlender Altwert: `-`. Bei
/// `reset` ist der Neuwert `default`; bei `worker` ist der Altwert `-` und der
/// Neuwert das rohe Ziel.
///
/// # Fehler
/// [`OpError::InvalidArguments`] für Nicht-Änderungen (`show`, unbekannte
/// Sub-Kommandos) und alles, was die Ausführung ablehnen würde.
pub fn plan_models_change(ctx: &OpContext, args: &[String]) -> Result<ModelsChange, OpError> {
    let parsed = ModelsArgs::from_raw_args(args)?;
    let action = parsed
        .action
        .as_deref()
        .map(str::trim)
        .filter(|action| !action.is_empty())
        .unwrap_or("show");
    let config = crate::provider::resolved_config(ctx)?;
    match action {
        "set" => {
            let usage = "/models set <rolle> <modell|provider/modell> [effort]";
            let role = parse_role(parsed.role.as_deref(), usage)?;
            let target = parsed
                .target
                .as_deref()
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .ok_or_else(|| {
                    OpError::InvalidArguments(format!("Ziel fehlt. Aufruf: {usage}."))
                })?;
            parse_role_effort(parsed.effort.as_deref(), role.key())?;
            let (provider, model) = check_set_target(&config, role, target)?;
            Ok(change(
                "set",
                role.key(),
                &role_value(&config, role),
                &format!("{provider}/{model}"),
            ))
        }
        "reset" | "clear" => {
            let role = parse_role(parsed.role.as_deref(), "/models reset <rolle>")?;
            Ok(change(
                "reset",
                role.key(),
                &role_value(&config, role),
                "default",
            ))
        }
        "image" => {
            let role = ModelRole::ImageUnderstanding;
            match parsed
                .role
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some("reset" | "clear") => Ok(change(
                    "image",
                    role.key(),
                    &role_value(&config, role),
                    "default",
                )),
                None | Some("show" | "list") => Err(OpError::InvalidArguments(
                    "/models image show ändert nichts".to_owned(),
                )),
                Some(target) => {
                    let (provider, model) = check_set_target(&config, role, target)?;
                    Ok(change(
                        "image",
                        role.key(),
                        &role_value(&config, role),
                        &format!("{provider}/{model}"),
                    ))
                }
            }
        }
        "worker" | "workers" => {
            let role = parsed
                .role
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    OpError::InvalidArguments(
                        "Aufruf: /models worker <rolle|all> <uia|modell|provider/modell>"
                            .to_owned(),
                    )
                })?;
            let target = parsed
                .target
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    OpError::InvalidArguments(
                        "Aufruf: /models worker <rolle|all> <uia|modell|provider/modell>"
                            .to_owned(),
                    )
                })?;
            Ok(change("worker", role, "-", target))
        }
        "add" => {
            let edit = prepare_list_edit(ctx, &config, parsed.role.as_deref(), true)?;
            let old = if edit.current.contains(&edit.model) {
                "present"
            } else {
                "absent"
            };
            Ok(change(
                "add",
                &format!("{}/{}", edit.provider, edit.model),
                old,
                "present",
            ))
        }
        "remove" => {
            let edit = prepare_list_edit(ctx, &config, parsed.role.as_deref(), false)?;
            let old = if edit.current.contains(&edit.model) {
                "present"
            } else {
                "absent"
            };
            Ok(change(
                "remove",
                &format!("{}/{}", edit.provider, edit.model),
                old,
                "absent",
            ))
        }
        other => Err(OpError::InvalidArguments(format!(
            "Keine Änderung: '{other}'."
        ))),
    }
}

/// Vorbereitete Änderung der Modellliste einer Provider-Datei.
struct ListEdit {
    /// Kanonischer Provider-Name.
    provider: String,
    /// Modell-Id.
    model: String,
    /// `<profil>/providers/<schlüssel>.toml`.
    path: std::path::PathBuf,
    /// Aktuelle Liste (Datei, sonst Config).
    current: BTreeSet<String>,
    /// Profilverzeichnis.
    profile_dir: std::path::PathBuf,
}

fn io_error(context: &str, error: impl std::fmt::Display) -> OpError {
    OpError::Execution(format!("{context}: {error}"))
}

/// Zerlegt `provider/modell` am ersten `/`.
fn parse_list_target(target: Option<&str>) -> Result<(&str, &str), OpError> {
    let usage = "Aufruf: /models add|remove <provider>/<modell>";
    let target = target
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::InvalidArguments(format!("Ziel fehlt. {usage}")))?;
    match target.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            Ok((provider, model))
        }
        _ => Err(OpError::InvalidArguments(format!(
            "Ungültiges Ziel '{target}'. {usage}"
        ))),
    }
}

/// Provider-Schlüssel als sicherer Dateiname (kein Pfadtrenner, kein führender Punkt).
fn is_safe_file_stem(stem: &str) -> bool {
    !stem.is_empty()
        && !stem.starts_with('.')
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Liest die aktuelle `models`-Liste der Provider-Datei; fehlt der Schlüssel,
/// gilt die aufgelöste Config (`fallback`).
fn read_current_models(
    path: &std::path::Path,
    fallback: &[String],
) -> Result<BTreeSet<String>, OpError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpError::InvalidArguments(
                "Der Provider hat keine eigene Datei im Profil (providers/<name>.toml).".to_owned(),
            ));
        }
        Err(error) => return Err(io_error("Provider-Datei lesen", error)),
    };
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| io_error("Provider-Datei lesen", error))?;
    Ok(
        match document.get("models").and_then(|item| item.as_array()) {
            Some(array) => array
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
            None => fallback.iter().cloned().collect(),
        },
    )
}

/// Schreibt die sortierte Liste; andere Schlüssel der Datei bleiben erhalten.
fn write_model_list(path: &std::path::Path, ids: &BTreeSet<String>) -> Result<(), OpError> {
    let mut writer = harw_config::ConfigWriter::open_locked(path)
        .map_err(|error| io_error("Provider-Datei öffnen", error))?;
    let mut array = toml_edit::Array::new();
    for id in ids {
        array.push(id.as_str());
    }
    writer
        .set_value("models", toml_edit::value(array))
        .map_err(|error| io_error("Provider-Datei ändern", error))?;
    writer
        .save()
        .map_err(|error| io_error("Provider-Datei speichern", error))
}

/// Prüft Ziel, Provider und (beim Hinzufügen) Katalog; ohne Schreiben.
fn prepare_list_edit(
    ctx: &OpContext,
    config: &ResolvedConfig,
    target: Option<&str>,
    adding: bool,
) -> Result<ListEdit, OpError> {
    let (provider_name, model_id) = parse_list_target(target)?;
    let (key, provider) = config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == provider_name || provider.name == provider_name)
        .ok_or_else(|| {
            OpError::InvalidArguments(format!(
                "Provider '{provider_name}' ist nicht konfiguriert."
            ))
        })?;
    if adding && !provider.enabled {
        return Err(OpError::InvalidArguments(format!(
            "Provider '{provider_name}' ist deaktiviert."
        )));
    }
    let canonical = provider.name.clone();
    if adding
        && !config
            .models
            .values()
            .any(|entry| entry.id == model_id && model_belongs_to(config, entry, &canonical))
    {
        return Err(OpError::InvalidArguments(format!(
            "Modell '{model_id}' steht nicht im Katalog von '{provider_name}'."
        )));
    }
    if !is_safe_file_stem(key) {
        return Err(OpError::InvalidArguments(format!(
            "Provider-Schlüssel '{key}' ist als Dateiname ungeeignet."
        )));
    }
    let profile_dir = crate::config_util::bound_home(ctx)?.profile_dir.clone();
    let path = profile_dir.join("providers").join(format!("{key}.toml"));
    let current = read_current_models(&path, &provider.models)?;
    Ok(ListEdit {
        provider: canonical,
        model: model_id.to_owned(),
        path,
        current,
        profile_dir,
    })
}

/// Dateiname einer Modelldatei: Bytes außerhalb `[A-Za-z0-9._-]` als `%XX`,
/// Endung `.toml` (wie `harw models`).
fn model_filename(id: &str) -> String {
    use std::fmt::Write as _;
    let mut name = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            name.push(char::from(byte));
        } else {
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name.push_str(".toml");
    name
}

/// Führt `add <provider>/<modell>` aus.
fn handle_add(
    ctx: &OpContext,
    config: &ResolvedConfig,
    target: Option<&str>,
) -> Result<OpOutput, OpError> {
    let edit = prepare_list_edit(ctx, config, target, true)?;
    let mut next = edit.current.clone();
    next.insert(edit.model.clone());
    write_model_list(&edit.path, &next)?;
    Ok(OpOutput {
        text: format!(
            "{}/{} hinzugefügt. {EFFECTIVE_NEXT_SESSION}",
            edit.provider, edit.model
        ),
        data: Some(json!({
            "action": "add",
            "provider": edit.provider,
            "model": edit.model,
            "persisted": true,
            "note": EFFECTIVE_NEXT_SESSION,
        })),
    })
}

/// Führt `remove <provider>/<modell>` aus; löscht die Modelldatei nur, wenn
/// sie eine reguläre Datei und kein Symlink ist.
fn handle_remove(
    ctx: &OpContext,
    config: &ResolvedConfig,
    target: Option<&str>,
) -> Result<OpOutput, OpError> {
    let edit = prepare_list_edit(ctx, config, target, false)?;
    let mut next = edit.current.clone();
    next.remove(&edit.model);
    write_model_list(&edit.path, &next)?;
    let model_path = edit
        .profile_dir
        .join("models")
        .join(model_filename(&edit.model));
    let mut file_removed = false;
    if let Ok(metadata) = std::fs::symlink_metadata(&model_path)
        && metadata.file_type().is_file()
        && !metadata.file_type().is_symlink()
    {
        std::fs::remove_file(&model_path)
            .map_err(|error| io_error("Modelldatei löschen", error))?;
        file_removed = true;
    }
    Ok(OpOutput {
        text: format!(
            "{}/{} entfernt. {EFFECTIVE_NEXT_SESSION}",
            edit.provider, edit.model
        ),
        data: Some(json!({
            "action": "remove",
            "provider": edit.provider,
            "model": edit.model,
            "persisted": true,
            "model_file_removed": file_removed,
            "note": EFFECTIVE_NEXT_SESSION,
        })),
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModelsArgs, ModelsOperation, resolve_target};
    use crate::config_util::{
        RecordedSelectionPersistCall, RecordingSelectionPersistence, SelectionPersistence,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_config::{InternalModelPoint, ModelRole, ResolvedConfig};
    use harw_operations::context::ServiceMap;
    use harw_operations::{
        FromRawArgs, NullSessionController, OpContext, OpError, OpOutput, Operation,
        SessionController, SharedSessionController, Surface,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn provider(name: &str, enabled: bool) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: format!("https://api.example.test/{name}"),
            auth: None,
            auth_header: None,
            api_key: None,
            headers: Default::default(),
            models: Vec::new(),
            enabled,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        }
    }

    fn model(id: &str, provider_name: &str, aliases: &[&str]) -> harw_config::ModelToml {
        harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: id.to_owned(),
            name: None,
            provider: provider_name.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            context_window: None,
            max_tokens: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            prompt_caching: None,
            default_reasoning_effort: None,
        }
    }

    /// Zwei aktivierte Provider (`provider-a`, `openrouter`), ein
    /// deaktivierter (`off`), zwei Katalogmodelle.
    fn test_config() -> ResolvedConfig {
        let mut config = ResolvedConfig::default();
        config
            .providers
            .insert("provider-a".to_owned(), provider("provider-a", true));
        config
            .providers
            .insert("openrouter".to_owned(), provider("openrouter", true));
        config
            .providers
            .insert("off".to_owned(), provider("off", false));
        config.models.insert(
            "model-a".to_owned(),
            model("model-a-2026", "provider-a", &["a-latest"]),
        );
        config.models.insert(
            "router-model".to_owned(),
            model("vendor/router-model", "openrouter", &[]),
        );
        config
    }

    /// [`test_config`] plus `provider-b` und Katalogmodelle, die denselben
    /// Alias (`shared`) bei **zwei** Providern führen, sowie einen Alias
    /// (`b-only`) und einen Katalog-Schlüssel (`b-key`), die nur zu
    /// `provider-b` gehören.
    fn shared_alias_config() -> ResolvedConfig {
        let mut config = test_config();
        config
            .providers
            .insert("provider-b".to_owned(), provider("provider-b", true));
        config.models.insert(
            "model-a-shared".to_owned(),
            model("model-a-shared-id", "provider-a", &["shared"]),
        );
        config.models.insert(
            "model-b-shared".to_owned(),
            model("model-b-shared-id", "provider-b", &["shared", "b-only"]),
        );
        config.models.insert(
            "b-key".to_owned(),
            model("model-b-key-id", "provider-b", &[]),
        );
        config
    }

    struct Fixture {
        ctx: OpContext,
        recorder: Arc<RecordingSelectionPersistence>,
        root: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn fixture(
        config: ResolvedConfig,
        controller: Option<SharedSessionController>,
    ) -> TestResult<Fixture> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-models-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());

        let recorder = Arc::new(RecordingSelectionPersistence::new());
        let persistence: Arc<dyn SelectionPersistence> = recorder.clone();
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(config));
        if let Some(controller) = controller {
            services.insert(controller);
        }
        Ok(Fixture {
            ctx: OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            recorder,
            root,
        })
    }

    async fn run(fixture: &Fixture, tokens: &[&str]) -> Result<OpOutput, OpError> {
        let args = ModelsArgs::from_raw_args(&toks(tokens))?;
        super::models(&fixture.ctx, args).await
    }

    fn data(output: &OpOutput) -> TestResult<&serde_json::Value> {
        output
            .data
            .as_ref()
            .ok_or(TestError::Missing("OpOutput.data"))
    }

    #[tokio::test]
    async fn image_shortcut_persists_vision_selection_and_reset() -> TestResult {
        let mut config = test_config();
        let mut vision = model("vision-model", "provider-a", &["vision"]);
        vision.capabilities.image_input = Some(true);
        config.models.insert("vision".to_owned(), vision);
        let fixture = fixture(config, None)?;
        let output = run(&fixture, &["image", "vision"])
            .await
            .map_err(ctx("image set"))?;
        assert_eq!(data(&output)?["role"], "image-understanding");
        assert_eq!(data(&output)?["provider"], "provider-a");
        assert_eq!(data(&output)?["model"], "vision-model");
        run(&fixture, &["image", "reset"])
            .await
            .map_err(ctx("image reset"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::ImageUnderstanding,
                    provider: Some("provider-a".to_owned()),
                    model: Some("vision-model".to_owned()),
                },
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::ImageUnderstanding,
                    provider: None,
                    model: None,
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn image_role_rejects_unknown_or_text_only_without_persistence() -> TestResult {
        let mut config = test_config();
        config
            .providers
            .insert("zai".to_owned(), provider("zai", true));
        let fixture = fixture(config, None)?;
        for tokens in [
            vec!["image", "provider-a/model-a-2026"],
            vec!["set", "image-understanding", "zai/glm-5.3"],
        ] {
            assert!(matches!(
                run(&fixture, &tokens).await,
                Err(OpError::InvalidArguments(_))
            ));
        }
        assert!(fixture.recorder.calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn image_show_reports_single_role_and_source() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        let output = run(&fixture, &["image", "show"])
            .await
            .map_err(ctx("image show"))?;
        let rows = data(&output)?["roles"]
            .as_array()
            .ok_or(TestError::Missing("roles"))?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["role"], "image-understanding");
        assert!(rows[0].get("source").is_some());
        Ok(())
    }

    #[test]
    fn models_args_from_raw_args_maps_three_positions() -> TestResult {
        let args = ModelsArgs::from_raw_args(&toks(&["set", "uia", "model-a"]))
            .map_err(ctx("ModelsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("set"));
        assert_eq!(args.role.as_deref(), Some("uia"));
        assert_eq!(args.target.as_deref(), Some("model-a"));

        let empty = ModelsArgs::from_raw_args(&toks(&[])).map_err(ctx("leer"))?;
        assert_eq!(empty, ModelsArgs::default());
        Ok(())
    }

    /// Prüft, dass `parse_role_effort` ein gültiges Level für eine Rolle mit
    /// Gewichtsfeld akzeptiert und es als `Some` zurückgibt.
    #[test]
    fn parse_role_effort_accepts_level_for_weighted_role() -> TestResult {
        let level = super::parse_role_effort(Some("high"), "root-orchestrator")
            .map_err(ctx("parse_role_effort"))?;
        assert_eq!(level, Some(harw_types::ReasoningEffort::High));
        Ok(())
    }

    /// Prüft, dass `parse_role_effort` für eine Rolle ohne Gewichtsfeld
    /// (z. B. `explorer`) mit deutschem Hinweis ablehnt.
    #[test]
    fn parse_role_effort_rejects_role_without_weight_field() {
        let error = super::parse_role_effort(Some("high"), "explorer")
            .expect_err("Rolle ohne Gewichtsfeld muss abgelehnt werden");
        match error {
            OpError::InvalidArguments(message) => {
                assert!(
                    message.contains("kein eigenes Reasoning-Gewicht"),
                    "unerwartete Meldung: {message}"
                );
                assert!(
                    message.contains("explorer"),
                    "unerwartete Meldung: {message}"
                );
            }
            other => panic!("erwartet InvalidArguments, gefunden: {other:?}"),
        }
    }

    /// Prüft, dass `parse_role_effort` ein unbekanntes Level ablehnt.
    #[test]
    fn parse_role_effort_rejects_unknown_level() {
        let error = super::parse_role_effort(Some("extrem"), "uia")
            .expect_err("unbekanntes Level muss abgelehnt werden");
        match error {
            OpError::InvalidArguments(message) => {
                assert!(
                    message.contains("Unbekanntes Effort-Level: 'extrem'"),
                    "unerwartete Meldung: {message}"
                );
            }
            other => panic!("erwartet InvalidArguments, gefunden: {other:?}"),
        }
    }

    #[test]
    fn models_declares_tui_only_command_without_model_tool() {
        let meta = ModelsOperation.meta();
        assert_eq!(meta.name, "models");
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "das Modell darf die Modellwahl nicht selbst ändern"
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path,
                visibility: harw_operations::CommandVisibility::TuiOnly
            } if *path == "/models"
        )));
    }

    #[test]
    fn resolve_target_prefers_known_model_key_id_and_alias() -> TestResult {
        let config = test_config();
        for target in ["model-a", "model-a-2026", "a-latest"] {
            let resolved = resolve_target(&config, target).map_err(ctx("bekanntes Modell"))?;
            assert_eq!(
                resolved,
                ("provider-a".to_owned(), "model-a-2026".to_owned()),
                "Ziel '{target}'"
            );
        }
        // Eine Katalog-ID mit `/` darf nicht als provider/modell zerlegt werden.
        let resolved =
            resolve_target(&config, "vendor/router-model").map_err(ctx("ID mit Schrägstrich"))?;
        assert_eq!(
            resolved,
            ("openrouter".to_owned(), "vendor/router-model".to_owned())
        );
        Ok(())
    }

    #[test]
    fn resolve_target_normalizes_provider_prefixed_alias_to_canonical_id() -> TestResult {
        let config = test_config();
        // Schlüssel, ID und Alias hinter dem Provider-Präfix landen alle bei
        // der kanonischen Katalog-ID; der genannte Provider bleibt maßgeblich.
        for target in [
            "provider-a/model-a",
            "provider-a/model-a-2026",
            "provider-a/a-latest",
            "provider-a/ a-latest ",
        ] {
            let resolved = resolve_target(&config, target).map_err(ctx("provider/alias"))?;
            assert_eq!(
                resolved,
                ("provider-a".to_owned(), "model-a-2026".to_owned()),
                "Ziel '{target}'"
            );
        }
        // Eine unbekannte Kennung bleibt unverändert (freie Provider-Modell-ID).
        let resolved =
            resolve_target(&config, "provider-a/unbekannt").map_err(ctx("freie Modell-ID"))?;
        assert_eq!(resolved, ("provider-a".to_owned(), "unbekannt".to_owned()));
        Ok(())
    }

    #[test]
    fn resolve_target_splits_provider_prefix_at_first_slash() -> TestResult {
        let config = test_config();
        let resolved = resolve_target(&config, "openrouter/nvidia/nemotron-x")
            .map_err(ctx("provider/modell"))?;
        assert_eq!(
            resolved,
            ("openrouter".to_owned(), "nvidia/nemotron-x".to_owned())
        );
        Ok(())
    }

    #[test]
    fn resolve_target_rejects_unknown_and_disabled_provider() {
        let config = test_config();
        for target in [
            "unbekannt",
            "off/some-model",
            "nope/some-model",
            "openrouter/",
        ] {
            assert!(
                matches!(
                    resolve_target(&config, target),
                    Err(OpError::InvalidArguments(ref message)) if message.contains("Unbekanntes Modell")
                ),
                "Ziel '{target}' muss abgelehnt werden"
            );
        }
    }

    #[tokio::test]
    async fn models_show_lists_every_role_and_live_selection() -> TestResult {
        let controller = Arc::new(NullSessionController::new());
        controller
            .set_active_provider("provider-a".to_owned())
            .map_err(ctx("set_active_provider"))?;
        controller
            .set_active_model("model-a-2026".to_owned())
            .map_err(ctx("set_active_model"))?;
        let shared: SharedSessionController = controller;
        let fixture = fixture(test_config(), Some(shared))?;

        let output = run(&fixture, &["show"])
            .await
            .map_err(ctx("show darf nicht fehlschlagen"))?;
        let data = data(&output)?;
        let roles = data["roles"]
            .as_array()
            .ok_or(TestError::Missing("roles"))?;
        assert_eq!(roles.len(), ModelRole::ALL.len());
        for (row, role) in roles.iter().zip(ModelRole::ALL) {
            assert_eq!(row["role"], serde_json::json!(role.key()));
            for key in ["label", "provider", "model", "source", "effort"] {
                assert!(row.get(key).is_some(), "Feld '{key}' fehlt: {row}");
            }
        }
        assert_eq!(data["live"]["provider"], serde_json::json!("provider-a"));
        assert_eq!(data["live"]["model"], serde_json::json!("model-a-2026"));
        assert!(
            output.text.contains("ab der nächsten Sitzung"),
            "{}",
            output.text
        );
        assert!(output.text.contains("worker-simple"), "{}", output.text);
        assert!(fixture.recorder.calls().is_empty(), "show schreibt nichts");
        Ok(())
    }

    #[tokio::test]
    async fn models_show_without_controller_reports_null_live() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        let output = run(&fixture, &[]).await.map_err(ctx("show"))?;
        let data = data(&output)?;
        assert_eq!(data["live"]["provider"], serde_json::Value::Null);
        assert_eq!(data["live"]["model"], serde_json::Value::Null);
        Ok(())
    }

    // ── Provider-Lebenszyklus: abgeschaltete und abgekündigte Modelle ─────────

    /// Ein in der eingebetteten Lebenszyklus-Tabelle abgeschaltetes Angebot
    /// (Mistral OCR 4.0, retired 2026-09-30 — für die echte Uhr sicher in der
    /// Vergangenheit; Ersatz `mistral-ocr-4-1`).
    const RETIRED_PROVIDER: &str = "mistral";
    const RETIRED_MODEL: &str = "mistral-ocr-4-0";
    /// In der Konfiguration eingetragener Alias des abgeschalteten Modells.
    const RETIRED_ALIAS: &str = "legacy-ocr";

    /// Ein abgekündigtes Angebot ohne angekündigtes Retirement (Claude Mythos
    /// Preview; Ersatz `claude-mythos-5`) — bleibt für die echte Uhr wählbar.
    const DEPRECATED_PROVIDER: &str = "anthropic";
    const DEPRECATED_MODEL: &str = "claude-mythos-preview";

    /// Profil-Config, die ein abgelehnter Wechsel byte-gleich lassen muss.
    const SEEDED_PROFILE: &str =
        "default_provider = \"provider-a\"\ndefault_model = \"model-a-2026\"\n";

    /// [`test_config`] plus aktivierte Provider für das abgeschaltete und das
    /// abgekündigte Angebot und das abgeschaltete Modell im Katalog (das
    /// abgekündigte wird per `provider/modell` gewählt).
    fn lifecycle_test_config() -> ResolvedConfig {
        let mut config = test_config();
        config.providers.insert(
            RETIRED_PROVIDER.to_owned(),
            provider(RETIRED_PROVIDER, true),
        );
        config.providers.insert(
            DEPRECATED_PROVIDER.to_owned(),
            provider(DEPRECATED_PROVIDER, true),
        );
        config.models.insert(
            RETIRED_MODEL.to_owned(),
            model(RETIRED_MODEL, RETIRED_PROVIDER, &[RETIRED_ALIAS]),
        );
        config
    }

    /// Kontext mit Controller (Live- und UIA-Auswahl belegt), Config und
    /// gebundenem Root-Space, dessen Profil-`config.toml` mit
    /// [`SEEDED_PROFILE`] belegt ist — die echte Datei-Persistenz.
    struct HomeFixture {
        ctx: OpContext,
        controller: Arc<NullSessionController>,
        config_path: std::path::PathBuf,
        _root: tempfile::TempDir,
    }

    fn home_fixture(config: ResolvedConfig) -> TestResult<HomeFixture> {
        let root = tempfile::tempdir().map_err(ctx("Root-Space anlegen"))?;
        let profile_dir = root.path().join("profiles").join("default");
        std::fs::create_dir_all(&profile_dir).map_err(ctx("Profilordner anlegen"))?;
        let config_path = profile_dir.join("config.toml");
        std::fs::write(&config_path, SEEDED_PROFILE).map_err(ctx("Profil-Config anlegen"))?;
        let home = crate::config_util::test_home_context(root.path())?;

        let controller = Arc::new(NullSessionController::new());
        controller
            .set_active_provider("provider-a".to_owned())
            .map_err(ctx("Provider setzen"))?;
        controller
            .set_active_model("model-a-2026".to_owned())
            .map_err(ctx("Modell setzen"))?;
        controller
            .set_uia_selection(harw_operations::session_control::UiaSelection::new(
                Some("provider-a".to_owned()),
                Some("model-a-2026".to_owned()),
            ))
            .map_err(ctx("UIA-Auswahl setzen"))?;

        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&controller) as SharedSessionController);
        services.insert(Arc::new(config));
        services.insert(home);
        let op_ctx = crate::knowledge_test_support::op_context(services)?;
        Ok(HomeFixture {
            ctx: op_ctx,
            controller,
            config_path,
            _root: root,
        })
    }

    /// Führt `/models <tokens>` gegen den Root-Space-Kontext aus und erwartet
    /// die Ablehnung: `InvalidArguments`, dessen Meldung jedes Stichwort aus
    /// `needles` enthält, Controller-Snapshot unverändert und Profil-
    /// `config.toml` byte-gleich.
    async fn assert_refused_and_untouched(
        fixture: &HomeFixture,
        tokens: &[&str],
        needles: &[&str],
    ) -> TestResult {
        let before = fixture.controller.snapshot();
        let args = ModelsArgs::from_raw_args(&toks(tokens)).map_err(ctx("Argumente"))?;
        match super::models(&fixture.ctx, args).await {
            Err(OpError::InvalidArguments(message)) => {
                for needle in needles {
                    assert!(
                        message.contains(needle),
                        "{tokens:?}: '{needle}' fehlt in '{message}'"
                    );
                }
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "{tokens:?}: erwartet InvalidArguments, gefunden: {other:?}"
                )));
            }
        }
        assert_eq!(
            fixture.controller.snapshot(),
            before,
            "{tokens:?}: der Controller bleibt unverändert"
        );
        let persisted =
            std::fs::read_to_string(&fixture.config_path).map_err(ctx("Profil-Config lesen"))?;
        assert_eq!(
            persisted, SEEDED_PROFILE,
            "{tokens:?}: die Profil-Config bleibt byte-gleich"
        );
        Ok(())
    }

    /// Wie [`assert_refused_and_untouched`] für ein abgeschaltetes Modell:
    /// die Meldung nennt `retired`, das Modell und den Ersatz.
    async fn assert_retired_refused_and_untouched(
        fixture: &HomeFixture,
        tokens: &[&str],
    ) -> TestResult {
        assert_refused_and_untouched(
            fixture,
            tokens,
            &["retired", RETIRED_MODEL, "mistral-ocr-4-1"],
        )
        .await
    }

    /// `/models set <interne Rolle>` auf ein abgeschaltetes Modell: abgelehnt,
    /// bevor der Provider gebaut oder etwas persistiert wird.
    #[tokio::test]
    async fn models_set_internal_role_to_a_retired_model_changes_nothing() -> TestResult {
        let fixture = home_fixture(lifecycle_test_config())?;
        assert_retired_refused_and_untouched(&fixture, &["set", "explorer", RETIRED_MODEL]).await
    }

    /// `/models set uia` (auch in der `provider/modell`-Form) auf ein
    /// abgeschaltetes Modell: abgelehnt, UIA-Auswahl und Profil unverändert.
    #[tokio::test]
    async fn models_set_uia_to_a_retired_model_changes_nothing() -> TestResult {
        let fixture = home_fixture(lifecycle_test_config())?;
        let target = format!("{RETIRED_PROVIDER}/{RETIRED_MODEL}");
        assert_retired_refused_and_untouched(&fixture, &["set", "uia", target.as_str()]).await?;
        assert_retired_refused_and_untouched(&fixture, &["set", "uia-worker", target.as_str()])
            .await
    }

    /// `/models worker` auf ein abgeschaltetes Modell: abgelehnt, keine
    /// Worker-Wahl persistiert.
    #[tokio::test]
    async fn models_worker_to_a_retired_model_changes_nothing() -> TestResult {
        let fixture = home_fixture(lifecycle_test_config())?;
        assert_retired_refused_and_untouched(&fixture, &["worker", "all", RETIRED_MODEL]).await
    }

    /// `provider/alias` eines abgeschalteten Modells: der Alias wird vor der
    /// Lebenszyklus-Prüfung auf die kanonische Katalog-ID aufgelöst, sodass
    /// die Ablehnung (mit Ersatzhinweis) greift, bevor etwas persistiert wird
    /// — für jede Rolle von `set` und für `worker`.
    #[tokio::test]
    async fn provider_prefixed_alias_of_a_retired_model_is_refused_and_changes_nothing()
    -> TestResult {
        let fixture = home_fixture(lifecycle_test_config())?;
        let target = format!("{RETIRED_PROVIDER}/{RETIRED_ALIAS}");
        for tokens in [
            vec!["set", "explorer", target.as_str()],
            vec!["set", "uia", target.as_str()],
            vec!["set", "uia-worker", target.as_str()],
            vec!["worker", "all", target.as_str()],
            vec!["worker", "uia-writer", target.as_str()],
        ] {
            assert_retired_refused_and_untouched(&fixture, &tokens).await?;
        }
        Ok(())
    }

    /// Derselbe Alias bei zwei Providern: `provider-a/shared` ergibt die ID
    /// von A, `provider-b/shared` die von B — unabhängig von der
    /// Iterationsreihenfolge der Konfiguration, in `resolve_target` wie in
    /// `set` und `worker` (gespeichert wird je Provider dessen eigene ID).
    #[tokio::test]
    async fn same_alias_on_two_providers_resolves_per_provider() -> TestResult {
        // Die Modelle liegen in einer `HashMap`: mehrfach neu aufbauen, damit
        // eine von der Iterationsreihenfolge abhängige Auflösung auffiele.
        for _ in 0..32 {
            let config = shared_alias_config();
            for (target, provider_name, model_id) in [
                ("provider-a/shared", "provider-a", "model-a-shared-id"),
                ("provider-b/shared", "provider-b", "model-b-shared-id"),
                ("provider-b/b-only", "provider-b", "model-b-shared-id"),
                ("provider-b/b-key", "provider-b", "model-b-key-id"),
                // Die reine ID eines fremden Providers ist weder Alias noch
                // Schlüssel: sie bleibt unverändert.
                (
                    "provider-a/model-b-shared-id",
                    "provider-a",
                    "model-b-shared-id",
                ),
            ] {
                let resolved = resolve_target(&config, target).map_err(ctx("provider/alias"))?;
                assert_eq!(
                    resolved,
                    (provider_name.to_owned(), model_id.to_owned()),
                    "Ziel '{target}'"
                );
            }
        }

        let fixture = fixture(shared_alias_config(), None)?;
        run(&fixture, &["set", "explorer", "provider-a/shared"])
            .await
            .map_err(ctx("set explorer provider-a/shared"))?;
        run(&fixture, &["set", "explorer", "provider-b/shared"])
            .await
            .map_err(ctx("set explorer provider-b/shared"))?;
        run(&fixture, &["worker", "uia-writer", "provider-b/shared"])
            .await
            .map_err(ctx("worker uia-writer provider-b/shared"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Explorer,
                    provider: Some("provider-a".to_owned()),
                    model: Some("model-a-shared-id".to_owned()),
                },
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Explorer,
                    provider: Some("provider-b".to_owned()),
                    model: Some("model-b-shared-id".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-writer".to_owned(),
                    value: Some("provider-b/model-b-shared-id".to_owned()),
                },
            ]
        );
        Ok(())
    }

    /// Ein Alias oder Katalog-Schlüssel, der nur zu einem anderen Provider
    /// gehört (`provider-a/<nur-provider-b>`), wird fail-closed abgelehnt —
    /// nie als Paar (Provider A, Modell von B) geprüft oder gespeichert:
    /// Controller und Profil-Config bleiben für jede Rolle unverändert.
    #[tokio::test]
    async fn cross_provider_only_alias_is_refused_and_changes_nothing() -> TestResult {
        let config = shared_alias_config();
        for target in ["provider-a/b-only", "provider-a/b-key"] {
            match resolve_target(&config, target) {
                Err(OpError::InvalidArguments(message)) => {
                    assert!(message.contains("provider-b"), "{target}: {message}");
                    assert!(
                        message.contains("kein Modell von 'provider-a'"),
                        "{target}: {message}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{target}: erwartet InvalidArguments, gefunden: {other:?}"
                    )));
                }
            }
        }

        let fixture = home_fixture(config)?;
        for target in ["provider-a/b-only", "provider-a/b-key"] {
            for tokens in [
                vec!["set", "explorer", target],
                vec!["set", "uia", target],
                vec!["set", "uia-worker", target],
                vec!["worker", "all", target],
                vec!["worker", "uia-writer", target],
            ] {
                assert_refused_and_untouched(
                    &fixture,
                    &tokens,
                    &["provider-b", "kein Modell von 'provider-a'"],
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Eine unpräfixierte Kennung, die als Alias bei zwei Providern (oder bei
    /// zwei Modellen eines Providers) steht, ist mehrdeutig: abgelehnt mit den
    /// sortierten `provider/id`-Kandidaten, vor Lebenszyklus-Prüfung und
    /// Persistenz — Controller und Profil-Config bleiben unverändert.
    #[tokio::test]
    async fn unprefixed_ambiguous_alias_is_refused_and_changes_nothing() -> TestResult {
        let config = shared_alias_config();
        match resolve_target(&config, "shared") {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("Mehrdeutig"), "{message}");
                assert!(
                    message.contains("provider-a/model-a-shared-id, provider-b/model-b-shared-id"),
                    "sortierte Kandidaten erwartet: {message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet InvalidArguments, gefunden: {other:?}"
                )));
            }
        }

        // Zwei Modelle desselben Providers mit gleichem Alias.
        let mut twin = shared_alias_config();
        twin.models.insert(
            "model-a-twin".to_owned(),
            model("model-a-twin-id", "provider-a", &["twin"]),
        );
        twin.models.insert(
            "model-a-twin-2".to_owned(),
            model("model-a-twin-2-id", "provider-a", &["twin"]),
        );
        match resolve_target(&twin, "twin") {
            Err(OpError::InvalidArguments(message)) => assert!(
                message.contains("provider-a/model-a-twin-2-id, provider-a/model-a-twin-id"),
                "{message}"
            ),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet InvalidArguments, gefunden: {other:?}"
                )));
            }
        }

        let fixture = home_fixture(config)?;
        for tokens in [
            vec!["set", "explorer", "shared"],
            vec!["set", "uia", "shared"],
            vec!["set", "uia-worker", "shared"],
            vec!["worker", "all", "shared"],
            vec!["worker", "uia-writer", "shared"],
        ] {
            assert_refused_and_untouched(
                &fixture,
                &tokens,
                &[
                    "Mehrdeutig",
                    "provider-a/model-a-shared-id",
                    "provider-b/model-b-shared-id",
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// Eindeutige unpräfixierte Kennungen lösen wie bisher auf (Schlüssel,
    /// `id`, Alias); dasselbe Modell unter zwei Schlüsseln ist eindeutig, und
    /// eine unbekannte Kennung bleibt abgelehnt.
    #[tokio::test]
    async fn unprefixed_unique_alias_still_resolves() -> TestResult {
        let mut config = shared_alias_config();
        // Dasselbe Modell (gleicher Provider, gleiche ID) unter zweitem Schlüssel.
        config.models.insert(
            "model-a-copy".to_owned(),
            model("model-a-2026", "provider-a", &["a-latest"]),
        );
        for (target, provider_name, model_id) in [
            ("model-a", "provider-a", "model-a-2026"),
            ("model-a-2026", "provider-a", "model-a-2026"),
            ("a-latest", "provider-a", "model-a-2026"),
            ("b-only", "provider-b", "model-b-shared-id"),
            ("b-key", "provider-b", "model-b-key-id"),
            ("vendor/router-model", "openrouter", "vendor/router-model"),
        ] {
            let resolved = resolve_target(&config, target).map_err(ctx("unpräfixiert"))?;
            assert_eq!(
                resolved,
                (provider_name.to_owned(), model_id.to_owned()),
                "Ziel '{target}'"
            );
        }
        assert!(matches!(
            resolve_target(&config, "gibtsnicht"),
            Err(OpError::InvalidArguments(ref message)) if message.contains("Unbekanntes Modell")
        ));

        // Auch durch `set` und `worker` hindurch: eindeutig → gespeichert.
        let fixture = fixture(config, None)?;
        run(&fixture, &["set", "explorer", "b-only"])
            .await
            .map_err(ctx("set explorer b-only"))?;
        run(&fixture, &["worker", "uia-writer", "a-latest"])
            .await
            .map_err(ctx("worker uia-writer a-latest"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Explorer,
                    provider: Some("provider-b".to_owned()),
                    model: Some("model-b-shared-id".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-writer".to_owned(),
                    value: Some("provider-a/model-a-2026".to_owned()),
                },
            ]
        );
        Ok(())
    }

    /// Nennt ein Alias bei einem Provider mehrere verschiedene Modelle, ist
    /// `provider/alias` mehrdeutig und wird abgelehnt (statt vom Zufall der
    /// Iterationsreihenfolge abzuhängen); der andere Provider bleibt aufgelöst.
    #[test]
    fn duplicate_alias_within_one_provider_is_refused_as_ambiguous() -> TestResult {
        let mut config = shared_alias_config();
        config.models.insert(
            "model-a-twin".to_owned(),
            model("model-a-twin-id", "provider-a", &["shared"]),
        );
        match resolve_target(&config, "provider-a/shared") {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("mehrdeutig"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet InvalidArguments, gefunden: {other:?}"
                )));
            }
        }
        let resolved = resolve_target(&config, "provider-b/shared").map_err(ctx("provider-b"))?;
        assert_eq!(
            resolved,
            ("provider-b".to_owned(), "model-b-shared-id".to_owned())
        );
        Ok(())
    }

    /// `provider/alias` (und `provider/schlüssel`) eines nicht abgeschalteten
    /// Modells wird zur kanonischen Katalog-ID aufgelöst; gespeichert und
    /// gemeldet wird die ID, nie der Alias — für `set` und `worker`.
    #[tokio::test]
    async fn provider_prefixed_alias_of_a_live_model_persists_the_canonical_id() -> TestResult {
        let fixture = fixture(test_config(), None)?;

        let output = run(&fixture, &["set", "explorer", "provider-a/a-latest"])
            .await
            .map_err(ctx("set explorer"))?;
        assert!(
            output.text.contains("provider-a/model-a-2026"),
            "{}",
            output.text
        );
        assert!(!output.text.contains("a-latest"), "{}", output.text);
        let data = data(&output)?;
        assert_eq!(data["provider"], serde_json::json!("provider-a"));
        assert_eq!(data["model"], serde_json::json!("model-a-2026"));

        run(&fixture, &["set", "uia", "provider-a/model-a"])
            .await
            .map_err(ctx("set uia"))?;
        run(&fixture, &["worker", "uia-writer", "provider-a/a-latest"])
            .await
            .map_err(ctx("worker uia-writer"))?;

        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Explorer,
                    provider: Some("provider-a".to_owned()),
                    model: Some("model-a-2026".to_owned()),
                },
                RecordedSelectionPersistCall::UiaSelection {
                    provider: Some("provider-a".to_owned()),
                    model: Some("model-a-2026".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-writer".to_owned(),
                    value: Some("provider-a/model-a-2026".to_owned()),
                },
            ]
        );
        Ok(())
    }

    /// Ein abgekündigtes (deprecated) Modell wird gespeichert; die
    /// Bestätigung trägt die Warnung samt Ersatzhinweis — für `set` und
    /// `worker`.
    #[tokio::test]
    async fn deprecated_model_is_persisted_with_a_warning() -> TestResult {
        let fixture = fixture(lifecycle_test_config(), None)?;
        let target = format!("{DEPRECATED_PROVIDER}/{DEPRECATED_MODEL}");

        let output = run(&fixture, &["set", "explorer", target.as_str()])
            .await
            .map_err(ctx("set explorer"))?;
        assert!(output.text.contains("Warnung"), "{}", output.text);
        assert!(output.text.contains("claude-mythos-5"), "{}", output.text);
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::InternalModel {
                point: InternalModelPoint::Explorer,
                provider: Some(DEPRECATED_PROVIDER.to_owned()),
                model: Some(DEPRECATED_MODEL.to_owned()),
            }]
        );

        let output = run(&fixture, &["worker", "uia-writer", target.as_str()])
            .await
            .map_err(ctx("worker uia-writer"))?;
        assert!(output.text.contains("Warnung"), "{}", output.text);
        assert_eq!(
            fixture.recorder.calls().len(),
            2,
            "auch die Worker-Wahl wird gespeichert"
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_set_uia_persists_uia_selection() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        let output = run(&fixture, &["set", "uia", "a-latest"])
            .await
            .map_err(ctx("set uia"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::UiaSelection {
                provider: Some("provider-a".to_owned()),
                model: Some("model-a-2026".to_owned()),
            }]
        );
        let data = data(&output)?;
        assert_eq!(data["persisted"], serde_json::json!(true));
        assert!(output.text.contains("nächsten Sitzung"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn models_set_internal_role_persists_internal_model() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(
            &fixture,
            &["set", "explorer", "openrouter/nvidia/nemotron-x"],
        )
        .await
        .map_err(ctx("set explorer"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::InternalModel {
                point: InternalModelPoint::Explorer,
                provider: Some("openrouter".to_owned()),
                model: Some("nvidia/nemotron-x".to_owned()),
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_set_orchestrator_uses_root_orchestrator_point() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(&fixture, &["set", "orchestrator", "model-a"])
            .await
            .map_err(ctx("set orchestrator"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![RecordedSelectionPersistCall::InternalModel {
                point: InternalModelPoint::RootOrchestrator,
                provider: Some("provider-a".to_owned()),
                model: Some("model-a-2026".to_owned()),
            }]
        );
        Ok(())
    }

    /// Live-Modellwechsel: `/models set orchestrator …` prüft den Provider,
    /// meldet die Wahl an die laufende Montage (neu gestartete Agenten nehmen
    /// sie sofort) und sagt das in der Bestätigung.
    #[tokio::test]
    async fn models_set_child_role_applies_live_when_the_runtime_supports_it() -> TestResult {
        type Call = (InternalModelPoint, Option<harw_config::InternalModelChoice>);
        #[derive(Default)]
        struct LiveRecorder {
            ready: std::sync::Mutex<Vec<String>>,
            calls: std::sync::Mutex<Vec<Call>>,
        }
        impl crate::live_model::LiveModelControl for LiveRecorder {
            fn ensure_provider_ready(&self, provider: &str) -> Result<(), String> {
                if let Ok(mut ready) = self.ready.lock() {
                    ready.push(provider.to_owned());
                }
                Ok(())
            }
            fn set_internal_model(
                &self,
                point: InternalModelPoint,
                choice: Option<harw_config::InternalModelChoice>,
            ) -> bool {
                if let Ok(mut calls) = self.calls.lock() {
                    calls.push((point, choice));
                }
                true
            }
        }

        let base = fixture(test_config(), None)?;
        let live = Arc::new(LiveRecorder::default());
        let persistence: Arc<dyn SelectionPersistence> = base.recorder.clone();
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(test_config()));
        services.insert(Arc::clone(&live) as crate::live_model::SharedLiveModelControl);
        let live_ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            base.ctx.sandbox().clone(),
            services,
        );
        let args = ModelsArgs::from_raw_args(&toks(&["set", "orchestrator", "model-a"]))
            .map_err(ctx("args"))?;
        let output = super::models(&live_ctx, args)
            .await
            .map_err(ctx("set orchestrator"))?;

        assert_eq!(
            live.ready.lock().map(|ready| ready.clone()).ok(),
            Some(vec!["provider-a".to_owned()])
        );
        assert_eq!(
            live.calls.lock().map(|calls| calls.clone()).ok(),
            Some(vec![(
                InternalModelPoint::RootOrchestrator,
                Some(harw_config::InternalModelChoice {
                    provider: Some("provider-a".to_owned()),
                    model: Some("model-a-2026".to_owned()),
                }),
            )])
        );
        assert!(
            output
                .text
                .contains("gilt ab sofort für neu gestartete Agenten"),
            "{}",
            output.text
        );
        assert_eq!(
            base.recorder.calls().len(),
            1,
            "die Wahl wird weiterhin gespeichert"
        );
        Ok(())
    }

    /// Live-Schnappschuss: eine gespeicherte Rollenwahl steht im folgenden
    /// `/models show` (Datenquelle der Übersicht F8) sofort drin — auch nach
    /// einem Zurücksetzen. Vorher blieb die Tabelle bis zum Neustart beim
    /// Stand des Starts.
    #[tokio::test]
    async fn models_set_and_reset_are_visible_in_the_following_show() -> TestResult {
        let base = fixture(test_config(), None)?;
        let persistence: Arc<dyn SelectionPersistence> = base.recorder.clone();
        let live: crate::live_config::SharedLiveConfig =
            Arc::new(crate::live_config::LiveConfig::new(Arc::new(test_config())));
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(Arc::new(test_config()));
        services.insert(Arc::clone(&live));
        let live_ctx = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            base.ctx.sandbox().clone(),
            services,
        );
        async fn run_in(ctx: &OpContext, tokens: &[&str]) -> Result<OpOutput, OpError> {
            let args = ModelsArgs::from_raw_args(&toks(tokens))?;
            super::models(ctx, args).await
        }
        let row = |output: &OpOutput, role: &str| -> TestResult<serde_json::Value> {
            data(output)?["roles"]
                .as_array()
                .and_then(|roles| roles.iter().find(|row| row["role"] == role).cloned())
                .ok_or(TestError::Missing("role row"))
        };

        run_in(
            &live_ctx,
            &["set", "explorer", "openrouter/vendor/router-model"],
        )
        .await
        .map_err(ctx("set explorer"))?;
        run_in(&live_ctx, &["set", "uia", "model-a"])
            .await
            .map_err(ctx("set uia"))?;
        let shown = run_in(&live_ctx, &["show"]).await.map_err(ctx("show"))?;
        let explorer = row(&shown, "explorer")?;
        assert_eq!(explorer["provider"], serde_json::json!("openrouter"));
        assert_eq!(explorer["model"], serde_json::json!("vendor/router-model"));
        let uia = row(&shown, "uia")?;
        assert_eq!(uia["provider"], serde_json::json!("provider-a"));
        assert_eq!(uia["model"], serde_json::json!("model-a-2026"));

        run_in(&live_ctx, &["reset", "explorer"])
            .await
            .map_err(ctx("reset explorer"))?;
        let shown = run_in(&live_ctx, &["show"])
            .await
            .map_err(ctx("show after reset"))?;
        assert_ne!(
            row(&shown, "explorer")?["model"],
            serde_json::json!("vendor/router-model")
        );
        Ok(())
    }

    /// Runde 5, Teil G: ein UIA-Worker-Modell eines anderen Providers als
    /// die UIA wird angenommen und als eigene Rollenwahl gespeichert.
    #[tokio::test]
    async fn models_set_uia_worker_accepts_another_provider() -> TestResult {
        let mut config = test_config();
        config.harness.uia_provider = Some("provider-a".to_owned());
        let fixture = fixture(config, None)?;

        run(&fixture, &["set", "uia-worker", "openrouter/some-model"])
            .await
            .map_err(ctx("fremder Provider"))?;
        run(&fixture, &["set", "uia-worker", "model-a"])
            .await
            .map_err(ctx("passender Provider"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: Some("openrouter/some-model".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: Some("provider-a/model-a-2026".to_owned()),
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_reset_dispatches_per_role() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        run(&fixture, &["reset", "uia"])
            .await
            .map_err(ctx("reset uia"))?;
        run(&fixture, &["reset", "uia-worker"])
            .await
            .map_err(ctx("reset uia-worker"))?;
        run(&fixture, &["reset", "worker-simple"])
            .await
            .map_err(ctx("reset worker-simple"))?;
        assert_eq!(
            fixture.recorder.calls(),
            vec![
                RecordedSelectionPersistCall::ClearUia,
                RecordedSelectionPersistCall::UiaWorkerRoleModel {
                    role: "uia-worker".to_owned(),
                    value: None,
                },
                RecordedSelectionPersistCall::UiaWorkerModel { model: None },
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::WorkerSimple,
                    provider: None,
                    model: None,
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_rejects_bad_input_in_german() -> TestResult {
        let fixture = fixture(test_config(), None)?;
        for (tokens, needle) in [
            (vec!["frobnicate"], "Unbekanntes /models-Sub-Kommando"),
            (vec!["set"], "Rolle fehlt"),
            (vec!["set", "kapitän", "model-a"], "Unbekannte Rolle"),
            (vec!["set", "uia"], "Ziel fehlt"),
            (vec!["set", "uia", "gibtsnicht"], "Unbekanntes Modell"),
            (vec!["reset"], "Rolle fehlt"),
        ] {
            let result = run(&fixture, &tokens).await;
            match result {
                Err(OpError::InvalidArguments(message)) => assert!(
                    message.contains(needle),
                    "{tokens:?}: erwartet '{needle}' in '{message}'"
                ),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?}: InvalidArguments erwartet, war {other:?}"
                    )));
                }
            }
        }
        assert!(fixture.recorder.calls().is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod remote_tests {
    use super::{
        AuthKind, REMOTE, ResolvedConfig, catalog_view, handle_add, handle_remove, model_filename,
        plan_models_change, redact_base_url, render_show,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_operations::context::ServiceMap;
    use harw_operations::operation::{RemoteCap, RemoteEffect, RemoteSlice};
    use harw_operations::{OpContext, OpError};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    fn provider(value: serde_json::Value) -> TestResult<harw_config::ProviderToml> {
        serde_json::from_value(value).map_err(ctx("ProviderToml bauen"))
    }

    fn model(id: &str, provider_name: &str) -> TestResult<harw_config::ModelToml> {
        serde_json::from_value(json!({"id": id, "provider": provider_name}))
            .map_err(ctx("ModelToml bauen"))
    }

    fn now() -> harw_model_catalog::lifecycle::OffsetDateTime {
        harw_model_catalog::lifecycle::OffsetDateTime::now_utc()
    }

    fn secret_config() -> TestResult<ResolvedConfig> {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "p1".to_owned(),
            provider(json!({
                "name": "p1",
                "api": "openai-chat",
                "base_url": "https://user:pw-fake@api.example.test/v1?key=q-fake-SECRET-marker#frag",
                "api_key": "sk-fake-SECRET-marker",
                "auth_header": "X-Auth-fake",
                "headers": {"X-Fake": "hdr-fake-SECRET-marker"},
            }))?,
        );
        config.providers.insert(
            "p2".to_owned(),
            provider(json!({
                "name": "p2",
                "api": "openai-chat",
                "base_url": "https://api.example.test/p2",
                "auth": "secrets:fake-id",
            }))?,
        );
        config.providers.insert(
            "p3".to_owned(),
            provider(json!({
                "name": "p3",
                "api": "openai-chat",
                "base_url": "https://api.example.test/p3",
                "auth": "file:/x/fake-path",
            }))?,
        );
        config.models.insert("m1".to_owned(), model("m1", "p1")?);
        Ok(config)
    }

    #[test]
    fn catalog_view_never_leaks_secrets() -> TestResult {
        let view = catalog_view(&secret_config()?, None, None, now());
        let json = serde_json::to_string(&view).map_err(ctx("serialisieren"))?;
        for forbidden in [
            "SECRET",
            "pw-fake",
            "X-Fake",
            "X-Auth-fake",
            "?key",
            "#frag",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} im JSON: {json}");
        }
        let p1 = view
            .providers
            .iter()
            .find(|p| p.name == "p1")
            .ok_or(TestError::Missing("p1"))?;
        assert_eq!(p1.base_url, "https://api.example.test/v1");
        assert_eq!(p1.auth_kind, AuthKind::InlineKey);
        assert!(p1.has_secret);
        let p2 = view
            .providers
            .iter()
            .find(|p| p.name == "p2")
            .ok_or(TestError::Missing("p2"))?;
        assert_eq!(p2.auth_kind, AuthKind::Secrets);
        assert_eq!(p2.auth_name.as_deref(), Some("fake-id"));
        assert!(p2.has_secret);
        Ok(())
    }

    #[test]
    fn catalog_view_file_secret_has_no_path() -> TestResult {
        let view = catalog_view(&secret_config()?, None, None, now());
        let p3 = view
            .providers
            .iter()
            .find(|p| p.name == "p3")
            .ok_or(TestError::Missing("p3"))?;
        assert_eq!(p3.auth_kind, AuthKind::File);
        assert_eq!(p3.auth_name, None);
        let json = serde_json::to_string(&view).map_err(ctx("serialisieren"))?;
        assert!(!json.contains("fake-path"));
        Ok(())
    }

    #[test]
    fn catalog_view_is_sorted_and_carries_live_model() -> TestResult {
        let view = catalog_view(&secret_config()?, Some("p1"), Some("m1"), now());
        let names: Vec<&str> = view.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["p1", "p2", "p3"]);
        assert_eq!(view.live.provider.as_deref(), Some("p1"));
        assert_eq!(view.live.model.as_deref(), Some("m1"));
        assert_eq!(view.models.len(), 1);
        assert_eq!(view.models[0].lifecycle, "unknown");
        Ok(())
    }

    #[test]
    fn redact_base_url_keeps_only_scheme_host_path() {
        assert_eq!(
            redact_base_url("http://h.example.test:8080/a/b?x=1#f"),
            "http://h.example.test:8080/a/b"
        );
        assert_eq!(
            redact_base_url("https://u:p@h.example.test"),
            "https://h.example.test"
        );
        assert_eq!(redact_base_url("no-scheme"), "");
        assert_eq!(redact_base_url("https://"), "");
    }

    #[test]
    fn catalog_view_lists_roles_like_show() -> TestResult {
        let config = secret_config()?;
        let view = catalog_view(&config, None, None, now());
        let rows = harw_config::resolve_role_models(&config);
        let shown = render_show(&rows, None, None);
        let data = shown.data.ok_or(TestError::Missing("data"))?;
        let shown_roles: Vec<String> = data["roles"]
            .as_array()
            .ok_or(TestError::Missing("roles"))?
            .iter()
            .filter_map(|row| row["role"].as_str().map(str::to_owned))
            .collect();
        let view_roles: Vec<String> = view.roles.iter().map(|r| r.role.to_owned()).collect();
        assert!(!view_roles.is_empty());
        assert_eq!(view_roles, shown_roles);
        Ok(())
    }

    #[test]
    fn models_remote_policy_shape() {
        let show = REMOTE.find_verb(&toks(&["show"]));
        assert!(show.is_some_and(|v| v.cap == RemoteCap::Observe && !v.stepup));
        assert!(
            REMOTE
                .find_verb(&toks(&[]))
                .is_some_and(|v| v.slice == RemoteSlice::Read)
        );
        let set = REMOTE.find_verb(&toks(&["set", "uia", "x/y"]));
        assert!(set.is_some_and(|v| v.stepup
            && v.cap == RemoteCap::Control
            && v.slice == RemoteSlice::Global
            && v.effect == RemoteEffect::NextSession));
        assert!(REMOTE.find_verb(&toks(&["pick", "uia"])).is_none());
        assert!(REMOTE.denied_tokens.contains(&"pick"));
        assert!(REMOTE.denied_tokens.contains(&"--allow-insecure-lan"));
        // Lese-Formen von image/worker bleiben Observe, Schreib-Formen nicht.
        assert!(
            REMOTE
                .find_verb(&toks(&["image", "show"]))
                .is_some_and(|v| !v.stepup)
        );
        assert!(
            REMOTE
                .find_verb(&toks(&["image", "x/y"]))
                .is_some_and(|v| v.stepup)
        );
        assert!(
            REMOTE
                .find_verb(&toks(&["worker"]))
                .is_some_and(|v| !v.stepup)
        );
        assert!(
            REMOTE
                .find_verb(&toks(&["add", "p/m"]))
                .is_some_and(|v| v.stepup)
        );
    }

    struct ListFixture {
        ctx: OpContext,
        config: Arc<ResolvedConfig>,
        profile: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    fn list_fixture() -> TestResult<ListFixture> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(dir.path())?;
        let profile = home.profile_dir.clone();
        std::fs::create_dir_all(profile.join("providers")).map_err(ctx("providers"))?;
        let mut config = ResolvedConfig::default();
        for (name, enabled) in [("p1", true), ("off", false)] {
            let mut entry = provider(json!({
                "name": name,
                "api": "x",
                "base_url": "https://api.example.test/v1",
                "models": ["m1"],
                "enabled": enabled,
            }))?;
            entry.models = vec!["m1".to_owned()];
            config.providers.insert(name.to_owned(), entry);
            std::fs::write(
                profile.join("providers").join(format!("{name}.toml")),
                format!(
                    "name = \"{name}\"\napi = \"x\"\nbase_url = \"https://api.example.test/v1\"\nenabled = {enabled}\nmodels = [\"m1\"]\n"
                ),
            )
            .map_err(ctx("Provider-Datei"))?;
        }
        config.models.insert("m1".to_owned(), model("m1", "p1")?);
        config.models.insert("m2".to_owned(), model("m2", "p1")?);
        config.models.insert("m9".to_owned(), model("m9", "off")?);
        let config = Arc::new(config);
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&home));
        services.insert(Arc::clone(&config));
        let ctx = crate::knowledge_test_support::op_context(services)?;
        Ok(ListFixture {
            ctx,
            config,
            profile,
            _dir: dir,
        })
    }

    fn file_models(path: &Path) -> TestResult<Vec<String>> {
        let text = std::fs::read_to_string(path).map_err(ctx("lesen"))?;
        let doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(ctx("parsen"))?;
        Ok(doc["models"]
            .as_array()
            .ok_or(TestError::Missing("models"))?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect())
    }

    #[test]
    fn add_then_remove_roundtrip() -> TestResult {
        let fx = list_fixture()?;
        let path = fx.profile.join("providers/p1.toml");
        handle_add(&fx.ctx, &fx.config, Some("p1/m2")).map_err(ctx("add"))?;
        assert_eq!(file_models(&path)?, ["m1", "m2"]);
        // Fremdschlüssel bleiben erhalten.
        let text = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        assert!(text.contains("api = \"x\""), "{text}");
        assert!(text.contains("name = \"p1\""), "{text}");
        // Doppeltes Hinzufügen erzeugt kein Duplikat.
        handle_add(&fx.ctx, &fx.config, Some("p1/m2")).map_err(ctx("add 2"))?;
        assert_eq!(file_models(&path)?, ["m1", "m2"]);

        std::fs::create_dir_all(fx.profile.join("models")).map_err(ctx("models dir"))?;
        let model_file = fx.profile.join("models").join(model_filename("m2"));
        std::fs::write(&model_file, "id = \"m2\"\n").map_err(ctx("Modelldatei"))?;
        handle_remove(&fx.ctx, &fx.config, Some("p1/m2")).map_err(ctx("remove"))?;
        assert_eq!(file_models(&path)?, ["m1"]);
        assert!(!model_file.exists());
        let text = std::fs::read_to_string(&path).map_err(ctx("lesen"))?;
        assert!(text.contains("api = \"x\""), "{text}");
        Ok(())
    }

    #[test]
    fn add_rejects_disabled_provider() -> TestResult {
        let fx = list_fixture()?;
        let error = handle_add(&fx.ctx, &fx.config, Some("off/m9"));
        assert!(matches!(error, Err(OpError::InvalidArguments(_))));
        assert_eq!(file_models(&fx.profile.join("providers/off.toml"))?, ["m1"]);
        Ok(())
    }

    #[test]
    fn add_rejects_unknown_model_and_bad_target() -> TestResult {
        let fx = list_fixture()?;
        for target in ["p1/unknown", "p1", "p1/", "/m1", "nope/m1"] {
            assert!(
                matches!(
                    handle_add(&fx.ctx, &fx.config, Some(target)),
                    Err(OpError::InvalidArguments(_))
                ),
                "{target}"
            );
        }
        assert_eq!(file_models(&fx.profile.join("providers/p1.toml"))?, ["m1"]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn remove_keeps_symlinked_model_file() -> TestResult {
        let fx = list_fixture()?;
        std::fs::create_dir_all(fx.profile.join("models")).map_err(ctx("models dir"))?;
        let real = fx.profile.join("real-model.toml");
        std::fs::write(&real, "id = \"m1\"\n").map_err(ctx("real"))?;
        let link = fx.profile.join("models").join(model_filename("m1"));
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        handle_remove(&fx.ctx, &fx.config, Some("p1/m1")).map_err(ctx("remove"))?;
        assert!(link.symlink_metadata().is_ok());
        assert!(real.exists());
        assert!(file_models(&fx.profile.join("providers/p1.toml"))?.is_empty());
        Ok(())
    }

    #[test]
    fn plan_models_change_text_is_canonical() -> TestResult {
        let fx = list_fixture()?;
        let add = plan_models_change(&fx.ctx, &toks(&["add", "p1/m2"])).map_err(ctx("add"))?;
        assert_eq!(add.action, "add");
        assert_eq!(add.canonical, "models.add p1/m2: absent -> present");
        let remove =
            plan_models_change(&fx.ctx, &toks(&["remove", "p1/m1"])).map_err(ctx("remove"))?;
        assert_eq!(remove.canonical, "models.remove p1/m1: present -> absent");
        let set =
            plan_models_change(&fx.ctx, &toks(&["set", "uia", "p1/m1"])).map_err(ctx("set"))?;
        assert_eq!(set.action, "set");
        assert!(
            set.canonical.starts_with("models.set uia: "),
            "{}",
            set.canonical
        );
        assert!(set.canonical.ends_with(" -> p1/m1"), "{}", set.canonical);
        // Planen schreibt nichts.
        assert_eq!(file_models(&fx.profile.join("providers/p1.toml"))?, ["m1"]);
        // Lesende Formen sind keine Änderung.
        assert!(plan_models_change(&fx.ctx, &toks(&["show"])).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn scan_is_not_available_until_guard_lands() -> TestResult {
        let fx = list_fixture()?;
        let args = <super::ModelsArgs as harw_operations::FromRawArgs>::from_raw_args(&toks(&[
            "scan", "p1",
        ]))
        .map_err(ctx("args"))?;
        let result = super::models(&fx.ctx, args).await;
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        Ok(())
    }
}
