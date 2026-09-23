//! Generische Merge-Engine für `HarnessConfig` über Config-Layer hinweg.
//!
//! `docs/design/config-scopes.md` Abschnitt 7b. Ersetzt zwei Stellen in
//! `harw-config/src/discovery.rs` (Paket B verdrahtet diese Ersetzung, siehe
//! Hinweis unten): die pauschale `resolved.harness = cfg`-Zuweisung
//! (`discovery.rs:649`) für jeden vertrauten Layer sowie die separate,
//! nur neun Felder abdeckende `merge_restricted_harness`-Funktion
//! (`discovery.rs:942-1003`) für den einen nicht vertrauten Projekt-Layer.
//! Beide Aufrufer unterscheiden sich künftig nur im übergebenen
//! [`LayerRole`].
//!
//! # Verantwortung
//! Dieses Modul kennt die **Anwendung** jeder [`crate::scope::MergeRule`]-
//! Variante (generische Regel-Helfer wie `profile_replaces`/`global_only`/
//! `intersection_list`/... plus eine handgeschriebene Sequenz von
//! Sektions-Helfern, einer pro `HarnessConfig`-Unterstruktur). Es kennt
//! **nicht**, welche Regel für welches Feld gilt — das steht ausschließlich
//! in `crate::scope::FIELD_TABLE`. Die Sektions-Helfer in diesem Modul
//! rufen die generischen Regel-Helfer mit der laut `FIELD_TABLE`
//! zugeordneten Regel pro Feld manuell auf (Rust hat keine
//! Laufzeit-Reflection über heterogene Struct-Felder — echte Generik über
//! `FIELD_TABLE` ist hier nicht möglich).
//!
//! # Nebenläufigkeit
//! Rein synchron, keine gemeinsam genutzten Zustände, kein `Arc`/`Mutex`,
//! keine Threads. Alle Funktionen sind zustandslos bis auf ihre `&mut`-
//! Parameter.
//!
//! # Fehler
//! Dieses Modul definiert keinen eigenen Fehlertyp — `merge_layer_into`
//! kann nicht fehlschlagen (jede Regel hat für jeden Eingabewert ein
//! wohldefiniertes Ergebnis); abgelehnte Lockerungsversuche werden über den
//! `Vec<ScopeDiagnostic>`-Rückgabewert sichtbar gemacht, nicht über `Result`.
//!
//! # Logging (`tracing`)
//! `docs/design/config-scopes.md` Abschnitt 7e: zusätzlich zum
//! `Vec<ScopeDiagnostic>`-Rückgabewert erzeugt jeder verworfene Layer-Wert
//! synchron **genau ein** `tracing::warn!` mit den strukturierten Feldern
//! `field` (gepunkteter Feldpfad), `layer_role` (`baseline`/`refinement`/
//! `untrusted_project`), `layer_path` (die `config.toml` bzw. das
//! Layer-Verzeichnis) und `reason` (maschinenlesbarer Ablehnungsgrund, z. B.
//! `exceeds_min_bound`, `global_only_override`, `untrusted_project_layer`).
//! - Ablehnungen mit `ScopeDiagnostic` (Regel-Helfer `global_only`,
//!   `intersection_list`, `min_bound`, `and_bool`, `or_bool`, `stricter_of`,
//!   Principals) laufen über `reject` und loggen zusätzlich den nicht
//!   geheimen `rejected_value`.
//! - Werte, die der nicht vertraute Projekt-Layer setzt, die aber per
//!   Scope-Regel still verworfen werden (`profile_replaces`,
//!   `config_version`, `default_provider`/`default_model`/
//!   `active_uia_definition`, `[onboarding]`, `[internal_models]`), laufen
//!   über `warn_untrusted_ignored`: nur Warnung, keine `ScopeDiagnostic`,
//!   und nie mit dem Wert selbst.
//!
//! Stilles Verengen (z. B. Entfernen eines `Intersection`-Eintrags) ist
//! keine Ablehnung und wird nicht geloggt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_config::{merge_layer_into, HarnessConfig, LayerRole};
//! use std::path::Path;
//!
//! let mut trusted = HarnessConfig::default();
//! let incoming = HarnessConfig::default();
//! let raw: toml::Value = toml::from_str("").unwrap();
//! let diagnostics = merge_layer_into(
//!     &mut trusted,
//!     incoming,
//!     &raw,
//!     LayerRole::Baseline,
//!     Path::new("/home/user/.harw/config.toml"),
//! );
//! assert!(diagnostics.is_empty());
//! ```

use std::path::Path;

use crate::harness_config::{
    CompactionToml, GuardsToml, HarnessConfig, McpListenerSection, McpPrincipalToml,
    OnboardingSection, PolicySection, ReasoningWeightsToml, SandboxSection, SessionSection,
    TuiSection,
};
use crate::internal_models::InternalModelsToml;
use crate::mode_toml::ModeSection;
use crate::permissions_toml::PermissionsSection;
use crate::plan_toml::ToolsSection;
use crate::research_toml::ResearchSection;
use crate::scope::{PERMISSIONS_DEFAULT_MODE_ORDER, POLICY_VISIBILITY_SCOPE_ORDER};

/// Grober Vertrauens-/Ebenen-Kontext eines [`merge_layer_into`]-Aufrufs;
/// bestimmt, welche [`crate::scope::MergeRule`]-Varianten überhaupt wirken
/// (`docs/design/config-scopes.md` Abschnitt 7c).
///
/// # Examples
/// ```rust
/// use harw_config::LayerRole;
///
/// assert_ne!(LayerRole::Baseline, LayerRole::Refinement);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    /// Der erste vertraute Layer (`~/.harw`, `layer_index == 0`). Jede
    /// Regel verhält sich hier identisch zu `ProfileReplaces` — es gibt
    /// noch keinen GLOBAL-Vorzustand, gegen den verengt werden könnte.
    Baseline,
    /// Jeder weitere vertraute Layer (aktives Profil, `layer_index >= 1`).
    Refinement,
    /// Der nicht vertraute Projekt-Layer aus `apply_restricted_layer`
    /// (`discovery.rs`).
    UntrustedProject,
}

/// Ein abgelehnter Scope-Lockerungsversuch aus [`merge_layer_into`]
/// (`docs/design/config-scopes.md` Abschnitt 7e). Modelliert auf
/// `discovery::ConfigDiagnostic` (nicht-fatal, sichtbar, blockiert den
/// Start nicht), trägt aber die für eine Scope-Verletzung nötigen
/// Zusatzfelder.
///
/// # Examples
/// ```rust
/// use harw_config::ScopeDiagnostic;
///
/// let diagnostic = ScopeDiagnostic {
///     field: "mcp_listener.enabled".to_owned(),
///     file: "/home/user/.harw/profiles/default/config.toml".to_owned(),
///     rejected_value: "true".to_owned(),
/// };
/// assert!(diagnostic.to_string().contains("mcp_listener.enabled"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDiagnostic {
    /// Gepunkteter Feldpfad, z. B. `"mcp_listener.enabled"` — identisch zu
    /// `FieldScope::path`.
    pub field: String,
    /// `config.toml`, die den Lockerungsversuch enthielt.
    pub file: String,
    /// Der verworfene Wert, `Debug`-formatiert. Nie ein Secret: jedes Feld,
    /// das eine `ScopeDiagnostic` auslösen kann, ist ein nicht-geheimer
    /// Skalar/Enum/Pfad/Regel-Eintrag — `credential_ref` nimmt nie teil, da
    /// `mcp_listener.principals` als Ganzes/`Intersection` verglichen wird,
    /// nicht sein `credential_ref`-Unterfeld isoliert.
    pub rejected_value: String,
}

impl ScopeDiagnostic {
    // Baut eine Diagnostic aus einem beliebigen `Debug`-formatierbaren
    // Wert; hält jeden Aufrufer unten kurz (`field`/`layer_path` sind an
    // jeder Stelle bereits bekannt).
    fn new(field: &str, layer_path: &Path, rejected_value: &dyn std::fmt::Debug) -> Self {
        Self {
            field: field.to_owned(),
            file: layer_path.display().to_string(),
            rejected_value: format!("{rejected_value:?}"),
        }
    }
}

impl std::fmt::Display for ScopeDiagnostic {
    /// Schreibt `"<field> in <file>: scope loosening attempt ignored
    /// (rejected value: <rejected_value>)"`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} in {}: scope loosening attempt ignored (rejected value: {})",
            self.field, self.file, self.rejected_value
        )
    }
}

impl LayerRole {
    // Stabiler, maschinenlesbarer Bezeichner fuer das strukturierte
    // `layer_role`-Feld der `tracing::warn!`-Aufrufe unten.
    fn label(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Refinement => "refinement",
            Self::UntrustedProject => "untrusted_project",
        }
    }
}

// Grund, aus dem ein Layer-Wert verworfen wurde; landet als strukturiertes
// `reason`-Feld in genau einem `tracing::warn!` pro Ablehnung.
#[derive(Debug, Clone, Copy)]
enum RejectionReason {
    // `GlobalOnly`: spaeterer Layer setzt einen vom globalen Wert
    // abweichenden Wert.
    GlobalOnlyOverride,
    // `Intersection`: spaeterer Layer fuegt einen im Home-Layer fehlenden
    // Eintrag hinzu.
    IntersectionNewEntry,
    // `MinBound`: spaeterer Layer setzt einen hoeheren Wert (oder den
    // Default-/Sentinel-Wert) als die bisherige Obergrenze.
    ExceedsMinBound,
    // `AndBool`: spaeterer Layer versucht einzuschalten.
    AndBoolEnable,
    // `OrBool`: spaeterer Layer versucht abzuschalten.
    OrBoolDisable,
    // `StricterOf`: spaeterer Layer setzt einen weniger strengen Wert.
    LessStrictValue,
    // `StricterOf`-Fallback (R1): Wert ausserhalb der Strenge-Ordnung.
    UnorderedValue,
    // `mcp_listener.principals`: neue Principal-`id`.
    PrincipalAdded,
    // `mcp_listener.principals`: bekannte `id` mit geaenderten Unterfeldern.
    PrincipalChanged,
    // Nicht vertrauter Projekt-Layer setzt ein Feld, das nur vertraute
    // Layer setzen duerfen (`ProfileReplaces`/`PerFileValidated`/
    // Sonderfaelle, Abschnitt 7c).
    UntrustedProjectLayer,
}

impl RejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::GlobalOnlyOverride => "global_only_override",
            Self::IntersectionNewEntry => "intersection_new_entry",
            Self::ExceedsMinBound => "exceeds_min_bound",
            Self::AndBoolEnable => "and_bool_enable",
            Self::OrBoolDisable => "or_bool_disable",
            Self::LessStrictValue => "less_strict_value",
            Self::UnorderedValue => "unordered_value",
            Self::PrincipalAdded => "principal_added",
            Self::PrincipalChanged => "principal_changed",
            Self::UntrustedProjectLayer => "untrusted_project_layer",
        }
    }
}

// Meldet einen abgelehnten Lockerungsversuch: genau ein `tracing::warn!`
// plus die zugehoerige `ScopeDiagnostic` in `out` (Abschnitt 7e).
// `rejected_value` ist per `ScopeDiagnostic`-Vertrag nie ein Secret
// (Principals werden nur ueber ihre `id` beschrieben, nie ueber
// `credential_ref`).
fn reject(
    out: &mut Vec<ScopeDiagnostic>,
    diagnostic: ScopeDiagnostic,
    role: LayerRole,
    reason: RejectionReason,
) {
    tracing::warn!(
        field = %diagnostic.field,
        layer_role = role.label(),
        layer_path = %diagnostic.file,
        reason = reason.as_str(),
        rejected_value = %diagnostic.rejected_value,
        "config: scope loosening attempt ignored"
    );
    out.push(diagnostic);
}

// Meldet einen vom nicht vertrauten Projekt-Layer gesetzten, aber per
// Scope-Regel still verworfenen Wert (`ProfileReplaces` und die
// Sonderfaelle `config_version`/`default_provider`/`default_model`/
// `active_uia_definition`/`[onboarding]`/`[internal_models]`). Genau ein
// `tracing::warn!`, **keine** `ScopeDiagnostic` (unveraendertes
// Rueckgabeverhalten von `merge_layer_into`) und bewusst **ohne** den Wert
// selbst — diese Felder sind nicht durch den `ScopeDiagnostic`-Vertrag als
// nicht-geheim abgesichert.
fn warn_untrusted_ignored(field: &str, layer_path: &Path) {
    tracing::warn!(
        field = %field,
        layer_role = LayerRole::UntrustedProject.label(),
        layer_path = %layer_path.display(),
        reason = RejectionReason::UntrustedProjectLayer.as_str(),
        "config: value from untrusted project layer ignored"
    );
}

// Ob `path` (Kette verschachtelter Tabellen-Keys) im geparsten Dokument
// `fields` ausdrücklich gesetzt ist. Bewusst als eigene, private Kopie von
// `discovery::field_present` gehalten statt importiert: `discovery::
// field_present` ist `fn` (Modul-privat), und dieser Arbeitsauftrag darf
// `discovery.rs` nicht anfassen (das ist Paket B). Identische Logik.
fn field_present(fields: &toml::Value, path: &[&str]) -> bool {
    let mut value = Some(fields);
    for key in path {
        value = value.and_then(|table| table.get(*key));
    }
    value.is_some()
}

// Minimum, wobei der Default-/Sentinel-Wert eines Typs (i. d. R. `0`) aus
// dem später geladenen Layer nie übernommen wird — bestehende
// `discovery::min_positive`-Konvention (`discovery.rs:1008-1014`), hier
// aus demselben Grund wie `field_present` als private Kopie gehalten statt
// importiert.
fn min_positive<T: Ord + Default + Copy>(trusted: T, incoming: T) -> T {
    if incoming == T::default() {
        trusted
    } else {
        trusted.min(incoming)
    }
}

// --- Generische Regel-Helfer (docs/design/config-scopes.md Abschnitt 7b) --

// `MergeRule::ProfileReplaces`: der Wert des Layers gewinnt, wenn er ihn
// ausdrücklich gesetzt hat (`present`); ein Layer, der das Feld nicht
// setzt, lässt `trusted` unverändert (kein Reset auf den Section-Default —
// das ist die eigentliche Bugfix-Wirkung, Abschnitt 4/7f). Nie vom nicht
// vertrauten Projekt-Layer angewendet (Abschnitt 7c) — setzt dieser das
// Feld dennoch, wird der Wert verworfen und genau einmal per
// `warn_untrusted_ignored` gemeldet.
fn profile_replaces<T: Clone>(
    trusted: &mut T,
    incoming: T,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
) {
    if role == LayerRole::UntrustedProject {
        if present {
            warn_untrusted_ignored(field, layer_path);
        }
        return;
    }
    if present {
        *trusted = incoming;
    }
}

// `MergeRule::GlobalOnly`: nur der beim ersten (Baseline-)Layer gesetzte
// Wert gilt. Ein späterer Layer, der einen *anderen* Wert setzt, wird
// ignoriert und erzeugt eine `ScopeDiagnostic`; denselben Wert erneut zu
// setzen ist ein stiller No-op. Bei `LayerRole::Baseline` identisch zu
// `profile_replaces` (Abschnitt 7b: "alle Helfer geben sofort
// profile_replaces-Verhalten zurueck").
fn global_only<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut T,
    incoming: T,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming;
        }
        return;
    }
    if !present {
        return;
    }
    if incoming != *trusted {
        let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
        reject(out, diagnostic, role, RejectionReason::GlobalOnlyOverride);
    }
}

// `MergeRule::Union`: Vereinigung aller Layer, die das Feld setzen. Kann
// per Konstruktion **nie** einen Lockerungsversuch ablehnen (jeder
// Eintrag, den der Layer setzt, landet im Ergebnis) — daher kein
// `ScopeDiagnostic`-Ausgabeparameter.
fn union_list<T: Clone + PartialEq>(
    trusted: &mut Vec<T>,
    incoming: &[T],
    present: bool,
    role: LayerRole,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming.to_vec();
        }
        return;
    }
    if !present {
        return;
    }
    for item in incoming {
        if !trusted.contains(item) {
            trusted.push(item.clone());
        }
    }
}

// `MergeRule::Intersection` (Default-Fall: volle Wertgleichheit als
// Vergleichsschluessel, `FieldScope::intersection_key == None`).
// Schnittmenge mit dem Home-Wert als Startmenge: ein spaeterer Layer kann
// nur Eintraege entfernen (still, kein Diagnostic), niemals welche
// hinzufuegen, die im Home-Layer fehlen (Diagnostic pro abgelehntem neuen
// Eintrag).
fn intersection_list<T: Clone + PartialEq + std::fmt::Debug>(
    trusted: &mut Vec<T>,
    incoming: &[T],
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming.to_vec();
        }
        return;
    }
    if !present {
        return;
    }
    let rejected: Vec<&T> = incoming
        .iter()
        .filter(|item| !trusted.contains(item))
        .collect();
    trusted.retain(|item| incoming.contains(item));
    for item in rejected {
        let diagnostic = ScopeDiagnostic::new(field, layer_path, item);
        reject(out, diagnostic, role, RejectionReason::IntersectionNewEntry);
    }
}

// `MergeRule::MinBound`: effektiver Wert = Minimum aller Layer, die das
// Feld setzen (`min_positive`: der Default-/Sentinel-Wert eines spaeteren
// Layers senkt die Obergrenze nie weiter).
fn min_bound<T: Ord + Default + Copy + std::fmt::Debug>(
    trusted: &mut T,
    incoming: T,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming;
        }
        return;
    }
    if !present {
        return;
    }
    let bounded = min_positive(*trusted, incoming);
    if bounded != incoming {
        let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
        reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
    }
    *trusted = bounded;
}

// Wie `min_bound`, aber fuer `Option<T>`-Felder (`guards.*`,
// `permissions.approval_timeout_secs`, `compaction.absolute_ceiling_tokens`):
// ein bisher ungesetzter Home-Wert (`None`) uebernimmt den ersten explizit
// gesetzten Wert ohne Vergleich (es gibt noch keine Baseline, gegen die
// verengt werden koennte).
fn merge_optional_min_bound<T: Ord + Default + Copy + std::fmt::Debug>(
    trusted: &mut Option<T>,
    incoming: Option<T>,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    match (*trusted, incoming) {
        (None, Some(value)) => *trusted = Some(value),
        (Some(mut current), Some(value)) => {
            min_bound(&mut current, value, true, role, field, layer_path, out);
            *trusted = Some(current);
        }
        // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
        // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
        // daher auch keine Warnung.
        (_, None) => {}
    }
}

// `MergeRule::AndBool`: `true` ist der lockere/erlaubende Wert; effektiv =
// UND-Verknuepfung. Ein spaeterer Layer darf nur abschalten.
fn and_bool(
    trusted: &mut bool,
    incoming: bool,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming;
        }
        return;
    }
    if !present {
        return;
    }
    let result = *trusted && incoming;
    if result != incoming {
        let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
        reject(out, diagnostic, role, RejectionReason::AndBoolEnable);
    }
    *trusted = result;
}

// `MergeRule::OrBool`: `true` ist der strenge/sichere Wert; effektiv =
// ODER-Verknuepfung. Ein spaeterer Layer darf nur einschalten.
fn or_bool(
    trusted: &mut bool,
    incoming: bool,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming;
        }
        return;
    }
    if !present {
        return;
    }
    let result = *trusted || incoming;
    if result != incoming {
        let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
        reject(out, diagnostic, role, RejectionReason::OrBoolDisable);
    }
    *trusted = result;
}

// Wie `or_bool`, aber fuer `Option<bool>` (`guards.enabled`) — s.
// `merge_optional_min_bound`.
fn merge_optional_or_bool(
    trusted: &mut Option<bool>,
    incoming: Option<bool>,
    present: bool,
    role: LayerRole,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    match (*trusted, incoming) {
        (None, Some(value)) => *trusted = Some(value),
        (Some(mut current), Some(value)) => {
            or_bool(&mut current, value, true, role, field, layer_path, out);
            *trusted = Some(current);
        }
        // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
        // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
        // daher auch keine Warnung.
        (_, None) => {}
    }
}

// `MergeRule::StricterOf`: Ordinalwert mit expliziter Strenge-Reihenfolge
// (`ordering`, Index 0 = strengster Wert). Ein Wert ausserhalb der
// Ordnung wird **nicht** eingeordnet, sondern faellt auf `GlobalOnly`-
// Verhalten zurueck (Risiko R1, Abschnitt 8): jede Abweichung vom
// aktuellen `trusted`-Wert wird abgelehnt + diagnostiziert, ohne einen
// Vergleichsversuch zu unternehmen.
// 8 Parameter, weil `ordering` zu den 7 Parametern hinzukommt, die alle
// generischen Merge-Regel-Helfer in diesem Modul teilen (vgl. `or_bool`,
// `merge_optional_or_bool`: role/field/layer_path/out als gemeinsamer
// "Merge-Kontext"). Ein eigener Kontext-Struct nur fuer diese Funktion
// wuerde von den Schwesterfunktionen abweichen, ohne echte Komplexitaet zu
// reduzieren (nur 2 Aufrufstellen), und dieser Auftrag ist ausdruecklich
// auf reine Clippy-Fixes ohne Verhaltensaenderung beschraenkt.
#[allow(clippy::too_many_arguments)]
fn stricter_of(
    trusted: &mut String,
    incoming: String,
    present: bool,
    role: LayerRole,
    ordering: &[&str],
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming;
        }
        return;
    }
    if !present {
        return;
    }
    let trusted_rank = ordering.iter().position(|v| *v == trusted.as_str());
    let incoming_rank = ordering.iter().position(|v| *v == incoming.as_str());
    match (trusted_rank, incoming_rank) {
        (Some(t), Some(i)) if i <= t => *trusted = incoming,
        (Some(_), Some(_)) => {
            let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
            reject(out, diagnostic, role, RejectionReason::LessStrictValue);
        }
        _ => {
            // Mindestens einer der beiden Werte liegt ausserhalb der
            // Ordnung — GlobalOnly-Fallback (R1): jede Abweichung wird
            // abgelehnt, kein Vergleichsversuch.
            if incoming != *trusted {
                let diagnostic = ScopeDiagnostic::new(field, layer_path, &incoming);
                reject(out, diagnostic, role, RejectionReason::UnorderedValue);
            }
        }
    }
}

// Wie `stricter_of`, aber fuer `Option<String>` (`permissions.default_mode`)
// — s. `merge_optional_min_bound`.
// Gleiche Begruendung wie bei `stricter_of` oben: `ordering` kommt zu den
// 7 gemeinsamen Merge-Kontext-Parametern hinzu, nur eine Aufrufstelle.
#[allow(clippy::too_many_arguments)]
fn merge_optional_stricter_of(
    trusted: &mut Option<String>,
    incoming: Option<String>,
    present: bool,
    role: LayerRole,
    ordering: &[&str],
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    match (trusted.clone(), incoming) {
        (None, Some(value)) => *trusted = Some(value),
        (Some(mut current), Some(value)) => {
            stricter_of(
                &mut current,
                value,
                true,
                role,
                ordering,
                field,
                layer_path,
                out,
            );
            *trusted = Some(current);
        }
        // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
        // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
        // daher auch keine Warnung.
        (_, None) => {}
    }
}

// `McpPrincipalToml` traegt keine `#[derive(PartialEq)]`
// (harness_config.rs) — dieser Arbeitsauftrag darf harness_config.rs nicht
// anfassen, daher hier eine manuelle Feldgleichheit statt eines generischen
// `T: PartialEq`-Bounds fuer `intersection_list_by_key`.
fn principal_eq(a: &McpPrincipalToml, b: &McpPrincipalToml) -> bool {
    a.id == b.id
        && a.credential_ref == b.credential_ref
        && a.tenant == b.tenant
        && a.workspace == b.workspace
        && a.job_capabilities == b.job_capabilities
}

// `mcp_listener.principals`: `Intersection` nach `id` (Risiko R2). Bespoke
// statt `intersection_list_by_key`, weil `McpPrincipalToml` keine
// `PartialEq`-Ableitung hat (s. o.) und `Debug` fuer den vollen Struct
// zwar vorhanden ist, aber `principal_eq` fuer den Gleichheitstest
// gebraucht wird.
fn merge_mcp_listener_principals(
    trusted: &mut Vec<McpPrincipalToml>,
    incoming: &[McpPrincipalToml],
    present: bool,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if role == LayerRole::Baseline {
        if present {
            *trusted = incoming.to_vec();
        }
        return;
    }
    if !present {
        return;
    }
    for candidate in incoming {
        match trusted.iter().find(|p| p.id == candidate.id) {
            None => {
                let diagnostic = ScopeDiagnostic::new(
                    "mcp_listener.principals",
                    layer_path,
                    &format!("new principal id {:?} rejected", candidate.id),
                );
                reject(out, diagnostic, role, RejectionReason::PrincipalAdded);
            }
            Some(existing) if !principal_eq(existing, candidate) => {
                let diagnostic = ScopeDiagnostic::new(
                    "mcp_listener.principals",
                    layer_path,
                    &format!(
                        "principal id {:?} field change rejected (trusted fassung wins)",
                        candidate.id
                    ),
                );
                reject(out, diagnostic, role, RejectionReason::PrincipalChanged);
            }
            Some(_) => {}
        }
    }
    let incoming_ids: Vec<&str> = incoming.iter().map(|p| p.id.as_str()).collect();
    trusted.retain(|p| incoming_ids.contains(&p.id.as_str()));
}

// --- Sektions-Helfer (ein Aufruf pro HarnessConfig-Unterstruktur) --------

// Top-Level-Skalare (Abschnitt 1.1). `default_provider`/`default_model`/
// `active_uia_definition` reproduzieren wortgleich den bestehenden
// Sonderfall aus `discovery.rs:631-639` (Abschnitt 7f) statt neu erfunden
// zu werden.
fn merge_top_level(
    trusted: &mut HarnessConfig,
    incoming: &mut HarnessConfig,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    // `config_version`: `PerFileValidated` — nie ueber Layer gemergt; jeder
    // Layer traegt seinen eigenen Wert fuer eine an anderer Stelle
    // durchgefuehrte Schema-Versionspruefung. `trusted` uebernimmt den
    // zuletzt geladenen *vertrauten* Layer-Wert, wie es die bisherige
    // `cfg`-Zuweisung auch tat. Ein nicht vertrauter Projekt-Layer darf
    // `trusted.config_version` nie ueberschreiben (heute implizit so, da
    // `merge_restricted_harness` dieses Feld nie anfasst und
    // `apply_restricted_layer` nie `resolved.harness` wholesale ersetzt).
    if role != LayerRole::UntrustedProject {
        trusted.config_version = incoming.config_version;
    } else if field_present(raw, &["config_version"]) {
        warn_untrusted_ignored("config_version", layer_path);
    }

    // Reproduziert discovery.rs:631-639 wortgleich (Abschnitt 7f): ein
    // Layer, der das Feld nicht setzt (`None`), erbt den bisher
    // akkumulierten Wert, bevor er selbst zum neuen `trusted`-Wert wird.
    if incoming.default_provider.is_none() {
        incoming.default_provider = trusted.default_provider.clone();
    }
    if incoming.default_model.is_none() {
        incoming.default_model = trusted.default_model.clone();
    }
    if incoming.active_uia_definition.is_none() {
        incoming.active_uia_definition = trusted.active_uia_definition.clone();
    }
    if role != LayerRole::UntrustedProject {
        trusted.default_provider = incoming.default_provider.clone();
        trusted.default_model = incoming.default_model.clone();
        trusted.active_uia_definition = incoming.active_uia_definition.clone();
    } else {
        for field in ["default_provider", "default_model", "active_uia_definition"] {
            if field_present(raw, &[field]) {
                warn_untrusted_ignored(field, layer_path);
            }
        }
    }

    let present = |field: &str| field_present(raw, &[field]);

    profile_replaces(
        &mut trusted.workspace_root,
        incoming.workspace_root.clone(),
        present("workspace_root"),
        role,
        "workspace_root",
        layer_path,
    );
    profile_replaces(
        &mut trusted.active_agent_definition,
        incoming.active_agent_definition.clone(),
        present("active_agent_definition"),
        role,
        "active_agent_definition",
        layer_path,
    );
    profile_replaces(
        &mut trusted.uia_provider,
        incoming.uia_provider.clone(),
        present("uia_provider"),
        role,
        "uia_provider",
        layer_path,
    );
    profile_replaces(
        &mut trusted.uia_model,
        incoming.uia_model.clone(),
        present("uia_model"),
        role,
        "uia_model",
        layer_path,
    );
    profile_replaces(
        &mut trusted.uia_worker_model,
        incoming.uia_worker_model.clone(),
        present("uia_worker_model"),
        role,
        "uia_worker_model",
        layer_path,
    );
    profile_replaces(
        &mut trusted.project_root_markers,
        incoming.project_root_markers.clone(),
        present("project_root_markers"),
        role,
        "project_root_markers",
        layer_path,
    );
    global_only(
        &mut trusted.policy_profile,
        incoming.policy_profile.clone(),
        present("policy_profile"),
        role,
        "policy_profile",
        layer_path,
        out,
    );
}

// `[logging]` (Abschnitt 1.2) — alle drei Felder `ProfileReplaces`, nie
// diagnostizierbar.
fn merge_logging(
    trusted: &mut HarnessConfig,
    incoming: crate::harness_config::LoggingSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = |field: &str| field_present(raw, &["logging", field]);
    profile_replaces(
        &mut trusted.logging.level,
        incoming.level,
        present("level"),
        role,
        "logging.level",
        layer_path,
    );
    profile_replaces(
        &mut trusted.logging.target_module_paths,
        incoming.target_module_paths,
        present("target_module_paths"),
        role,
        "logging.target_module_paths",
        layer_path,
    );
    profile_replaces(
        &mut trusted.logging.json,
        incoming.json,
        present("json"),
        role,
        "logging.json",
        layer_path,
    );
}

// `[tui]` (Abschnitt 1.3) — beide Felder `ProfileReplaces`.
fn merge_tui(
    trusted: &mut HarnessConfig,
    incoming: TuiSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = |field: &str| field_present(raw, &["tui", field]);
    profile_replaces(
        &mut trusted.tui.theme,
        incoming.theme,
        present("theme"),
        role,
        "tui.theme",
        layer_path,
    );
    profile_replaces(
        &mut trusted.tui.keybindings_file,
        incoming.keybindings_file,
        present("keybindings_file"),
        role,
        "tui.keybindings_file",
        layer_path,
    );
}

// `[session]` (Abschnitt 1.4) — `retention_days` ist `MinBound`
// (sicherheitsrelevante Compliance-Obergrenze), der Rest `ProfileReplaces`.
fn merge_session(
    trusted: &mut HarnessConfig,
    incoming: SessionSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["session", field]);
    profile_replaces(
        &mut trusted.session.store_dir,
        incoming.store_dir,
        present("store_dir"),
        role,
        "session.store_dir",
        layer_path,
    );
    profile_replaces(
        &mut trusted.session.journal_format,
        incoming.journal_format,
        present("journal_format"),
        role,
        "session.journal_format",
        layer_path,
    );
    min_bound(
        &mut trusted.session.retention_days,
        incoming.retention_days,
        present("retention_days"),
        role,
        "session.retention_days",
        layer_path,
        out,
    );
    profile_replaces(
        &mut trusted.session.title_generation,
        incoming.title_generation,
        present("title_generation"),
        role,
        "session.title_generation",
        layer_path,
    );
    profile_replaces(
        &mut trusted.session.title_model,
        incoming.title_model,
        present("title_model"),
        role,
        "session.title_model",
        layer_path,
    );
}

// `[policy]` (Abschnitt 1.5) — beide Felder sicherheitskritisch (🔒):
// `default_visibility_scope` ist `StricterOf`, `require_approval_for` ist
// `Union`.
fn merge_policy(
    trusted: &mut HarnessConfig,
    incoming: PolicySection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["policy", field]);
    stricter_of(
        &mut trusted.policy.default_visibility_scope,
        incoming.default_visibility_scope,
        present("default_visibility_scope"),
        role,
        POLICY_VISIBILITY_SCOPE_ORDER,
        "policy.default_visibility_scope",
        layer_path,
        out,
    );
    union_list(
        &mut trusted.policy.require_approval_for,
        &incoming.require_approval_for,
        present("require_approval_for"),
        role,
    );
}

// `[mcp_listener]` (Abschnitt 1.6) — alle neun Felder sicherheitskritisch
// (🔒). `principals` seit Risiko R2 `Intersection` nach `id`, nicht mehr
// `GlobalOnly`.
fn merge_mcp_listener(
    trusted: &mut HarnessConfig,
    incoming: McpListenerSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["mcp_listener", field]);
    and_bool(
        &mut trusted.mcp_listener.enabled,
        incoming.enabled,
        present("enabled"),
        role,
        "mcp_listener.enabled",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.mcp_listener.listen_addr,
        incoming.listen_addr,
        present("listen_addr"),
        role,
        "mcp_listener.listen_addr",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.mcp_listener.path,
        incoming.path,
        present("path"),
        role,
        "mcp_listener.path",
        layer_path,
        out,
    );
    merge_mcp_listener_principals(
        &mut trusted.mcp_listener.principals,
        &incoming.principals,
        present("principals"),
        role,
        layer_path,
        out,
    );
}

// `[onboarding]` (Abschnitt 1.7) — reproduziert discovery.rs:640-642
// wortgleich (Abschnitt 7f): die ganze Tabelle ist atomar, kein
// Per-Flag-Merge. Nie vom nicht vertrauten Projekt-Layer angewendet
// (`ProfileReplaces`, Abschnitt 7c) — heute ohnehin unerreichbar
// (`discovery.rs:542-558`).
fn merge_onboarding(
    trusted: &mut HarnessConfig,
    incoming: OnboardingSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    if role == LayerRole::UntrustedProject {
        // Atomare Tabelle: genau eine Warnung fuer die ganze Tabelle.
        if field_present(raw, &["onboarding"]) {
            warn_untrusted_ignored("onboarding", layer_path);
        }
        return;
    }
    if field_present(raw, &["onboarding"]) {
        trusted.onboarding = incoming;
    }
}

// `[tools.plan]` (Abschnitt 1.8) — vier Felder `Global`
// (`OrBool`/`MinBound`), fuenf `Profile`/`ProfileReplaces`.
fn merge_tools_plan(
    trusted: &mut HarnessConfig,
    incoming: ToolsSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let plan = incoming.plan;
    let present = |field: &str| field_present(raw, &["tools", "plan", field]);
    profile_replaces(
        &mut trusted.tools.plan.enabled,
        plan.enabled,
        present("enabled"),
        role,
        "tools.plan.enabled",
        layer_path,
    );
    profile_replaces(
        &mut trusted.tools.plan.persist,
        plan.persist,
        present("persist"),
        role,
        "tools.plan.persist",
        layer_path,
    );
    profile_replaces(
        &mut trusted.tools.plan.require_for_complex_work,
        plan.require_for_complex_work,
        present("require_for_complex_work"),
        role,
        "tools.plan.require_for_complex_work",
        layer_path,
    );
    or_bool(
        &mut trusted.tools.plan.validate_dependency_cycles,
        plan.validate_dependency_cycles,
        present("validate_dependency_cycles"),
        role,
        "tools.plan.validate_dependency_cycles",
        layer_path,
        out,
    );
    or_bool(
        &mut trusted.tools.plan.validate_write_conflicts,
        plan.validate_write_conflicts,
        present("validate_write_conflicts"),
        role,
        "tools.plan.validate_write_conflicts",
        layer_path,
        out,
    );
    min_bound(
        &mut trusted.tools.plan.max_nodes,
        plan.max_nodes,
        present("max_nodes"),
        role,
        "tools.plan.max_nodes",
        layer_path,
        out,
    );
    profile_replaces(
        &mut trusted.tools.plan.require_exploration_for,
        plan.require_exploration_for,
        present("require_exploration_for"),
        role,
        "tools.plan.require_exploration_for",
        layer_path,
    );
    profile_replaces(
        &mut trusted.tools.plan.exploration_ttl_secs,
        plan.exploration_ttl_secs,
        present("exploration_ttl_secs"),
        role,
        "tools.plan.exploration_ttl_secs",
        layer_path,
    );
    min_bound(
        &mut trusted.tools.plan.max_expand_depth,
        plan.max_expand_depth,
        present("max_expand_depth"),
        role,
        "tools.plan.max_expand_depth",
        layer_path,
        out,
    );
}

// `[mode]` (Abschnitt 1.9) — einziges Feld `ProfileReplaces`.
fn merge_mode(
    trusted: &mut HarnessConfig,
    incoming: ModeSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = field_present(raw, &["mode", "default"]);
    profile_replaces(
        &mut trusted.mode.default,
        incoming.default,
        present,
        role,
        "mode.default",
        layer_path,
    );
}

// `[research]` (Abschnitt 1.10) — vier Felder sicherheitskritisch (🔒) und
// `Global`, `cache_ttl_secs` `Profile`/`ProfileReplaces`.
fn merge_research(
    trusted: &mut HarnessConfig,
    incoming: ResearchSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["research", field]);
    intersection_list(
        &mut trusted.research.network_allow_hosts,
        &incoming.network_allow_hosts,
        present("network_allow_hosts"),
        role,
        "research.network_allow_hosts",
        layer_path,
        out,
    );
    and_bool(
        &mut trusted.research.cargo_registry_read,
        incoming.cargo_registry_read,
        present("cargo_registry_read"),
        role,
        "research.cargo_registry_read",
        layer_path,
        out,
    );
    min_bound(
        &mut trusted.research.max_fetch_bytes,
        incoming.max_fetch_bytes,
        present("max_fetch_bytes"),
        role,
        "research.max_fetch_bytes",
        layer_path,
        out,
    );
    min_bound(
        &mut trusted.research.fetch_timeout_secs,
        incoming.fetch_timeout_secs,
        present("fetch_timeout_secs"),
        role,
        "research.fetch_timeout_secs",
        layer_path,
        out,
    );
    profile_replaces(
        &mut trusted.research.cache_ttl_secs,
        incoming.cache_ttl_secs,
        present("cache_ttl_secs"),
        role,
        "research.cache_ttl_secs",
        layer_path,
    );
}

// `[permissions]` (Abschnitt 1.11) — alle fuenf direkten Felder sowie
// `RuleToml.tool`/`.pattern` sicherheitskritisch (🔒).
fn merge_permissions(
    trusted: &mut HarnessConfig,
    incoming: PermissionsSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["permissions", field]);
    merge_optional_stricter_of(
        &mut trusted.permissions.default_mode,
        incoming.default_mode,
        present("default_mode"),
        role,
        PERMISSIONS_DEFAULT_MODE_ORDER,
        "permissions.default_mode",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.permissions.approval_timeout_secs,
        incoming.approval_timeout_secs,
        present("approval_timeout_secs"),
        role,
        "permissions.approval_timeout_secs",
        layer_path,
        out,
    );
    intersection_list(
        &mut trusted.permissions.allow,
        &incoming.allow,
        present("allow"),
        role,
        "permissions.allow",
        layer_path,
        out,
    );
    union_list(
        &mut trusted.permissions.deny,
        &incoming.deny,
        present("deny"),
        role,
    );
    intersection_list(
        &mut trusted.permissions.extra_roots,
        &incoming.extra_roots,
        present("extra_roots"),
        role,
        "permissions.extra_roots",
        layer_path,
        out,
    );
}

// `[sandbox]` (Abschnitt 1.12) — `cargo`/`tmux` mergen je als ein
// atomares `GlobalOnly`-`Option<T>` (`CargoSandboxToml`/`TmuxSandboxToml`
// sind Pflichtfeld-Structs: alle Unterfelder werden gemeinsam gesetzt oder
// gar nicht, ein whole-struct-Vergleich ist daher aequivalent zu acht
// unabhaengigen Einzelfeld-Vergleichen, die `FIELD_TABLE` fuer
// Exhaustivitaets-Zwecke dennoch einzeln auflistet).
fn merge_sandbox(
    trusted: &mut HarnessConfig,
    incoming: SandboxSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    global_only(
        &mut trusted.sandbox.cargo,
        incoming.cargo,
        field_present(raw, &["sandbox", "cargo"]),
        role,
        "sandbox.cargo",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.sandbox.tmux,
        incoming.tmux,
        field_present(raw, &["sandbox", "tmux"]),
        role,
        "sandbox.tmux",
        layer_path,
        out,
    );
}

// `[internal_models]` (Abschnitt 1.13) — nachgebildet (nicht neu erfunden,
// Abschnitt 7f) aus `discovery::merge_internal_models`
// (`discovery.rs:846-866`): ein Feld, das der Layer selbst nicht setzt,
// erbt den akkumulierten `trusted`-Wert statt auf den Section-Default
// zurueckzufallen. Eigene Kopie statt Aufruf der (modul-privaten)
// `discovery::merge_internal_models`, aus demselben Grund wie
// `field_present`/`min_positive` oben. Nie vom nicht vertrauten
// Projekt-Layer angewendet (`ProfileReplaces`, Abschnitt 7c).
fn merge_internal_models(
    trusted: &mut HarnessConfig,
    mut incoming: InternalModelsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    if role == LayerRole::UntrustedProject {
        // Eine Warnung fuer die ganze Tabelle (nie uebernommen, egal
        // welche Unterfelder gesetzt sind).
        if field_present(raw, &["internal_models"]) {
            warn_untrusted_ignored("internal_models", layer_path);
        }
        return;
    }
    let Some(raw_table) = raw.get("internal_models") else {
        // Diese Layer hat gar keine `[internal_models]`-Tabelle: trusted
        // bleibt vollstaendig bestehen.
        return;
    };
    if raw_table.get("use_openrouter_defaults").is_none() {
        incoming.use_openrouter_defaults = trusted.internal_models.use_openrouter_defaults;
    }
    for point in crate::internal_models::InternalModelPoint::ALL {
        if raw_table.get(point.key()).is_none() {
            incoming.set_choice(point, trusted.internal_models.choice(point).cloned());
        }
    }
    trusted.internal_models = incoming;
}

// `[compaction]` (Abschnitt 1.14) — einziges Feld `MinBound` (Kostenobergrenze).
fn merge_compaction(
    trusted: &mut HarnessConfig,
    incoming: CompactionToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    merge_optional_min_bound(
        &mut trusted.compaction.absolute_ceiling_tokens,
        incoming.absolute_ceiling_tokens,
        field_present(raw, &["compaction", "absolute_ceiling_tokens"]),
        role,
        "compaction.absolute_ceiling_tokens",
        layer_path,
        out,
    );
    profile_replaces(
        &mut trusted.compaction.max_history_bytes,
        incoming.max_history_bytes,
        field_present(raw, &["compaction", "max_history_bytes"]),
        role,
        "compaction.max_history_bytes",
        layer_path,
    );
}

// `[reasoning]` (Abschnitt 1.15) — alle sechs Felder `ProfileReplaces`.
fn merge_reasoning(
    trusted: &mut HarnessConfig,
    incoming: ReasoningWeightsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = |field: &str| field_present(raw, &["reasoning", field]);
    profile_replaces(
        &mut trusted.reasoning.uia,
        incoming.uia,
        present("uia"),
        role,
        "reasoning.uia",
        layer_path,
    );
    profile_replaces(
        &mut trusted.reasoning.root_orchestrator,
        incoming.root_orchestrator,
        present("root_orchestrator"),
        role,
        "reasoning.root_orchestrator",
        layer_path,
    );
    profile_replaces(
        &mut trusted.reasoning.root_orchestrator_with_subs,
        incoming.root_orchestrator_with_subs,
        present("root_orchestrator_with_subs"),
        role,
        "reasoning.root_orchestrator_with_subs",
        layer_path,
    );
    profile_replaces(
        &mut trusted.reasoning.sub_orchestrator,
        incoming.sub_orchestrator,
        present("sub_orchestrator"),
        role,
        "reasoning.sub_orchestrator",
        layer_path,
    );
    profile_replaces(
        &mut trusted.reasoning.worker_complex,
        incoming.worker_complex,
        present("worker_complex"),
        role,
        "reasoning.worker_complex",
        layer_path,
    );
    profile_replaces(
        &mut trusted.reasoning.worker_simple,
        incoming.worker_simple,
        present("worker_simple"),
        role,
        "reasoning.worker_simple",
        layer_path,
    );
}

// `[guards]` (Abschnitt 1.16) — `enabled` `OrBool`, die uebrigen fuenf
// `MinBound`, alle als `Option<T>` (Laufzeit-Default liegt beim Consumer).
fn merge_guards(
    trusted: &mut HarnessConfig,
    incoming: GuardsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["guards", field]);
    merge_optional_or_bool(
        &mut trusted.guards.enabled,
        incoming.enabled,
        present("enabled"),
        role,
        "guards.enabled",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.repeated_failure_warn,
        incoming.repeated_failure_warn,
        present("repeated_failure_warn"),
        role,
        "guards.repeated_failure_warn",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.repeated_failure_abort,
        incoming.repeated_failure_abort,
        present("repeated_failure_abort"),
        role,
        "guards.repeated_failure_abort",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.no_progress_rounds_warn,
        incoming.no_progress_rounds_warn,
        present("no_progress_rounds_warn"),
        role,
        "guards.no_progress_rounds_warn",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.no_progress_rounds_abort,
        incoming.no_progress_rounds_abort,
        present("no_progress_rounds_abort"),
        role,
        "guards.no_progress_rounds_abort",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.plan_stale_rounds,
        incoming.plan_stale_rounds,
        present("plan_stale_rounds"),
        role,
        "guards.plan_stale_rounds",
        layer_path,
        out,
    );
}

/// Wendet `incoming` (bereits vollständig deserialisierte `HarnessConfig`
/// dieses Layers, inkl. dessen eigener Section-Defaults für alles, was der
/// Layer nicht selbst setzt) gemäß `crate::scope::FIELD_TABLE` und `role`
/// auf `trusted` (der bisher akkumulierte Stand) an.
///
/// # Description
/// Ersetzt `resolved.harness = cfg` (`discovery.rs:649`) für vertraute
/// Layer und `merge_restricted_harness` (`discovery.rs:942-1003`) für den
/// nicht vertrauten Projekt-Layer — beide Aufrufer unterscheiden sich nur
/// im übergebenen `role` (Paket B verdrahtet diese Ersetzung in
/// `discovery.rs`, siehe Modul-Doku).
///
/// `raw` ist das **rohe**, noch nicht von `[network]`/`[browser]`/`[dod]`/
/// `[web]` bereinigte `toml::Value` dieses Layers und entscheidet je Feld
/// per Präsenzprüfung, ob dieser Layer das Feld überhaupt selbst gesetzt
/// hat (statt sich auf Serde-Defaults in `incoming` zu verlassen, die für
/// nicht gesetzte Felder identisch zu explizit gesetzten Default-Werten
/// aussehen).
///
/// # Arguments
/// - `trusted` (`&mut HarnessConfig`): der bisher akkumulierte, vertrauens-
///   würdige Stand über alle zuvor verarbeiteten Layer.
/// - `incoming` (`HarnessConfig`): die vollständig deserialisierte
///   `HarnessConfig` dieses einen Layers.
/// - `raw` (`&toml::Value`): das rohe TOML-Dokument dieses Layers, für
///   Präsenzprüfungen.
/// - `role` (`LayerRole`): Baseline/Refinement/UntrustedProject — bestimmt,
///   welche `MergeRule`-Varianten überhaupt wirken (Abschnitt 7c).
/// - `layer_path` (`&Path`): Pfad der `config.toml` dieses Layers, für
///   `ScopeDiagnostic::file`.
///
/// # Returns
/// Jede `ScopeDiagnostic`, die durch einen abgelehnten Lockerungs-/
/// Erweiterungsversuch entstanden ist (Abschnitt 7c/7e). Leer, wenn kein
/// Layer-Wert verworfen wurde.
///
/// # Concurrency
/// Rein synchron, keine gemeinsam genutzten Zustände.
///
/// # Examples
/// ```rust,no_run
/// use harw_config::{merge_layer_into, HarnessConfig, LayerRole};
/// use std::path::Path;
///
/// let mut trusted = HarnessConfig::default();
/// let incoming = HarnessConfig::default();
/// let raw: toml::Value = toml::from_str("").unwrap();
/// let _diagnostics = merge_layer_into(
///     &mut trusted,
///     incoming,
///     &raw,
///     LayerRole::Refinement,
///     Path::new("/home/user/.harw/profiles/default/config.toml"),
/// );
/// ```
pub fn merge_layer_into(
    trusted: &mut HarnessConfig,
    incoming: HarnessConfig,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) -> Vec<ScopeDiagnostic> {
    let mut out = Vec::new();
    let mut incoming = incoming;

    merge_top_level(trusted, &mut incoming, raw, role, layer_path, &mut out);
    merge_logging(trusted, incoming.logging, raw, role, layer_path);
    merge_tui(trusted, incoming.tui, raw, role, layer_path);
    merge_session(trusted, incoming.session, raw, role, layer_path, &mut out);
    merge_policy(trusted, incoming.policy, raw, role, layer_path, &mut out);
    merge_mcp_listener(
        trusted,
        incoming.mcp_listener,
        raw,
        role,
        layer_path,
        &mut out,
    );
    merge_onboarding(trusted, incoming.onboarding, raw, role, layer_path);
    merge_tools_plan(trusted, incoming.tools, raw, role, layer_path, &mut out);
    merge_mode(trusted, incoming.mode, raw, role, layer_path);
    merge_research(trusted, incoming.research, raw, role, layer_path, &mut out);
    merge_permissions(
        trusted,
        incoming.permissions,
        raw,
        role,
        layer_path,
        &mut out,
    );
    merge_sandbox(trusted, incoming.sandbox, raw, role, layer_path, &mut out);
    merge_internal_models(trusted, incoming.internal_models, raw, role, layer_path);
    merge_compaction(
        trusted,
        incoming.compaction,
        raw,
        role,
        layer_path,
        &mut out,
    );
    merge_reasoning(trusted, incoming.reasoning, raw, role, layer_path);
    merge_guards(trusted, incoming.guards, raw, role, layer_path, &mut out);

    // `base_dir`: `#[serde(skip)]`, kein TOML-Feld, kein `FIELD_TABLE`-
    // Eintrag (Abschnitt 1.1, "89. Zeile"). Reine Buchführung, die dem
    // heutigen `cfg.base_dir = Some(base.clone())` + `resolved.harness =
    // cfg`-Verhalten entspricht (discovery.rs:648-649): der zuletzt
    // verarbeitete *vertraute* Layer bestimmt `base_dir`, ohne
    // MergeRule-Anwendung. Ein nicht vertrauter Projekt-Layer darf ihn nie
    // setzen (heute implizit so: `apply_restricted_layer` ersetzt
    // `resolved.harness` nie wholesale, s. o.).
    if role != LayerRole::UntrustedProject {
        trusted.base_dir = incoming.base_dir;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::path::PathBuf;

    fn layer_path() -> PathBuf {
        PathBuf::from("/home/user/.harw/profiles/default/config.toml")
    }

    fn raw_from(src: &str) -> TestResult<toml::Value> {
        toml::from_str(src).map_err(ctx("valid toml fixture"))
    }

    // Test 1 (Abschnitt 7h): ProfileReplaces — Profil laesst das Feld
    // unbenutzt, Home-Wert bleibt erhalten statt auf den Default zu
    // fallen.
    #[test]
    fn test_profile_replaces_keeps_trusted_value_when_layer_omits_field() -> TestResult {
        let mut trusted = HarnessConfig::default();
        trusted.logging.level = "debug".to_owned();
        let incoming = HarnessConfig::default();
        let raw = raw_from("default_provider = \"anthropic\"")?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.logging.level, "debug");
        assert!(diagnostics.is_empty());
        Ok(())
    }

    // uia_worker_model (Abschnitt 6.3, 1.1): ProfileReplaces — ein
    // vertrautes Profil setzt einen neuen Wert und ersetzt den Home-Wert.
    #[test]
    fn test_uia_worker_model_profile_replaces_overrides_trusted_value() -> TestResult {
        let mut trusted = HarnessConfig {
            uia_worker_model: Some("claude-old".to_owned()),
            ..Default::default()
        };
        let incoming = HarnessConfig {
            uia_worker_model: Some("claude-new".to_owned()),
            ..Default::default()
        };
        let raw = raw_from("uia_worker_model = \"claude-new\"")?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.uia_worker_model.as_deref(), Some("claude-new"));
        assert!(diagnostics.is_empty());
        Ok(())
    }

    // uia_worker_model gegen einen nicht vertrauten Projekt-Layer: `profile_replaces`
    // gibt bei `LayerRole::UntrustedProject` sofort zurueck (Abschnitt 7c) —
    // der Projekt-Layer kann den Wert nicht setzen, auch wenn er ihn im
    // rohen TOML traegt.
    #[test]
    fn test_uia_worker_model_untrusted_project_layer_cannot_set_it() -> TestResult {
        let mut trusted = HarnessConfig {
            uia_worker_model: Some("claude-trusted".to_owned()),
            ..Default::default()
        };
        let incoming = HarnessConfig {
            uia_worker_model: Some("claude-evil".to_owned()),
            ..Default::default()
        };
        let raw = raw_from("uia_worker_model = \"claude-evil\"")?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.uia_worker_model.as_deref(), Some("claude-trusted"));
        assert!(diagnostics.is_empty());
        Ok(())
    }

    // Test 3: GlobalOnly — Profil versucht sandbox.cargo zu aendern.
    #[test]
    fn test_global_only_rejects_profile_override_and_emits_diagnostic() -> TestResult {
        use crate::harness_config::{CargoSandboxModeToml, CargoSandboxToml};

        let mut trusted = HarnessConfig::default();
        trusted.sandbox.cargo = Some(CargoSandboxToml {
            mode: CargoSandboxModeToml::Inspect,
            cargo_bin: "/opt/harw/cargo".to_owned(),
            rustup_home: "/opt/harw/rustup".to_owned(),
            cargo_home: "/opt/harw/cargo-home".to_owned(),
        });

        let mut incoming = HarnessConfig::default();
        incoming.sandbox.cargo = Some(CargoSandboxToml {
            mode: CargoSandboxModeToml::Fetch,
            cargo_bin: "/evil/cargo".to_owned(),
            rustup_home: "/opt/harw/rustup".to_owned(),
            cargo_home: "/opt/harw/cargo-home".to_owned(),
        });

        let raw = raw_from(
            r#"
                [sandbox.cargo]
                mode = "fetch"
                cargo_bin = "/evil/cargo"
                rustup_home = "/opt/harw/rustup"
                cargo_home = "/opt/harw/cargo-home"
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        let cargo = trusted
            .sandbox
            .cargo
            .as_ref()
            .ok_or(TestError::Missing("sandbox.cargo after merge"))?;
        assert_eq!(cargo.cargo_bin, "/opt/harw/cargo");
        assert!(diagnostics.iter().any(|d| d.field == "sandbox.cargo"));
        Ok(())
    }

    // Test 4: Union — beide Layer-Eintraege ueberleben.
    #[test]
    fn test_union_combines_entries_from_both_layers() -> TestResult {
        let mut trusted = HarnessConfig::default();
        trusted.policy.require_approval_for = vec!["shell.exec".to_owned()];
        let mut incoming = HarnessConfig::default();
        incoming.policy.require_approval_for = vec!["fs.write".to_owned()];
        let raw = raw_from(
            r#"
                [policy]
                require_approval_for = ["fs.write"]
            "#,
        )?;
        merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.policy.require_approval_for.len(), 2);
        assert!(
            trusted
                .policy
                .require_approval_for
                .contains(&"shell.exec".to_owned())
        );
        assert!(
            trusted
                .policy
                .require_approval_for
                .contains(&"fs.write".to_owned())
        );
        Ok(())
    }

    // Test 5: Intersection — ein neuer Eintrag wird verworfen +
    // diagnostiziert.
    #[test]
    fn test_intersection_drops_new_entry_and_emits_diagnostic() -> TestResult {
        use crate::permissions_toml::RuleToml;

        let mut trusted = HarnessConfig::default();
        trusted.permissions.allow = vec![
            RuleToml {
                tool: "shell.exec".to_owned(),
                pattern: None,
            },
            RuleToml {
                tool: "fs.read".to_owned(),
                pattern: None,
            },
        ];
        let mut incoming = HarnessConfig::default();
        incoming.permissions.allow = vec![
            RuleToml {
                tool: "shell.exec".to_owned(),
                pattern: None,
            },
            RuleToml {
                tool: "fs.write".to_owned(),
                pattern: None,
            },
        ];
        let raw = raw_from(
            r#"
                [[permissions.allow]]
                tool = "shell.exec"
                [[permissions.allow]]
                tool = "fs.write"
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.permissions.allow.len(), 1);
        assert_eq!(trusted.permissions.allow[0].tool, "shell.exec");
        assert!(diagnostics.iter().any(|d| d.field == "permissions.allow"));
        Ok(())
    }

    // Test 6: MinBound.
    #[test]
    fn test_min_bound_rejects_higher_value_and_keeps_lower() -> TestResult {
        let mut trusted = HarnessConfig::default();
        trusted.guards.repeated_failure_warn = Some(2);
        let mut incoming = HarnessConfig::default();
        incoming.guards.repeated_failure_warn = Some(5);
        let raw = raw_from(
            r#"
                [guards]
                repeated_failure_warn = 5
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.guards.repeated_failure_warn, Some(2));
        assert!(
            diagnostics
                .iter()
                .any(|d| d.field == "guards.repeated_failure_warn")
        );
        Ok(())
    }

    // Test 7: AndBool.
    #[test]
    fn test_and_bool_keeps_false_when_profile_tries_to_enable() -> TestResult {
        let mut trusted = HarnessConfig::default();
        trusted.mcp_listener.enabled = false;
        let mut incoming = HarnessConfig::default();
        incoming.mcp_listener.enabled = true;
        let raw = raw_from(
            r#"
                [mcp_listener]
                enabled = true
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert!(!trusted.mcp_listener.enabled);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.field == "mcp_listener.enabled")
        );
        Ok(())
    }

    // Test 9 + 24: StricterOf inkl. R1-Fallback fuer unbekannte Werte.
    #[test]
    fn test_stricter_of_falls_back_to_global_only_for_unknown_value() -> TestResult {
        let mut trusted = HarnessConfig::default();
        trusted.policy.default_visibility_scope = "self".to_owned();
        let mut incoming = HarnessConfig::default();
        incoming.policy.default_visibility_scope = "team".to_owned();
        let raw = raw_from(
            r#"
                [policy]
                default_visibility_scope = "team"
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.policy.default_visibility_scope, "self");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.field == "policy.default_visibility_scope"
                    && d.rejected_value.contains("team"))
        );
        Ok(())
    }

    // Test 25/26/27: mcp_listener.principals Intersection-by-key (R2).
    #[test]
    fn test_mcp_listener_principals_allows_removal_rejects_addition_and_field_change() -> TestResult
    {
        use crate::auth_toml::SecretRef;
        use crate::harness_config::McpPrincipalToml;
        use std::str::FromStr;

        let p1 = McpPrincipalToml {
            id: "p1".to_owned(),
            credential_ref: SecretRef::from_str("env:P1_TOKEN").map_err(ctx("env:P1_TOKEN"))?,
            tenant: "mia".to_owned(),
            workspace: "harwness".to_owned(),
            job_capabilities: vec![],
        };
        let p2 = McpPrincipalToml {
            id: "p2".to_owned(),
            credential_ref: SecretRef::from_str("env:P2_TOKEN").map_err(ctx("env:P2_TOKEN"))?,
            tenant: "mia".to_owned(),
            workspace: "harwness".to_owned(),
            job_capabilities: vec![],
        };

        let mut trusted = HarnessConfig::default();
        trusted.mcp_listener.principals = vec![p1.clone(), p2];
        let mut incoming = HarnessConfig::default();
        incoming.mcp_listener.principals = vec![p1];
        let raw = raw_from(
            r#"
                [[mcp_listener.principals]]
                id = "p1"
                credential_ref = "env:P1_TOKEN"
                tenant = "mia"
                workspace = "harwness"
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.mcp_listener.principals.len(), 1);
        assert_eq!(trusted.mcp_listener.principals[0].id, "p1");
        assert!(
            diagnostics.is_empty(),
            "removal alone must not be diagnosed"
        );
        Ok(())
    }

    // Test 21 (Regression): Baseline-Layer setzt die erste Vereinigungsmenge
    // ohne Diagnostic, egal welche Rolle spaeter kommt.
    #[test]
    fn test_baseline_layer_behaves_like_profile_replaces_for_every_rule() -> TestResult {
        let mut trusted = HarnessConfig::default();
        let mut incoming = HarnessConfig::default();
        incoming.mcp_listener.enabled = true;
        incoming.guards.repeated_failure_warn = Some(9);
        let raw = raw_from(
            r#"
                [mcp_listener]
                enabled = true
                [guards]
                repeated_failure_warn = 9
            "#,
        )?;
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert!(trusted.mcp_listener.enabled);
        assert_eq!(trusted.guards.repeated_failure_warn, Some(9));
        assert!(diagnostics.is_empty());
        Ok(())
    }
}
