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
//! Nur der öffentliche `&str`-Einstieg `merge_layer_toml_into` kann
//! scheitern: er parst das rohe TOML selbst und liefert für ungültiges TOML
//! `ConfigError::TomlParse`.
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
//! ```rust
//! use harw_config::{merge_layer_toml_into, HarnessConfig, LayerRole};
//! use std::path::Path;
//!
//! let mut trusted = HarnessConfig::default();
//! let incoming = HarnessConfig::default();
//! let diagnostics = merge_layer_toml_into(
//!     &mut trusted,
//!     incoming,
//!     "",
//!     LayerRole::Baseline,
//!     Path::new("/home/user/.harw/config.toml"),
//! )?;
//! assert!(diagnostics.is_empty());
//! # Ok::<(), harw_config::ConfigError>(())
//! ```

use std::path::Path;

use crate::error::{ConfigError, ConfigResult};
use crate::harness_config::{
    CompactionToml, DreamToml, GuardsToml, HarnessConfig, HostToml, KnowledgeToml,
    McpListenerSection, McpPrincipalToml, OnboardingSection, PolicySection, ReasoningWeightsToml,
    SandboxSection, SessionSection, TuiSection,
};
use crate::internal_models::InternalModelsToml;
use crate::memory_toml::MemorySection;
use crate::mode_toml::ModeSection;
use crate::permissions_toml::PermissionsSection;
use crate::plan_toml::ToolsSection;
use crate::research_toml::ResearchSection;
use crate::retention_toml::RetentionSection;
use crate::scope::{PERMISSIONS_DEFAULT_MODE_ORDER, POLICY_VISIBILITY_SCOPE_ORDER};
use crate::uia_worker_models::UiaWorkerModelsToml;

/// Grober Vertrauens-/Ebenen-Kontext eines `merge_layer_into`-Aufrufs;
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

/// Ein abgelehnter Scope-Lockerungsversuch aus `merge_layer_into`
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

// Eingebaute Vorgaben, gegen die der nicht vertraute Projekt-Layer
// verglichen wird, wenn noch kein vertrauter Layer den Schluessel gesetzt
// hat (gleiches Muster wie `merge_host`/`merge_agent_limits`/
// `merge_shell_limits`). Sie spiegeln `harw_runtime::assembly::
// default_approval_mode` (`Delegated` = `"auto"`) und
// `harw_core::guard::GuardPolicy::default`. harw-config (Ring I) darf keine
// Ring-A-Crates importieren, daher hier dupliziert — eine Aenderung dort
// muss hier nachgezogen werden.
const UNTRUSTED_BASELINE_DEFAULT_MODE: &str = "auto";
const UNTRUSTED_BASELINE_GUARDS_ENABLED: bool = true;
const UNTRUSTED_BASELINE_REPEATED_FAILURE_WARN: u32 = 2;
const UNTRUSTED_BASELINE_REPEATED_FAILURE_ABORT: u32 = 3;
const UNTRUSTED_BASELINE_NO_PROGRESS_ROUNDS_WARN: u32 = 4;
const UNTRUSTED_BASELINE_NO_PROGRESS_ROUNDS_ABORT: u32 = 8;
const UNTRUSTED_BASELINE_PLAN_STALE_ROUNDS: u32 = 6;
const UNTRUSTED_BASELINE_ORCHESTRATOR_READ_WARN: u32 = 4;
const UNTRUSTED_BASELINE_ORCHESTRATOR_READ_LIMIT: u32 = 5;

// Wie `min_bound`, aber fuer `Option<T>`-Felder (`guards.*`,
// `permissions.approval_timeout_secs`, `compaction.absolute_ceiling_tokens`).
// Ist noch kein Wert gesetzt (`None`), uebernimmt ein vertrauter Layer den
// ersten explizit gesetzten Wert ohne Vergleich. Der nicht vertraute
// Projekt-Layer wird dann gegen `untrusted_baseline` verglichen: nur ein
// Wert, der hoechstens so gross ist, wird uebernommen, der Default-/
// Sentinel-Wert (`0`) nie. `untrusted_baseline == None` heisst: in diesem
// Crate ist keine Vorgabe bekannt, es bleibt beim bisherigen Verhalten.
// 8 Parameter, weil `untrusted_baseline` zu den 7 gemeinsamen
// Merge-Kontext-Parametern hinzukommt (wie `ordering` bei `stricter_of`).
#[allow(clippy::too_many_arguments)]
fn merge_optional_min_bound<T: Ord + Default + Copy + std::fmt::Debug>(
    trusted: &mut Option<T>,
    incoming: Option<T>,
    present: bool,
    role: LayerRole,
    untrusted_baseline: Option<T>,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
    // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
    // daher auch keine Warnung.
    let Some(value) = incoming else {
        return;
    };
    match *trusted {
        Some(mut current) => {
            min_bound(&mut current, value, true, role, field, layer_path, out);
            *trusted = Some(current);
        }
        None => match (role, untrusted_baseline) {
            (LayerRole::UntrustedProject, Some(baseline)) => {
                if value != T::default() && value <= baseline {
                    *trusted = Some(value);
                } else {
                    let diagnostic = ScopeDiagnostic::new(field, layer_path, &value);
                    reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
                }
            }
            _ => *trusted = Some(value),
        },
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
// `merge_optional_min_bound`: ein vertrauter Layer uebernimmt den ersten
// Wert ohne Vergleich, der nicht vertraute Projekt-Layer darf ohne
// vertrauten Wert nur `true` setzen oder die Vorgabe `untrusted_baseline`
// wiederholen (abschalten ist nur erlaubt, wenn schon die Vorgabe `false`
// ist).
// Gleiche Begruendung wie bei `merge_optional_min_bound`:
// `untrusted_baseline` kommt zu den 7 gemeinsamen Merge-Kontext-Parametern.
#[allow(clippy::too_many_arguments)]
fn merge_optional_or_bool(
    trusted: &mut Option<bool>,
    incoming: Option<bool>,
    present: bool,
    role: LayerRole,
    untrusted_baseline: bool,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
    // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
    // daher auch keine Warnung.
    let Some(value) = incoming else {
        return;
    };
    match *trusted {
        Some(mut current) => {
            or_bool(&mut current, value, true, role, field, layer_path, out);
            *trusted = Some(current);
        }
        None if role == LayerRole::UntrustedProject => {
            if value || !untrusted_baseline {
                *trusted = Some(value);
            } else {
                let diagnostic = ScopeDiagnostic::new(field, layer_path, &value);
                reject(out, diagnostic, role, RejectionReason::OrBoolDisable);
            }
        }
        None => *trusted = Some(value),
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
// — s. `merge_optional_min_bound`: ein vertrauter Layer uebernimmt den
// ersten Wert ohne Vergleich, der nicht vertraute Projekt-Layer wird ohne
// vertrauten Wert gegen `untrusted_baseline` eingeordnet (nur gleich streng
// oder strenger wird uebernommen; ein Wert ausserhalb der Ordnung nur, wenn
// er der Vorgabe gleicht — R1-Fallback wie in `stricter_of`).
// Gleiche Begruendung wie bei `stricter_of` oben: `ordering` und
// `untrusted_baseline` kommen zu den 7 gemeinsamen Merge-Kontext-Parametern
// hinzu, nur eine Aufrufstelle.
#[allow(clippy::too_many_arguments)]
fn merge_optional_stricter_of(
    trusted: &mut Option<String>,
    incoming: Option<String>,
    present: bool,
    role: LayerRole,
    ordering: &[&str],
    untrusted_baseline: &str,
    field: &str,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    if !present {
        return;
    }
    // `present` mit `None` ist per Serde nicht erreichbar (ein gesetzter
    // Schluessel deserialisiert immer zu `Some`) — nichts verworfen,
    // daher auch keine Warnung.
    let Some(value) = incoming else {
        return;
    };
    match trusted.clone() {
        Some(mut current) => {
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
        None if role == LayerRole::UntrustedProject => {
            let base = ordering.iter().position(|v| *v == untrusted_baseline);
            let rank = ordering.iter().position(|v| *v == value.as_str());
            match (base, rank) {
                (Some(b), Some(i)) if i <= b => *trusted = Some(value),
                (Some(_), Some(_)) => {
                    let diagnostic = ScopeDiagnostic::new(field, layer_path, &value);
                    reject(out, diagnostic, role, RejectionReason::LessStrictValue);
                }
                _ => {
                    // Mindestens einer der beiden Werte liegt ausserhalb der
                    // Ordnung — R1-Fallback: jede Abweichung von der Vorgabe
                    // wird abgelehnt, `trusted` bleibt `None`.
                    if value != untrusted_baseline {
                        let diagnostic = ScopeDiagnostic::new(field, layer_path, &value);
                        reject(out, diagnostic, role, RejectionReason::UnorderedValue);
                    }
                }
            }
        }
        None => *trusted = Some(value),
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
    // Runde 5, Teil I: `[tui] child_stream` (rein darstellend, `ProfileReplaces`).
    profile_replaces(
        &mut trusted.tui.child_stream,
        incoming.child_stream,
        present("child_stream"),
        role,
        "tui.child_stream",
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

// `[tools.doc]` (Abschnitt 1.8a) — `remote_ocr`. Wie `merge_shell_limits`:
// vertraute Layer (Home und Profil) setzen frei, auch lockernd; ein nicht
// vertrautes Projekt darf nur verschärfen (`off` < `ask` < `on`), nie etwa
// von `ask` auf `on` wechseln.
fn merge_tools_doc(
    trusted: &mut HarnessConfig,
    incoming: crate::plan_toml::DocSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    const FIELD: &str = "tools.doc.remote_ocr";
    if !field_present(raw, &["tools", "doc", "remote_ocr"]) {
        return;
    }
    let value = incoming.remote_ocr;
    if role != LayerRole::UntrustedProject || value <= trusted.tools.doc.remote_ocr {
        trusted.tools.doc.remote_ocr = value;
    } else {
        let diagnostic = ScopeDiagnostic::new(FIELD, layer_path, &value.as_str());
        reject(out, diagnostic, role, RejectionReason::LessStrictValue);
    }
}

// `[session_listener]` — alle sechs Felder `GlobalOnly` und
// sicherheitskritisch: nur die Baseline (Home) setzt sie.
fn merge_session_listener(
    trusted: &mut HarnessConfig,
    incoming: crate::session_listener_toml::SessionListenerSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["session_listener", field]);
    global_only(
        &mut trusted.session_listener.enabled,
        incoming.enabled,
        present("enabled"),
        role,
        "session_listener.enabled",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.session_listener.listen,
        incoming.listen,
        present("listen"),
        role,
        "session_listener.listen",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.session_listener.allow_non_loopback,
        incoming.allow_non_loopback,
        present("allow_non_loopback"),
        role,
        "session_listener.allow_non_loopback",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.session_listener.node_id,
        incoming.node_id,
        present("node_id"),
        role,
        "session_listener.node_id",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.session_listener.tier,
        incoming.tier,
        present("tier"),
        role,
        "session_listener.tier",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.session_listener.approval_device,
        incoming.approval_device,
        present("approval_device"),
        role,
        "session_listener.approval_device",
        layer_path,
        out,
    );
}

// `[tools.container]` — alle vier Felder `GlobalOnly` und sicherheitskritisch:
// Home/Profil (Baseline) setzen sie, ein Projekt kann sie nie ändern.
fn merge_tools_container(
    trusted: &mut HarnessConfig,
    incoming: crate::plan_toml::ContainerToolsSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    let present = |field: &str| field_present(raw, &["tools", "container", field]);
    global_only(
        &mut trusted.tools.container.enabled,
        incoming.enabled,
        present("enabled"),
        role,
        "tools.container.enabled",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.tools.container.engine,
        incoming.engine,
        present("engine"),
        role,
        "tools.container.engine",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.tools.container.connection,
        incoming.connection,
        present("connection"),
        role,
        "tools.container.connection",
        layer_path,
        out,
    );
    global_only(
        &mut trusted.tools.container.images,
        incoming.images,
        present("images"),
        role,
        "tools.container.images",
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

// `[memory]` — alle zehn Felder `ProfileReplaces`: Home und Profil ersetzen,
// ein nicht vertrautes Projekt darf keines setzen (wird gemeldet und
// verworfen).
fn merge_memory(
    trusted: &mut HarnessConfig,
    incoming: MemorySection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = |field: &str| field_present(raw, &["memory", field]);
    profile_replaces(
        &mut trusted.memory.enabled,
        incoming.enabled,
        present("enabled"),
        role,
        "memory.enabled",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.global_enabled,
        incoming.global_enabled,
        present("global_enabled"),
        role,
        "memory.global_enabled",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.token_budget,
        incoming.token_budget,
        present("token_budget"),
        role,
        "memory.token_budget",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.max_facts,
        incoming.max_facts,
        present("max_facts"),
        role,
        "memory.max_facts",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.max_body_bytes,
        incoming.max_body_bytes,
        present("max_body_bytes"),
        role,
        "memory.max_body_bytes",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.max_unused_days,
        incoming.max_unused_days,
        present("max_unused_days"),
        role,
        "memory.max_unused_days",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.consolidate_deadline_secs,
        incoming.consolidate_deadline_secs,
        present("consolidate_deadline_secs"),
        role,
        "memory.consolidate_deadline_secs",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.forget_deadline_secs,
        incoming.forget_deadline_secs,
        present("forget_deadline_secs"),
        role,
        "memory.forget_deadline_secs",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.promote_deadline_secs,
        incoming.promote_deadline_secs,
        present("promote_deadline_secs"),
        role,
        "memory.promote_deadline_secs",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.sweep_deadline_secs,
        incoming.sweep_deadline_secs,
        present("sweep_deadline_secs"),
        role,
        "memory.sweep_deadline_secs",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.context_ledger,
        incoming.context_ledger,
        present("context_ledger"),
        role,
        "memory.context_ledger",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.security_signals,
        incoming.security_signals,
        present("security_signals"),
        role,
        "memory.security_signals",
        layer_path,
    );
    profile_replaces(
        &mut trusted.memory.llm_extraction,
        incoming.llm_extraction,
        present("llm_extraction"),
        role,
        "memory.llm_extraction",
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
        UNTRUSTED_BASELINE_DEFAULT_MODE,
        "permissions.default_mode",
        layer_path,
        out,
    );
    // Keine Vorgabe (`None`): sie liegt in harw-runtime, und eine laengere
    // Wartezeit verleiht keine Befugnis.
    merge_optional_min_bound(
        &mut trusted.permissions.approval_timeout_secs,
        incoming.approval_timeout_secs,
        present("approval_timeout_secs"),
        role,
        None,
        "permissions.approval_timeout_secs",
        layer_path,
        out,
    );
    // Runde 7, Teil L4: Klassifizierer-Zeitlimit; wie das Freigabe-Zeitlimit
    // darf eine spätere Ebene nur verkürzen. Keine Vorgabe (`None`), aus
    // demselben Grund wie beim Freigabe-Zeitlimit.
    merge_optional_min_bound(
        &mut trusted.permissions.auto_classifier_timeout_secs,
        incoming.auto_classifier_timeout_secs,
        present("auto_classifier_timeout_secs"),
        role,
        None,
        "permissions.auto_classifier_timeout_secs",
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

// `[uia_worker_models]` (Abschnitt 1.20, Runde 5 Teil G) — wie
// `[internal_models]`: jede Rolle `ProfileReplaces`, ein Feld, das der Layer
// nicht setzt, erbt den akkumulierten `trusted`-Wert. Nie vom nicht
// vertrauten Projekt-Layer angewendet (ein Projekt darf nicht bestimmen,
// welches Modell die UIA-Worker ansprechen).
fn merge_uia_worker_models(
    trusted: &mut HarnessConfig,
    mut incoming: UiaWorkerModelsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    if role == LayerRole::UntrustedProject {
        if field_present(raw, &["uia_worker_models"]) {
            warn_untrusted_ignored("uia_worker_models", layer_path);
        }
        return;
    }
    let Some(raw_table) = raw.get("uia_worker_models") else {
        return;
    };
    for worker_role in crate::uia_worker_models::UIA_WORKER_ROLES {
        let Some(key) = UiaWorkerModelsToml::toml_key(worker_role) else {
            continue;
        };
        if raw_table.get(key).is_none() {
            incoming.set(
                worker_role,
                trusted
                    .uia_worker_models
                    .get(worker_role)
                    .map(str::to_owned),
            );
        }
    }
    trusted.uia_worker_models = incoming;
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
    // Keine Vorgabe (`None`): sie liegt in harw-core, und das Feld ist nur
    // eine Kostengrenze, keine Befugnis.
    merge_optional_min_bound(
        &mut trusted.compaction.absolute_ceiling_tokens,
        incoming.absolute_ceiling_tokens,
        field_present(raw, &["compaction", "absolute_ceiling_tokens"]),
        role,
        None,
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

// `[knowledge]` (Abschnitt 1.17) — `diary.retention_days` `ProfileReplaces`.
fn merge_knowledge(
    trusted: &mut HarnessConfig,
    incoming: KnowledgeToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    profile_replaces(
        &mut trusted.knowledge.diary.retention_days,
        incoming.diary.retention_days,
        field_present(raw, &["knowledge", "diary", "retention_days"]),
        role,
        "knowledge.diary.retention_days",
        layer_path,
    );
}

// `[dream]` (Abschnitt 1.18) — alle fünf Felder `ProfileReplaces`.
fn merge_dream(
    trusted: &mut HarnessConfig,
    incoming: DreamToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    profile_replaces(
        &mut trusted.dream.enabled,
        incoming.enabled,
        field_present(raw, &["dream", "enabled"]),
        role,
        "dream.enabled",
        layer_path,
    );
    profile_replaces(
        &mut trusted.dream.budget,
        incoming.budget,
        field_present(raw, &["dream", "budget"]),
        role,
        "dream.budget",
        layer_path,
    );
    profile_replaces(
        &mut trusted.dream.idle_minutes,
        incoming.idle_minutes,
        field_present(raw, &["dream", "idle_minutes"]),
        role,
        "dream.idle_minutes",
        layer_path,
    );
    profile_replaces(
        &mut trusted.dream.cooldown_minutes,
        incoming.cooldown_minutes,
        field_present(raw, &["dream", "cooldown_minutes"]),
        role,
        "dream.cooldown_minutes",
        layer_path,
    );
    profile_replaces(
        &mut trusted.dream.schedule,
        incoming.schedule,
        field_present(raw, &["dream", "schedule"]),
        role,
        "dream.schedule",
        layer_path,
    );
}

// `[host]` (Abschnitt 1.19, Runde 5 Teil B) — `sudo_session_minutes` ist
// `MinBound` (global): nur der Home-Layer setzt frei; jeder spaetere Layer
// (Profil wie nicht vertrautes Projekt) kann die Merkfrist nur verkuerzen.
// Ein ungesetzter Home-Wert gilt als Vorgabe (`DEFAULT_SUDO_SESSION_MINUTES`).
// Anders als bei `merge_optional_min_bound` bindet diese Vorgabe hier nicht
// nur das nicht vertraute Projekt, sondern auch ein vertrautes Profil
// (`Refinement`) — ein spaeterer Layer kann die Frist also auch dann nicht
// ueber die Vorgabe hinaus verlaengern (fail-closed fuer ein Geheimnis im
// Speicher). `0` (kein Sitzungs-Merken) ist als Verkuerzung immer erlaubt.
fn merge_host(
    trusted: &mut HarnessConfig,
    incoming: HostToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    const FIELD: &str = "host.sudo_session_minutes";
    if !field_present(raw, &["host", "sudo_session_minutes"]) {
        return;
    }
    let Some(value) = incoming.sudo_session_minutes else {
        return;
    };
    if role == LayerRole::Baseline {
        trusted.host.sudo_session_minutes = Some(value);
        return;
    }
    let current = trusted
        .host
        .sudo_session_minutes
        .unwrap_or(crate::harness_config::DEFAULT_SUDO_SESSION_MINUTES);
    if value <= current {
        trusted.host.sudo_session_minutes = Some(value);
    } else {
        let diagnostic = ScopeDiagnostic::new(FIELD, layer_path, &value);
        reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
    }
}

// `[agents]` (Abschnitt 1.21, Runde 5 Teil K) — vier Orchestrierungsgrenzen.
// Vertraute Layer (Home und Profil) setzen frei, auch nach oben; ein nicht
// vertrautes Projekt darf jede Zahl nur senken. Vergleichswert ist der bisher
// gesetzte Wert, sonst die Vorgabe — ein Projekt kommt also auch ohne
// Home-Wert nie ueber die Vorgabe hinaus. Das Klemmen auf den erlaubten
// Bereich geschieht erst beim Lesen (`AgentLimitsToml::effective`).
fn merge_agent_limits(
    trusted: &mut HarnessConfig,
    incoming: crate::agent_limits::AgentLimitsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    use crate::agent_limits::{
        DEFAULT_MAX_ROOT_ORCHESTRATORS, DEFAULT_MAX_SPAWN_DEPTH,
        DEFAULT_MAX_SUB_ORCHESTRATOR_DEPTH, DEFAULT_MAX_SUB_ORCHESTRATORS,
    };
    let limits = &mut trusted.agents;
    let fields: [(&str, Option<u32>, &mut Option<u32>, u32); 4] = [
        (
            "max_root_orchestrators",
            incoming.max_root_orchestrators,
            &mut limits.max_root_orchestrators,
            DEFAULT_MAX_ROOT_ORCHESTRATORS,
        ),
        (
            "max_sub_orchestrators",
            incoming.max_sub_orchestrators,
            &mut limits.max_sub_orchestrators,
            DEFAULT_MAX_SUB_ORCHESTRATORS,
        ),
        (
            "max_sub_orchestrator_depth",
            incoming.max_sub_orchestrator_depth,
            &mut limits.max_sub_orchestrator_depth,
            DEFAULT_MAX_SUB_ORCHESTRATOR_DEPTH,
        ),
        (
            "max_spawn_depth",
            incoming.max_spawn_depth,
            &mut limits.max_spawn_depth,
            DEFAULT_MAX_SPAWN_DEPTH,
        ),
    ];
    for (name, value, slot, default) in fields {
        if !field_present(raw, &["agents", name]) {
            continue;
        }
        let Some(value) = value else {
            continue;
        };
        if role != LayerRole::UntrustedProject || value <= slot.unwrap_or(default) {
            *slot = Some(value);
        } else {
            let diagnostic = ScopeDiagnostic::new(&format!("agents.{name}"), layer_path, &value);
            reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
        }
    }
}

// `[shell]` (Abschnitt 1.22, Runde 5 Teil N) — `max_timeout_secs`.
// Vertraute Layer (Home und Profil) setzen frei, auch nach oben; ein nicht
// vertrautes Projekt darf die Obergrenze nur senken (Vergleichswert: der
// bisher gesetzte Wert, sonst die Vorgabe 900 s). Geklemmt wird erst beim
// Lesen (`ShellToml::effective_max_timeout_secs`).
fn merge_shell_limits(
    trusted: &mut HarnessConfig,
    incoming: crate::shell_limits::ShellToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    const FIELD: &str = "shell.max_timeout_secs";
    if !field_present(raw, &["shell", "max_timeout_secs"]) {
        return;
    }
    let Some(value) = incoming.max_timeout_secs else {
        return;
    };
    let current = trusted
        .shell
        .max_timeout_secs
        .unwrap_or(crate::shell_limits::DEFAULT_SHELL_MAX_TIMEOUT_SECS);
    if role != LayerRole::UntrustedProject || value <= current {
        trusted.shell.max_timeout_secs = Some(value);
    } else {
        let diagnostic = ScopeDiagnostic::new(FIELD, layer_path, &value);
        reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
    }
}

// `[jobs]` — `max_running`. Wie `merge_shell_limits`: vertraute Layer
// (Home und Profil) setzen frei, auch nach oben; ein nicht vertrautes
// Projekt darf die Obergrenze nur senken (Vergleichswert: der bisher
// gesetzte Wert, sonst die Vorgabe 16). Der Bereich 1–256 ist schon beim
// Parsen geprüft (`harness_config::JobsToml`).
fn merge_jobs(
    trusted: &mut HarnessConfig,
    incoming: crate::harness_config::JobsToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    const FIELD: &str = "jobs.max_running";
    if !field_present(raw, &["jobs", "max_running"]) {
        return;
    }
    let Some(value) = incoming.max_running else {
        return;
    };
    let current = trusted
        .jobs
        .max_running
        .unwrap_or(crate::harness_config::DEFAULT_JOBS_MAX_RUNNING);
    if role != LayerRole::UntrustedProject || value <= current {
        trusted.jobs.max_running = Some(value);
    } else {
        let diagnostic = ScopeDiagnostic::new(FIELD, layer_path, &value);
        reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
    }
}

// `[retention.<klasse>]` — je Klasse (aus `harw_retention::CLASSES`):
// `enabled` und `keep_newest` sind `ProfileReplaces` (ein nicht vertrautes
// Projekt kann also weder eine sicherheitsrelevante Löschung einschalten
// noch Schutzdateien reduzieren); `max_age_secs`/`max_bytes`/`max_files`
// sind `MinBound`: vertraute Layer (Home, Profil) setzen frei, ein nicht
// vertrautes Projekt darf nur senken (Vergleichswert: der bisher gesetzte
// Wert, sonst die Klassenvorgabe; `None` = unbegrenzt, jeder Wert senkt).
fn merge_retention(
    trusted: &mut HarnessConfig,
    incoming: RetentionSection,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
    out: &mut Vec<ScopeDiagnostic>,
) {
    for class in harw_retention::CLASSES {
        let (Some(inc), Some(cur)) = (
            incoming.class_config(class.id),
            trusted.retention.class_config_mut(class.id),
        ) else {
            continue;
        };
        let present = |key: &str| field_present(raw, &["retention", class.id, key]);
        profile_replaces(
            &mut cur.enabled,
            inc.enabled,
            present("enabled"),
            role,
            &format!("retention.{}.enabled", class.id),
            layer_path,
        );
        profile_replaces(
            &mut cur.keep_newest,
            inc.keep_newest,
            present("keep_newest"),
            role,
            &format!("retention.{}.keep_newest", class.id),
            layer_path,
        );
        let limits = [
            (
                "max_age_secs",
                &mut cur.max_age_secs,
                inc.max_age_secs,
                class.defaults.max_age_secs,
            ),
            (
                "max_bytes",
                &mut cur.max_bytes,
                inc.max_bytes,
                class.defaults.max_bytes,
            ),
            (
                "max_files",
                &mut cur.max_files,
                inc.max_files,
                class.defaults.max_files,
            ),
        ];
        for (key, slot, value, default) in limits {
            let Some(value) = value else { continue };
            if !present(key) {
                continue;
            }
            let current = (*slot).or(default);
            if role != LayerRole::UntrustedProject || current.is_none_or(|c| value <= c) {
                *slot = Some(value);
            } else {
                let field = format!("retention.{}.{key}", class.id);
                let diagnostic = ScopeDiagnostic::new(&field, layer_path, &value);
                reject(out, diagnostic, role, RejectionReason::ExceedsMinBound);
            }
        }
    }
}

// `[agent_compiler]` (#22 Welle 2B) — alle drei Felder `ProfileReplaces`.
fn merge_agent_compiler(
    trusted: &mut HarnessConfig,
    incoming: crate::harness_config::AgentCompilerToml,
    raw: &toml::Value,
    role: LayerRole,
    layer_path: &Path,
) {
    let present = |field: &str| field_present(raw, &["agent_compiler", field]);
    profile_replaces(
        &mut trusted.agent_compiler.cache_max_bytes,
        incoming.cache_max_bytes,
        present("cache_max_bytes"),
        role,
        "agent_compiler.cache_max_bytes",
        layer_path,
    );
    profile_replaces(
        &mut trusted.agent_compiler.keep_versions,
        incoming.keep_versions,
        present("keep_versions"),
        role,
        "agent_compiler.keep_versions",
        layer_path,
    );
    profile_replaces(
        &mut trusted.agent_compiler.auto_build_uia,
        incoming.auto_build_uia,
        present("auto_build_uia"),
        role,
        "agent_compiler.auto_build_uia",
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

// `[guards]` (Abschnitt 1.16) — `enabled` `OrBool`, die uebrigen `MinBound`,
// alle als `Option<T>` (Laufzeit-Default liegt beim Consumer). Ein nicht
// vertrautes Projekt ohne vertrauten Wert wird gegen die gespiegelte
// Laufzeit-Vorgabe (`UNTRUSTED_BASELINE_*`) verglichen.
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
        UNTRUSTED_BASELINE_GUARDS_ENABLED,
        "guards.enabled",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.repeated_failure_warn,
        incoming.repeated_failure_warn,
        present("repeated_failure_warn"),
        role,
        Some(UNTRUSTED_BASELINE_REPEATED_FAILURE_WARN),
        "guards.repeated_failure_warn",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.repeated_failure_abort,
        incoming.repeated_failure_abort,
        present("repeated_failure_abort"),
        role,
        Some(UNTRUSTED_BASELINE_REPEATED_FAILURE_ABORT),
        "guards.repeated_failure_abort",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.no_progress_rounds_warn,
        incoming.no_progress_rounds_warn,
        present("no_progress_rounds_warn"),
        role,
        Some(UNTRUSTED_BASELINE_NO_PROGRESS_ROUNDS_WARN),
        "guards.no_progress_rounds_warn",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.no_progress_rounds_abort,
        incoming.no_progress_rounds_abort,
        present("no_progress_rounds_abort"),
        role,
        Some(UNTRUSTED_BASELINE_NO_PROGRESS_ROUNDS_ABORT),
        "guards.no_progress_rounds_abort",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.plan_stale_rounds,
        incoming.plan_stale_rounds,
        present("plan_stale_rounds"),
        role,
        Some(UNTRUSTED_BASELINE_PLAN_STALE_ROUNDS),
        "guards.plan_stale_rounds",
        layer_path,
        out,
    );
    // Runde 7, Teil A2.
    merge_optional_min_bound(
        &mut trusted.guards.orchestrator_read_warn,
        incoming.orchestrator_read_warn,
        present("orchestrator_read_warn"),
        role,
        Some(UNTRUSTED_BASELINE_ORCHESTRATOR_READ_WARN),
        "guards.orchestrator_read_warn",
        layer_path,
        out,
    );
    merge_optional_min_bound(
        &mut trusted.guards.orchestrator_read_limit,
        incoming.orchestrator_read_limit,
        present("orchestrator_read_limit"),
        role,
        Some(UNTRUSTED_BASELINE_ORCHESTRATOR_READ_LIMIT),
        "guards.orchestrator_read_limit",
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
/// Crate-intern; von außen ist `merge_layer_toml_into` der Einstieg.
pub(crate) fn merge_layer_into(
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
    merge_tools_doc(trusted, incoming.tools.doc, raw, role, layer_path, &mut out);
    merge_tools_container(
        trusted,
        incoming.tools.container.clone(),
        raw,
        role,
        layer_path,
        &mut out,
    );
    merge_tools_plan(trusted, incoming.tools, raw, role, layer_path, &mut out);
    merge_mode(trusted, incoming.mode, raw, role, layer_path);
    merge_research(trusted, incoming.research, raw, role, layer_path, &mut out);
    merge_memory(trusted, incoming.memory, raw, role, layer_path);
    merge_session_listener(
        trusted,
        incoming.session_listener.clone(),
        raw,
        role,
        layer_path,
        &mut out,
    );
    merge_retention(trusted, incoming.retention, raw, role, layer_path, &mut out);
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
    // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
    merge_uia_worker_models(trusted, incoming.uia_worker_models, raw, role, layer_path);
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
    merge_knowledge(trusted, incoming.knowledge, raw, role, layer_path);
    merge_dream(trusted, incoming.dream, raw, role, layer_path);
    merge_host(trusted, incoming.host, raw, role, layer_path, &mut out);
    // Runde 5, Teil K: `[agents]` — Orchestrierungsgrenzen.
    merge_agent_limits(trusted, incoming.agents, raw, role, layer_path, &mut out);
    // Runde 5, Teil N: `[shell]` — Obergrenze für `shell.exec`-Zeitlimits.
    merge_shell_limits(trusted, incoming.shell, raw, role, layer_path, &mut out);
    // `[jobs]` — Höchstzahl laufender Hintergrund-Jobs.
    merge_jobs(trusted, incoming.jobs, raw, role, layer_path, &mut out);
    // #22 Welle 2B: `[agent_compiler]` — Build-Cache und Versionen.
    merge_agent_compiler(trusted, incoming.agent_compiler, raw, role, layer_path);

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

/// Test-/Tooling-Einstieg in die Merge-Engine ohne Fremdtyp in der
/// Signatur: wie `merge_layer_into`, nimmt das rohe TOML dieses Layers aber
/// als `&str` statt als `toml::Value`.
///
/// # Description
/// Parst `raw_toml` selbst und nutzt das Ergebnis ausschließlich für die
/// Präsenzprüfungen je Feld. `incoming` wird so übernommen, wie es
/// übergeben wird (es wird nicht aus `raw_toml` neu deserialisiert); der
/// Aufrufer ist dafür verantwortlich, dass beide zum selben Layer gehören.
///
/// # Arguments
/// - `trusted` (`&mut HarnessConfig`): der bisher akkumulierte Stand.
/// - `incoming` (`HarnessConfig`): die deserialisierte `HarnessConfig`
///   dieses Layers.
/// - `raw_toml` (`&str`): der rohe TOML-Text dieses Layers.
/// - `role` (`LayerRole`): Baseline/Refinement/UntrustedProject.
/// - `layer_path` (`&Path`): Pfad der `config.toml` dieses Layers, für
///   `ScopeDiagnostic::file`.
///
/// # Returns
/// Jede `ScopeDiagnostic` eines abgelehnten Lockerungsversuchs, wie
/// `merge_layer_into`.
///
/// # Errors
/// `ConfigError::TomlParse`, wenn `raw_toml` kein gültiges TOML ist; `trusted`
/// bleibt dann unverändert.
///
/// # Examples
/// ```rust
/// use harw_config::{merge_layer_toml_into, HarnessConfig, LayerRole};
/// use std::path::Path;
///
/// let mut trusted = HarnessConfig::default();
/// let diagnostics = merge_layer_toml_into(
///     &mut trusted,
///     HarnessConfig::default(),
///     "",
///     LayerRole::Baseline,
///     Path::new("/home/user/.harw/config.toml"),
/// )?;
/// assert!(diagnostics.is_empty());
/// # Ok::<(), harw_config::ConfigError>(())
/// ```
#[doc(hidden)]
pub fn merge_layer_toml_into(
    trusted: &mut HarnessConfig,
    incoming: HarnessConfig,
    raw_toml: &str,
    role: LayerRole,
    layer_path: &Path,
) -> ConfigResult<Vec<ScopeDiagnostic>> {
    let raw: toml::Value =
        toml::from_str(raw_toml).map_err(|e| ConfigError::TomlParse(e.to_string()))?;
    Ok(merge_layer_into(trusted, incoming, &raw, role, layer_path))
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

    // knowledge.diary.retention_days (Abschnitt 1.17): ProfileReplaces —
    // ein vertrautes Profil ersetzt, ein nicht vertrautes Projekt nicht.
    #[test]
    fn test_diary_retention_days_is_profile_replaces() -> TestResult {
        let src = "[knowledge.diary]\nretention_days = 30";
        let incoming: HarnessConfig = toml::from_str(src).map_err(ctx("parse layer"))?;
        let raw = raw_from(src)?;
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.knowledge.diary.effective_retention_days(), 90);
        merge_layer_into(
            &mut trusted,
            incoming.clone(),
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.knowledge.diary.retention_days, Some(30));

        let mut untouched = HarnessConfig::default();
        merge_layer_into(
            &mut untouched,
            incoming,
            &raw,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(untouched.knowledge.diary.retention_days, None);
        Ok(())
    }

    // [dream] (Abschnitt 1.18): ProfileReplaces — ein vertrautes Profil
    // ersetzt, ein nicht vertrautes Projekt nicht.
    #[test]
    fn test_dream_section_is_profile_replaces() -> TestResult {
        let src = "[dream]\nenabled = false\nbudget = 4000\nidle_minutes = 5\n\
                   cooldown_minutes = 30\nschedule = \"0 3 * * 1\"";
        let incoming: HarnessConfig = toml::from_str(src).map_err(ctx("parse layer"))?;
        let raw = raw_from(src)?;
        let mut trusted = HarnessConfig::default();
        assert!(trusted.dream.effective_enabled());
        assert_eq!(trusted.dream.effective_budget(), 16_384);
        assert_eq!(trusted.dream.effective_schedule(), None);
        merge_layer_into(
            &mut trusted,
            incoming.clone(),
            &raw,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert!(!trusted.dream.effective_enabled());
        assert_eq!(trusted.dream.effective_budget(), 4000);
        assert_eq!(trusted.dream.effective_idle_minutes(), 5);
        assert_eq!(trusted.dream.effective_cooldown_minutes(), 30);
        assert_eq!(trusted.dream.effective_schedule(), Some("0 3 * * 1"));

        let mut untouched = HarnessConfig::default();
        merge_layer_into(
            &mut untouched,
            incoming,
            &raw,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(untouched.dream, DreamToml::default());
        Ok(())
    }

    // [host] sudo_session_minutes (Abschnitt 1.19, Runde 5 Teil B):
    // MinBound — ein spaeterer Layer kann die Merkfrist nur verkuerzen.
    #[test]
    fn test_host_sudo_session_minutes_is_min_bound() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.host.effective_sudo_session_minutes(), 10);
        let home = "[host]\nsudo_session_minutes = 5";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.host.sudo_session_minutes, Some(5));

        let longer = "[host]\nsudo_session_minutes = 30";
        merge_layer_into(
            &mut trusted,
            toml::from_str(longer).map_err(ctx("parse longer"))?,
            &raw_from(longer)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(
            trusted.host.sudo_session_minutes,
            Some(5),
            "ein Profil darf die Frist nicht verlaengern"
        );

        let shorter = "[host]\nsudo_session_minutes = 2";
        merge_layer_into(
            &mut trusted,
            toml::from_str(shorter).map_err(ctx("parse shorter"))?,
            &raw_from(shorter)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.host.sudo_session_minutes, Some(2));

        // Ohne Home-Wert gilt die Vorgabe (10) als Obergrenze: weder ein
        // Profil noch ein nicht vertrautes Projekt verlaengern sie.
        let mut fresh = HarnessConfig::default();
        for role in [LayerRole::Refinement, LayerRole::UntrustedProject] {
            let diagnostics = merge_layer_into(
                &mut fresh,
                toml::from_str(longer).map_err(ctx("parse longer"))?,
                &raw_from(longer)?,
                role,
                &layer_path(),
            );
            assert_eq!(fresh.host.sudo_session_minutes, None, "{role:?}");
            assert!(!diagnostics.is_empty(), "{role:?}: Ablehnung sichtbar");
        }

        let capped = HostToml {
            sudo_session_minutes: Some(10_000),
        };
        assert_eq!(capped.effective_sudo_session_minutes(), 60);
        Ok(())
    }

    // [agents] (Abschnitt 1.21, Runde 5 Teil K): Home und Profil setzen
    // frei (auch nach oben), ein nicht vertrautes Projekt senkt nur.
    #[test]
    fn test_agent_limits_trusted_layers_raise_untrusted_project_only_lowers() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.agents.effective().max_root_orchestrators, 1);

        let home = "[agents]\nmax_root_orchestrators = 2\nmax_spawn_depth = 5";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.agents.max_root_orchestrators, Some(2));
        assert_eq!(trusted.agents.max_spawn_depth, Some(5));

        // Ein Profil darf erhöhen.
        let profile = "[agents]\nmax_root_orchestrators = 3\nmax_sub_orchestrators = 4";
        merge_layer_into(
            &mut trusted,
            toml::from_str(profile).map_err(ctx("parse profile"))?,
            &raw_from(profile)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.agents.max_root_orchestrators, Some(3));
        assert_eq!(trusted.agents.max_sub_orchestrators, Some(4));

        // Ein nicht vertrautes Projekt kann nicht erhöhen …
        let raise = "[agents]\nmax_root_orchestrators = 4\nmax_spawn_depth = 6";
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(raise).map_err(ctx("parse raise"))?,
            &raw_from(raise)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.agents.max_root_orchestrators, Some(3));
        assert_eq!(trusted.agents.max_spawn_depth, Some(5));
        assert_eq!(diagnostics.len(), 2, "beide Erhöhungen sichtbar abgelehnt");

        // … aber senken.
        let lower = "[agents]\nmax_root_orchestrators = 1\nmax_sub_orchestrator_depth = 1";
        merge_layer_into(
            &mut trusted,
            toml::from_str(lower).map_err(ctx("parse lower"))?,
            &raw_from(lower)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.agents.max_root_orchestrators, Some(1));
        assert_eq!(trusted.agents.max_sub_orchestrator_depth, Some(1));

        // Ohne Home-Wert gilt die Vorgabe als Obergrenze für das Projekt.
        let mut fresh = HarnessConfig::default();
        let over_default = "[agents]\nmax_root_orchestrators = 2";
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(over_default).map_err(ctx("parse over default"))?,
            &raw_from(over_default)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.agents.max_root_orchestrators, None);
        assert!(!diagnostics.is_empty());
        Ok(())
    }

    // [tools.container]: nur die Baseline (Home) setzt; Profil und Projekt
    // können weder aktivieren noch Images/Engine ändern.
    #[test]
    fn test_tools_container_is_global_only() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert!(!trusted.tools.container.enabled);
        let home = "[tools.container]\nenabled = true\nimages = [\"a=docker.io/x@sha256:00\"]";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert!(trusted.tools.container.enabled);
        assert_eq!(trusted.tools.container.images.len(), 1);

        for role in [LayerRole::Refinement, LayerRole::UntrustedProject] {
            let attack =
                "[tools.container]\nengine = \"/tmp/podman\"\nimages = [\"b=evil@sha256:11\"]";
            merge_layer_into(
                &mut trusted,
                toml::from_str(attack).map_err(ctx("parse attack"))?,
                &raw_from(attack)?,
                role,
                &layer_path(),
            );
            assert_eq!(
                trusted.tools.container.engine, "/usr/bin/podman",
                "{role:?}"
            );
            assert_eq!(trusted.tools.container.images.len(), 1, "{role:?}");
        }
        Ok(())
    }

    // [session_listener]: nur die Baseline (Home) setzt; Profil und Projekt
    // können weder aktivieren noch Adresse, Identität oder Tier ändern.
    #[test]
    fn test_session_listener_is_global_only() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert!(!trusted.session_listener.enabled);
        let home = "[session_listener]\nenabled = true\nnode_id = \"gw\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert!(trusted.session_listener.enabled);
        for role in [LayerRole::Refinement, LayerRole::UntrustedProject] {
            let attack =
                "[session_listener]\nlisten = \"0.0.0.0:1\"\ntier = \"owner\"\nnode_id = \"evil\"";
            merge_layer_into(
                &mut trusted,
                toml::from_str(attack).map_err(ctx("parse attack"))?,
                &raw_from(attack)?,
                role,
                &layer_path(),
            );
            assert_eq!(
                trusted.session_listener.listen, "127.0.0.1:7443",
                "{role:?}"
            );
            assert_eq!(trusted.session_listener.tier, "observer", "{role:?}");
            assert_eq!(
                trusted.session_listener.node_id.as_deref(),
                Some("gw"),
                "{role:?}"
            );
        }
        Ok(())
    }

    // [tools.doc] remote_ocr: Home/Profil setzen frei, ein nicht vertrautes
    // Projekt verschärft nur (`off` < `ask` < `on`).
    #[test]
    fn test_tools_doc_remote_ocr_untrusted_project_only_tightens() -> TestResult {
        use crate::plan_toml::RemoteOcrMode;

        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.tools.doc.remote_ocr, RemoteOcrMode::Off);

        let home = "[tools.doc]\nremote_ocr = \"ask\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.tools.doc.remote_ocr, RemoteOcrMode::Ask);

        // Ein vertrautes Profil darf lockern.
        let profile = "[tools.doc]\nremote_ocr = \"on\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(profile).map_err(ctx("parse profile"))?,
            &raw_from(profile)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.tools.doc.remote_ocr, RemoteOcrMode::On);

        // Ein Layer ohne `[tools.doc]` lässt den Wert stehen.
        let unrelated = "[shell]\nmax_timeout_secs = 60";
        merge_layer_into(
            &mut trusted,
            toml::from_str(unrelated).map_err(ctx("parse unrelated"))?,
            &raw_from(unrelated)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.tools.doc.remote_ocr, RemoteOcrMode::On);

        let tighten = "[tools.doc]\nremote_ocr = \"ask\"";
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(tighten).map_err(ctx("parse tighten"))?,
            &raw_from(tighten)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.tools.doc.remote_ocr, RemoteOcrMode::Ask);
        assert!(diagnostics.is_empty());

        // Default `off`: das Projekt darf weder auf `ask` noch auf `on`
        // lockern.
        for loosen in [
            "[tools.doc]\nremote_ocr = \"ask\"",
            "[tools.doc]\nremote_ocr = \"on\"",
        ] {
            let mut fresh = HarnessConfig::default();
            let diagnostics = merge_layer_into(
                &mut fresh,
                toml::from_str(loosen).map_err(ctx("parse loosen"))?,
                &raw_from(loosen)?,
                LayerRole::UntrustedProject,
                &layer_path(),
            );
            assert_eq!(fresh.tools.doc.remote_ocr, RemoteOcrMode::Off);
            assert_eq!(diagnostics.len(), 1, "Lockerung sichtbar abgelehnt");
            assert_eq!(diagnostics[0].field, "tools.doc.remote_ocr");
        }
        Ok(())
    }

    // [shell] max_timeout_secs (Runde 5, Teil N): Home/Profil erhöhen,
    // ein nicht vertrautes Projekt senkt nur.
    #[test]
    fn test_shell_max_timeout_trusted_layers_raise_untrusted_project_only_lowers() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.shell.effective_max_timeout_secs(), 900);

        let home = "[shell]\nmax_timeout_secs = 1800";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.shell.max_timeout_secs, Some(1800));

        let profile = "[shell]\nmax_timeout_secs = 3600";
        merge_layer_into(
            &mut trusted,
            toml::from_str(profile).map_err(ctx("parse profile"))?,
            &raw_from(profile)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.shell.max_timeout_secs, Some(3600));

        let raise = "[shell]\nmax_timeout_secs = 3601";
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(raise).map_err(ctx("parse raise"))?,
            &raw_from(raise)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.shell.max_timeout_secs, Some(3600));
        assert_eq!(diagnostics.len(), 1, "Erhöhung sichtbar abgelehnt");

        let lower = "[shell]\nmax_timeout_secs = 120";
        merge_layer_into(
            &mut trusted,
            toml::from_str(lower).map_err(ctx("parse lower"))?,
            &raw_from(lower)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.shell.effective_max_timeout_secs(), 120);

        // Ohne Home-Wert ist die Vorgabe 900 die Obergrenze für das Projekt.
        let mut fresh = HarnessConfig::default();
        let over_default = "[shell]\nmax_timeout_secs = 1200";
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(over_default).map_err(ctx("parse over default"))?,
            &raw_from(over_default)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.shell.max_timeout_secs, None);
        assert!(!diagnostics.is_empty());
        Ok(())
    }

    #[test]
    fn test_retention_untrusted_project_can_only_tighten_and_never_opt_in() -> TestResult {
        let mut trusted = HarnessConfig::default();
        let home = "[retention.tui_log]\nmax_files = 10\n[retention.dod_spool]\nenabled = true";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.retention.tui_log.max_files, Some(10));
        assert_eq!(trusted.retention.dod_spool.enabled, Some(true));

        // Projekt: lockern (hoeher), Opt-in einer anderen Klasse, `enabled =
        // false` und `keep_newest` werden verworfen; Senken wirkt.
        let project = "[retention.tui_log]\nmax_files = 50\nmax_age_secs = 60\nkeep_newest = 0\n\
                       [retention.dod_spool]\nenabled = false\n\
                       [retention.freeze_resolved]\nenabled = true\nmax_files = 5";
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(project).map_err(ctx("parse project"))?,
            &raw_from(project)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.retention.tui_log.max_files, Some(10));
        // Vorgabe 14 Tage; 60 s ist niedriger und damit erlaubt.
        assert_eq!(trusted.retention.tui_log.max_age_secs, Some(60));
        assert_eq!(trusted.retention.tui_log.keep_newest, None);
        assert_eq!(trusted.retention.dod_spool.enabled, Some(true));
        assert_eq!(trusted.retention.freeze_resolved.enabled, None);
        // Unbegrenzte Vorgabe (`None`): jeder Wert senkt.
        let fields: Vec<&str> = diagnostics.iter().map(|d| d.field.as_str()).collect();
        assert!(
            fields.contains(&"retention.tui_log.max_files"),
            "{fields:?}"
        );

        // Ein vertrauter Profil-Layer darf auch lockern.
        let profile = "[retention.tui_log]\nmax_files = 99";
        merge_layer_into(
            &mut trusted,
            toml::from_str(profile).map_err(ctx("parse profile"))?,
            &raw_from(profile)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.retention.tui_log.max_files, Some(99));
        Ok(())
    }

    #[test]
    fn test_memory_trusted_layers_replace_untrusted_project_is_ignored() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert!(trusted.memory.global_enabled);
        assert_eq!(trusted.memory.max_facts, None);

        let home = "[memory]\nglobal_enabled = false\nmax_facts = 100";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert!(!trusted.memory.global_enabled);
        assert_eq!(trusted.memory.max_facts, Some(100));

        // Ein nicht vertrauter Projekt-Layer darf nichts davon ändern.
        let project = "[memory]\nglobal_enabled = true\nmax_facts = 5000\ntoken_budget = 9";
        merge_layer_into(
            &mut trusted,
            toml::from_str(project).map_err(ctx("parse project"))?,
            &raw_from(project)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert!(!trusted.memory.global_enabled);
        assert_eq!(trusted.memory.max_facts, Some(100));
        assert_eq!(trusted.memory.token_budget, None);

        // Ein Layer ohne `[memory]` lässt den Stand unberührt.
        let other = "[jobs]\nmax_running = 8";
        merge_layer_into(
            &mut trusted,
            toml::from_str(other).map_err(ctx("parse other"))?,
            &raw_from(other)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.memory.max_facts, Some(100));
        Ok(())
    }

    #[test]
    fn test_jobs_max_running_trusted_layers_raise_untrusted_project_only_lowers() -> TestResult {
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.jobs.effective_max_running(), 16);

        let home = "[jobs]\nmax_running = 8";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.jobs.max_running, Some(8));

        let raise = "[jobs]\nmax_running = 32";
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(raise).map_err(ctx("parse raise"))?,
            &raw_from(raise)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.jobs.max_running, Some(8));
        assert_eq!(diagnostics.len(), 1, "Erhöhung sichtbar abgelehnt");
        assert_eq!(diagnostics[0].field, "jobs.max_running");
        assert_eq!(trusted.jobs.effective_max_running(), 8);

        let lower = "[jobs]\nmax_running = 4";
        merge_layer_into(
            &mut trusted,
            toml::from_str(lower).map_err(ctx("parse lower"))?,
            &raw_from(lower)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.jobs.max_running, Some(4));

        // Vertraute Layer (hier: Profil-Verfeinerung) dürfen weiterhin erhöhen.
        let mut fresh = HarnessConfig::default();
        let refine = "[jobs]\nmax_running = 64";
        merge_layer_into(
            &mut fresh,
            toml::from_str(refine).map_err(ctx("parse refine"))?,
            &raw_from(refine)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(fresh.jobs.max_running, Some(64));
        Ok(())
    }

    #[test]
    fn test_jobs_max_running_untrusted_project_cannot_exceed_default() -> TestResult {
        let mut fresh = HarnessConfig::default();
        let over_default = "[jobs]\nmax_running = 17";
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(over_default).map_err(ctx("parse over default"))?,
            &raw_from(over_default)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.jobs.max_running, None);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(fresh.jobs.effective_max_running(), 16);

        let at_default = "[jobs]\nmax_running = 16";
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(at_default).map_err(ctx("parse at default"))?,
            &raw_from(at_default)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.jobs.max_running, Some(16));
        assert!(diagnostics.is_empty());
        Ok(())
    }

    #[test]
    fn test_jobs_absent_layer_keeps_trusted_value() -> TestResult {
        let mut trusted = HarnessConfig::default();
        let home = "[jobs]\nmax_running = 8";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.jobs.max_running, Some(8));

        let without_jobs = "[shell]\nmax_timeout_secs = 120";
        merge_layer_into(
            &mut trusted,
            toml::from_str(without_jobs).map_err(ctx("parse without jobs"))?,
            &raw_from(without_jobs)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.jobs.max_running, Some(8));
        Ok(())
    }

    // [tui] child_stream (Runde 5, Teil I): ProfileReplaces; Vorgabe
    // `orchestrators`, ein nicht vertrautes Projekt wird ignoriert, ein
    // unbekannter Wert parst nicht.
    #[test]
    fn test_tui_child_stream_profile_replaces() -> TestResult {
        use crate::ChildStreamModeToml;
        let mut trusted = HarnessConfig::default();
        assert_eq!(trusted.tui.child_stream, ChildStreamModeToml::Orchestrators);

        let all = "[tui]\nchild_stream = \"all\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(all).map_err(ctx("parse all"))?,
            &raw_from(all)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        assert_eq!(trusted.tui.child_stream, ChildStreamModeToml::All);

        let none = "[tui]\nchild_stream = \"none\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(none).map_err(ctx("parse none"))?,
            &raw_from(none)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(
            trusted.tui.child_stream,
            ChildStreamModeToml::All,
            "nicht vertrautes Projekt bleibt wirkungslos"
        );
        merge_layer_into(
            &mut trusted,
            toml::from_str(none).map_err(ctx("parse none"))?,
            &raw_from(none)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.tui.child_stream, ChildStreamModeToml::Off);
        assert_eq!(ChildStreamModeToml::Off.as_str(), "none");

        let bogus = "[tui]\nchild_stream = \"workers\"";
        assert!(toml::from_str::<HarnessConfig>(bogus).is_err());
        Ok(())
    }

    // [uia_worker_models] (Runde 5, Teil G): ProfileReplaces je Rolle; ein
    // Profil erbt nicht gesetzte Rollen vom Home-Layer, ein nicht vertrautes
    // Projekt wird ignoriert.
    #[test]
    fn test_uia_worker_models_merge_per_role_and_ignore_untrusted_project() -> TestResult {
        let mut trusted = HarnessConfig::default();
        let home = "[uia_worker_models]\nuia_writer = \"openai/gpt-5\"\nuia_explorer = \"uia\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(home).map_err(ctx("parse home"))?,
            &raw_from(home)?,
            LayerRole::Baseline,
            &layer_path(),
        );
        let profile = "[uia_worker_models]\nuia_explorer = \"anthropic/claude-opus-5-5\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(profile).map_err(ctx("parse profile"))?,
            &raw_from(profile)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(
            trusted.uia_worker_models.get("uia-writer"),
            Some("openai/gpt-5")
        );
        assert_eq!(
            trusted.uia_worker_models.get("uia-explorer"),
            Some("anthropic/claude-opus-5-5")
        );

        let evil = "[uia_worker_models]\nuia_writer = \"evil/model\"";
        merge_layer_into(
            &mut trusted,
            toml::from_str(evil).map_err(ctx("parse evil"))?,
            &raw_from(evil)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(
            trusted.uia_worker_models.get("uia-writer"),
            Some("openai/gpt-5")
        );
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
            tenant: "alice".to_owned(),
            workspace: "harwness".to_owned(),
            job_capabilities: vec![],
        };
        let p2 = McpPrincipalToml {
            id: "p2".to_owned(),
            credential_ref: SecretRef::from_str("env:P2_TOKEN").map_err(ctx("env:P2_TOKEN"))?,
            tenant: "alice".to_owned(),
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
                tenant = "alice"
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

    // Befund M1: ohne vertrauten Wert wird ein nicht vertrautes Projekt
    // gegen die gespiegelte Laufzeit-Vorgabe verglichen — `full`, das
    // Abschalten der Waechter, der Sentinel `0` und eine hoehere Schwelle
    // werden abgelehnt und bleiben `None`.
    #[test]
    fn test_untrusted_project_cannot_loosen_unset_permissions_and_guards() -> TestResult {
        let src = "[permissions]\ndefault_mode = \"full\"\n[guards]\nenabled = false\n\
                   orchestrator_read_limit = 0\nrepeated_failure_abort = 999";
        let incoming: HarnessConfig = toml::from_str(src).map_err(ctx("parse layer"))?;
        let raw = raw_from(src)?;
        let mut trusted = HarnessConfig::default();
        let diagnostics = merge_layer_into(
            &mut trusted,
            incoming,
            &raw,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.permissions.default_mode, None);
        assert_eq!(trusted.guards.enabled, None);
        assert_eq!(trusted.guards.orchestrator_read_limit, None);
        assert_eq!(trusted.guards.repeated_failure_abort, None);
        assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
        for field in [
            "permissions.default_mode",
            "guards.enabled",
            "guards.orchestrator_read_limit",
            "guards.repeated_failure_abort",
        ] {
            assert!(
                diagnostics.iter().any(|d| d.field == field),
                "{field} fehlt in {diagnostics:?}"
            );
        }
        Ok(())
    }

    // Befund M1, Gegenprobe: gleich streng oder strenger als die Vorgabe
    // bleibt fuer das nicht vertraute Projekt erlaubt; ein Wert ausserhalb
    // der Ordnung (R1) nicht.
    #[test]
    fn test_untrusted_project_may_tighten_unset_permissions_and_guards() -> TestResult {
        for mode in ["ask", "auto"] {
            let src = format!("[permissions]\ndefault_mode = \"{mode}\"");
            let mut fresh = HarnessConfig::default();
            let diagnostics = merge_layer_into(
                &mut fresh,
                toml::from_str(&src).map_err(ctx("parse mode"))?,
                &raw_from(&src)?,
                LayerRole::UntrustedProject,
                &layer_path(),
            );
            assert_eq!(fresh.permissions.default_mode.as_deref(), Some(mode));
            assert!(
                !diagnostics
                    .iter()
                    .any(|d| d.field == "permissions.default_mode"),
                "{mode}: {diagnostics:?}"
            );
        }

        let guards = "[guards]\nenabled = true\nrepeated_failure_warn = 1\n\
                      repeated_failure_abort = 3\norchestrator_read_limit = 2";
        let mut fresh = HarnessConfig::default();
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(guards).map_err(ctx("parse guards"))?,
            &raw_from(guards)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.guards.enabled, Some(true));
        assert_eq!(fresh.guards.repeated_failure_warn, Some(1));
        assert_eq!(fresh.guards.repeated_failure_abort, Some(3));
        assert_eq!(fresh.guards.orchestrator_read_limit, Some(2));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let unordered = "[permissions]\ndefault_mode = \"yolo\"";
        let mut fresh = HarnessConfig::default();
        let diagnostics = merge_layer_into(
            &mut fresh,
            toml::from_str(unordered).map_err(ctx("parse unordered"))?,
            &raw_from(unordered)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(fresh.permissions.default_mode, None);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.field == "permissions.default_mode"),
            "{diagnostics:?}"
        );
        Ok(())
    }

    // Befund M1: vertraute Layer bleiben unveraendert — ein Profil setzt
    // einen ungesetzten Wert weiter frei, auch lockerer als die Vorgabe.
    #[test]
    fn test_trusted_refinement_still_sets_unset_permissions_and_guards_freely() -> TestResult {
        let src = "[permissions]\ndefault_mode = \"full\"\n[guards]\nenabled = false";
        let mut trusted = HarnessConfig::default();
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(src).map_err(ctx("parse layer"))?,
            &raw_from(src)?,
            LayerRole::Refinement,
            &layer_path(),
        );
        assert_eq!(trusted.permissions.default_mode.as_deref(), Some("full"));
        assert_eq!(trusted.guards.enabled, Some(false));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        Ok(())
    }

    // Befund M1: Felder ohne gespiegelte Vorgabe (`untrusted_baseline ==
    // None`) behalten das bisherige Verhalten — der erste Wert wird auch vom
    // nicht vertrauten Projekt uebernommen.
    #[test]
    fn test_untrusted_project_unset_timeouts_keep_previous_behaviour() -> TestResult {
        let src = "[permissions]\napproval_timeout_secs = 60";
        let mut trusted = HarnessConfig::default();
        let diagnostics = merge_layer_into(
            &mut trusted,
            toml::from_str(src).map_err(ctx("parse layer"))?,
            &raw_from(src)?,
            LayerRole::UntrustedProject,
            &layer_path(),
        );
        assert_eq!(trusted.permissions.approval_timeout_secs, Some(60));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        Ok(())
    }
}
