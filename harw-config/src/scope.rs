//! `SettingScope` — die drei Lebensdauern einer Einstellung (Contract
//! `docs/design/config-scopes.md` §2, Zeile A2).
//!
//! Eine Einstellung kann auf drei Ebenen mit steigender Präzedenz gelten:
//! `Global` (dauerhaft für den User, `~/.harw/config.toml`), `Project`
//! (dauerhaft pro Projekt, `~/.harw/profiles/<p>/projects/<key>/settings.toml`)
//! und `Session` (nur im Speicher, endet mit der Session). Bei einem
//! Modus-Konflikt gewinnt die Ebene mit der höchsten Präzedenz
//! (`Session > Project > Global`); bei Allow/Deny-Regeln gilt zusätzlich
//! „Deny gewinnt über alle Scopes hinweg“ — das ist Sache des Consumers
//! (`harw-extension-api`), nicht dieses Moduls.
//!
//! Dieses Modul kennt ausschließlich den Scope-Wert selbst: Ordnung nach
//! Präzedenz, textuelle Darstellung und `serde`-Kodierung. Es hat keine
//! Kenntnis von konkreten Speicherorten oder Regelinhalten.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Lebensdauer einer Einstellung, aufsteigend nach Präzedenz deklariert.
///
/// # Description
/// Die Deklarationsreihenfolge der Varianten (`Global`, `Project`,
/// `Session`) entspricht absichtlich der Präzedenzordnung: der
/// abgeleitete [`Ord`] sortiert Enum-Varianten nach Deklarationsposition,
/// wodurch `Global < Project < Session` ohne manuelle Implementierung gilt.
///
/// # Examples
/// ```rust
/// use harw_config::SettingScope;
///
/// assert!(SettingScope::Global < SettingScope::Project);
/// assert!(SettingScope::Project < SettingScope::Session);
/// assert_eq!(SettingScope::Session, SettingScope::Session.max(SettingScope::Global));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingScope {
    /// Dauerhaft für den User (`~/.harw/config.toml` bzw. aktives Profil).
    Global,
    /// Dauerhaft pro Projekt (autoritätsgewährend außerhalb des Repos).
    Project,
    /// Nur im Speicher, endet mit der Session.
    Session,
}

impl SettingScope {
    /// Gibt die kanonische Kleinschreib-Textform zurück (`"session"`,
    /// `"project"`, `"global"`).
    ///
    /// # Returns
    /// Ein statischer `&'static str`, identisch zur `serde`-Kodierung und zu
    /// [`FromStr`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SettingScope::Session => "session",
            SettingScope::Project => "project",
            SettingScope::Global => "global",
        }
    }
}

impl fmt::Display for SettingScope {
    /// Schreibt die Kleinschreib-Textform (siehe [`SettingScope::as_str`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SettingScope {
    type Err = ConfigError;

    /// Parst `"session"`, `"project"` oder `"global"` (exakte Kleinschreibung).
    ///
    /// # Errors
    /// - [`ConfigError::Invalid`]: `s` ist keiner der drei erlaubten Werte.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session" => Ok(SettingScope::Session),
            "project" => Ok(SettingScope::Project),
            "global" => Ok(SettingScope::Global),
            other => Err(ConfigError::Invalid(format!(
                "unbekannter SettingScope {other:?}, erwartet session|project|global"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// MergeRule-Taxonomie (docs/design/config-scopes.md, Abschnitt 6/7a/7b).
//
// Getrennt von `SettingScope` oberhalb: `SettingScope` ordnet die
// Lebensdauer einer *Laufzeit*-Einstellung (Global/Project/Session, siehe
// Moduldoku oben). Die folgenden Typen ordnen dagegen jedes einzelne
// `HarnessConfig`-TOML-Blattfeld einem Geltungsbereich (Home vs. aktives
// Profil) und einer Merge-Regel über die Config-Layer hinweg zu — ein
// unabhängiges, rein deklaratives Modell ohne Merge-Logik (die lebt in
// `crate::merge`).
// ---------------------------------------------------------------------------

/// Legt fest, wie ein einzelnes `HarnessConfig`-Blattfeld über die
/// vertrauten Layer (Home → aktives Profil) hinweg zusammengeführt wird,
/// und — mit denselben Varianten, aber eingeschränkter Anwendung
/// (`docs/design/config-scopes.md` Abschnitt 7c) — gegen einen nicht
/// vertrauten Projekt-Layer. Die tatsächliche Merge-Logik pro Variante lebt
/// in `crate::merge`; dieses Enum ist rein deklarativ.
///
/// # Examples
/// ```rust
/// use harw_config::{FIELD_TABLE, MergeRule};
///
/// let entry = FIELD_TABLE
///     .iter()
///     .find(|f| f.path == "mcp_listener.principals")
///     .expect("mcp_listener.principals is declared");
/// assert_eq!(entry.merge, MergeRule::Intersection);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRule {
    /// Der zuletzt **explizit gesetzte** Wert gewinnt. Ein Layer, der das
    /// Feld nicht setzt, lässt den Wert des vorigen Layers unverändert
    /// stehen (kein Reset auf den Section-Default — behebt die
    /// „ERSETZT"-Regression aus Abschnitt 4 der Spezifikation für alle so
    /// eingestuften Felder). Nie vom nicht vertrauten Projekt-Layer
    /// angewendet.
    ProfileReplaces,
    /// Nur der Wert des vertrauten Home-Layers gilt je. Ein späterer Layer
    /// (Profil **oder** nicht vertrautes Projekt), der einen *anderen* Wert
    /// setzt, wird ignoriert und löst eine `ScopeDiagnostic`-Warnung aus;
    /// denselben Wert erneut zu setzen ist ein stiller No-op.
    GlobalOnly,
    /// Vereinigung aller Layer, die das Feld setzen (einschränkende Listen:
    /// mehr Einträge sind immer sicher — z. B. mehr Freigabepflichten).
    Union,
    /// Schnittmenge aller Layer, die das Feld setzen, mit dem Home-Wert als
    /// Startmenge (rechte-erweiternde Listen: ein späterer Layer kann nur
    /// Einträge entfernen, niemals welche hinzufügen, die im Home-Layer
    /// fehlen). Der Vergleichsschlüssel für „ist derselbe Eintrag" ist
    /// standardmäßig die volle Wertgleichheit (`FieldScope::intersection_key
    /// == None`); für Listen von Structs mit einem engeren
    /// Identitätsfeld (z. B. `McpPrincipalToml` über `id` allein) trägt
    /// `FieldScope::intersection_key` den Feldnamen. Bei einem engeren
    /// Identitätsfeld gewinnt für die überlebenden Einträge immer die
    /// vollständige Home-Fassung des Elements — kein Feld-Merge innerhalb
    /// eines Listenelements, selbst wenn ein späterer Layer denselben
    /// Schlüssel mit abweichenden Werten erneut setzt. Keine separate
    /// `MergeRule`-Variante nötig — dieselbe `Intersection` deckt beide
    /// Spielarten ab, nur der Vergleichsschlüssel unterscheidet sich.
    Intersection,
    /// Numerische Obergrenze: effektiver Wert = Minimum aller Layer, die
    /// das Feld setzen (bestehende `min_positive`-Konvention wiederverwendet:
    /// `0`/der jeweilige Unset-Sentinel-Wert eines späteren Layers senkt die
    /// Obergrenze nie weiter).
    MinBound,
    /// Bool-Feld, bei dem `true` der **lockere/erlaubende** Wert ist:
    /// effektiv = UND-Verknüpfung aller Layer, die das Feld setzen. Ein
    /// späterer Layer darf nur abschalten (verschärfen), nie einschalten.
    AndBool,
    /// Bool-Feld, bei dem `true` der **strenge/sichere** Wert ist: effektiv
    /// = ODER-Verknüpfung aller Layer, die das Feld setzen. Ein späterer
    /// Layer darf nur einschalten (verschärfen), nie abschalten.
    OrBool,
    /// Ordinalwert mit expliziter, in `FieldScope::ordering` hinterlegter
    /// Strenge-Reihenfolge (strengster Wert zuerst, siehe
    /// [`PERMISSIONS_DEFAULT_MODE_ORDER`]/[`POLICY_VISIBILITY_SCOPE_ORDER`]).
    /// Effektiv = der strengste unter allen Layern, die das Feld setzen,
    /// gesetzte Wert; ein Versuch, einen lockereren Wert zu wählen, wird
    /// ignoriert + gewarnt. Ein Wert außerhalb der Ordnung wird **nicht**
    /// eingeordnet, sondern verhält sich wie `GlobalOnly` (Risiko R1,
    /// Abschnitt 8 der Spezifikation).
    StricterOf,
    /// Kein eigenständiges Merge: Dieses Feld reist nur als Teil eines
    /// umschließenden atomaren Werts (Listenelement oder Punkt-Struct), der
    /// selbst unter der Regel eines anderen Feldes gemergt wird. Existiert,
    /// damit ein Exhaustivitäts-Test (Paket C) auch solche Felder
    /// nachweislich erfasst.
    CompositeMember,
    /// Wird über Layer hinweg **nie** zusammengeführt: Jeder Layer prüft
    /// seinen eigenen Wert unabhängig gegen die unterstützte(n)
    /// Schema-Version(en) beim Laden dieser einen Datei. Nur für
    /// `config_version` verwendet.
    PerFileValidated,
}

/// Wo ein Feld herkommen darf, bevor [`MergeRule`] bestimmt, *wie* mehrere
/// Layer-Werte kombiniert werden. Unabhängig von [`SettingScope`] (siehe
/// Moduldoku oben) — dieses Enum beschreibt Config-*Datei*-Layer
/// (Home/Profil), nicht die Laufzeit-Lebensdauer einer einzelnen Einstellung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Der Home-Layer (`~/.harw`) legt die Baseline fest; das Profil ist ihr
    /// untergeordnet (Details je nach [`MergeRule`]).
    Global,
    /// Betrifft nur das aktive Profil; die Profil-Ebene ersetzt dort den
    /// globalen Wert ([`MergeRule::ProfileReplaces`]).
    Profile,
    /// Kein Scope im GLOBAL/PROFIL-Sinn — aktuell nur `config_version`
    /// ([`MergeRule::PerFileValidated`]).
    NotScoped,
}

/// Strenge-Reihenfolge für `FieldScope::ordering` von
/// `permissions.default_mode` (Abschnitt 6.2): Index 0 = strengster Wert.
/// `ask` fragt bei jeder Aktion nach (sicherste Einstellung), `full` nie
/// (offenste Einstellung), `auto` liegt dazwischen. Quelle der Wertemenge:
/// `permissions_toml::ALLOWED_MODES`; die Reihenfolge selbst ist neu von der
/// Spezifikation festgelegt (im bisherigen Code nirgends kodiert).
pub const PERMISSIONS_DEFAULT_MODE_ORDER: &[&str] = &["ask", "auto", "full"];

/// Strenge-Reihenfolge (bewusste **Teilordnung**, Risiko R1 Abschnitt 8) für
/// `policy.default_visibility_scope`: Index 0 = strengster Wert. Deckt nur
/// die beiden im Code belegten Werte ab; jeder dritte/unbekannte String wird
/// **nicht** eingeordnet, sondern von `crate::merge::stricter_of` wie eine
/// `GlobalOnly`-Abweichung behandelt (ignoriert + `ScopeDiagnostic`).
pub const POLICY_VISIBILITY_SCOPE_ORDER: &[&str] = &["self", "everyone"];

/// Strenge-Reihenfolge für `tools.doc.remote_ocr`: Index 0 = strengster Wert.
/// `off` schickt nie eine Datei nach außen, `ask` nur nach Freigabe, `on`
/// ohne Nachfrage. Entspricht der Variantenreihenfolge von
/// [`crate::RemoteOcrMode`].
pub const TOOLS_DOC_REMOTE_OCR_ORDER: &[&str] = &["off", "ask", "on"];

/// Ein Eintrag der zentralen Deklarationstabelle [`FIELD_TABLE`]
/// (`docs/design/config-scopes.md` Abschnitt 7a).
#[derive(Debug, Clone, Copy)]
pub struct FieldScope {
    /// Gepunkteter `HarnessConfig`-Pfad, exakt wie in Abschnitt 1/6.3 der
    /// Spezifikation, z. B. `"mcp_listener.enabled"` oder
    /// `"internal_models.session_title"`. Für Listenelement-Unterfelder mit
    /// `[]`-Notation wie in der Spezifikation, z. B.
    /// `"mcp_listener.principals[].id"`. Zwei Sonderfälle, die keinem
    /// eindeutigen einzelnen TOML-Container zugeordnet werden können
    /// (`RuleToml` wird sowohl von `permissions.allow[]` als auch von
    /// `permissions.deny[]` verwendet; `InternalModelChoice` von jeder der
    /// zehn `internal_models.<stelle>`-Stellen), übernehmen die exakte
    /// Pfad-Schreibweise aus Abschnitt 6.3 der Spezifikation
    /// (`"RuleToml.tool"`, `"RuleToml.pattern"`,
    /// `"InternalModelChoice.provider/.model"`).
    pub path: &'static str,
    /// Ob das Feld Home- oder Profil-Ebene zugeordnet ist (siehe
    /// [`MergeRule`] für die tatsächliche Merge-Semantik).
    pub scope: Scope,
    /// Die anzuwendende Merge-Regel.
    pub merge: MergeRule,
    /// Strenge-Reihenfolge für `MergeRule::StricterOf`, strengster Wert
    /// zuerst (siehe [`PERMISSIONS_DEFAULT_MODE_ORDER`]/
    /// [`POLICY_VISIBILITY_SCOPE_ORDER`]). `None` für jede andere Regel.
    pub ordering: Option<&'static [&'static str]>,
    /// Nur für `MergeRule::Intersection` auf einer Liste von Structs
    /// relevant: der Feldname, der als Vergleichsschlüssel dient (z. B.
    /// `Some("id")` für `mcp_listener.principals`). `None` bedeutet volle
    /// Wertgleichheit des Elements (Default-Fall laut `MergeRule::Intersection`-
    /// Doku) oder — für jede andere `MergeRule`-Variante — schlicht „nicht
    /// zutreffend".
    pub intersection_key: Option<&'static str>,
    /// `true` für vom Nutzer als sicherheits-/beschränkungsrelevant markierte
    /// Felder (🔒 in Abschnitt 2/6 der Spezifikation: Freigaben, Sandbox,
    /// Netzwerk, Secrets, Listener, Berechtigungen). Zusätzliches Feld
    /// gegenüber dem in Abschnitt 6.1 skizzierten `FieldScope` — dort nicht
    /// vorgesehen, aber als reine Metadaten-Ergänzung (kein Einfluss auf die
    /// Merge-Logik) mit der Spezifikation vereinbar.
    pub security_critical: bool,
}

/// Die zentrale, öffentliche Deklarationstabelle: ein Eintrag pro
/// `HarnessConfig`-Blattfeld (`docs/design/config-scopes.md` Abschnitt 6.3),
/// in derselben Reihenfolge wie Abschnitt 1/6.3 der Spezifikation, damit die
/// Tabelle 1:1 dagegen geprüft werden kann. Exakt 115 Einträge (Abschnitt 6.3
/// Kontrollsumme: `ProfileReplaces` 57 · `GlobalOnly` 11 · `MinBound` 21 ·
/// `CompositeMember` 11 · `Intersection` 4 · `OrBool` 3 · `Union` 2 ·
/// `AndBool` 2 · `StricterOf` 3 · `PerFileValidated` 1).
///
/// Rein deklarativ — keine Merge-Logik (die lebt in `crate::merge`). Der
/// Exhaustivitäts-Test, der jedes `HarnessConfig`-/Section-Struct-Feld
/// gegen diese Tabelle prüft, lebt bewusst in Paket C
/// (`harw-config/tests/config_scope_exhaustive.rs`), nicht hier (Abschnitt
/// 7a der Spezifikation).
#[rustfmt::skip]
pub static FIELD_TABLE: &[FieldScope] = &[
    // 1.1 Top-Level (11)
    FieldScope { path: "config_version", scope: Scope::NotScoped, merge: MergeRule::PerFileValidated, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "workspace_root", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "default_provider", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "default_model", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "active_agent_definition", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "active_uia_definition", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_provider", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_model", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_worker_model", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "policy_profile", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "project_root_markers", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.2 [logging] (3)
    FieldScope { path: "logging.level", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "logging.target_module_paths", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "logging.json", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.3 [tui] (2)
    FieldScope { path: "tui.theme", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tui.keybindings_file", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // Runde 5, Teil I: Live-Stream der Kind-Agenten im Verlauf.
    FieldScope { path: "tui.child_stream", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.4 [session] (5)
    FieldScope { path: "session.store_dir", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "session.journal_format", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "session.retention_days", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "session.title_generation", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "session.title_model", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.5 [policy] (2)
    FieldScope { path: "policy.default_visibility_scope", scope: Scope::Global, merge: MergeRule::StricterOf, ordering: Some(POLICY_VISIBILITY_SCOPE_ORDER), intersection_key: None, security_critical: true },
    FieldScope { path: "policy.require_approval_for", scope: Scope::Global, merge: MergeRule::Union, ordering: None, intersection_key: None, security_critical: true },
    // 1.6 [mcp_listener] (9)
    FieldScope { path: "mcp_listener.enabled", scope: Scope::Global, merge: MergeRule::AndBool, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.listen_addr", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.path", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.principals", scope: Scope::Global, merge: MergeRule::Intersection, ordering: None, intersection_key: Some("id"), security_critical: true },
    FieldScope { path: "mcp_listener.principals[].id", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.principals[].credential_ref", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.principals[].tenant", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.principals[].workspace", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "mcp_listener.principals[].job_capabilities", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    // 1.7 [onboarding] (4)
    FieldScope { path: "onboarding.seen", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "onboarding.seen.provider", scope: Scope::Profile, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "onboarding.seen.model", scope: Scope::Profile, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "onboarding.seen.channel", scope: Scope::Profile, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: false },
    // 1.8 [tools.plan] (9)
    FieldScope { path: "tools.plan.enabled", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.persist", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.require_for_complex_work", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.validate_dependency_cycles", scope: Scope::Global, merge: MergeRule::OrBool, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.validate_write_conflicts", scope: Scope::Global, merge: MergeRule::OrBool, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.max_nodes", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.require_exploration_for", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.exploration_ttl_secs", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "tools.plan.max_expand_depth", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    // 1.8a [tools.doc] (1) — Remote-OCR für `doc.read_pdf`. Home und Profil
    // setzen frei, ein nicht vertrautes Projekt darf nur verschärfen
    // (`merge_tools_doc`, Ordnung `off` < `ask` < `on`).
    FieldScope { path: "tools.doc.remote_ocr", scope: Scope::Profile, merge: MergeRule::StricterOf, ordering: Some(TOOLS_DOC_REMOTE_OCR_ORDER), intersection_key: None, security_critical: true },
    // 1.9 [mode] (1)
    FieldScope { path: "mode.default", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.10 [research] (5)
    FieldScope { path: "research.network_allow_hosts", scope: Scope::Global, merge: MergeRule::Intersection, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "research.cargo_registry_read", scope: Scope::Global, merge: MergeRule::AndBool, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "research.max_fetch_bytes", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "research.fetch_timeout_secs", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "research.cache_ttl_secs", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.11 [permissions] (7)
    FieldScope { path: "permissions.default_mode", scope: Scope::Global, merge: MergeRule::StricterOf, ordering: Some(PERMISSIONS_DEFAULT_MODE_ORDER), intersection_key: None, security_critical: true },
    FieldScope { path: "permissions.approval_timeout_secs", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    // Runde 7, Teil L4: Zeitlimit des Auto-Modus-Klassifizierers.
    FieldScope { path: "permissions.auto_classifier_timeout_secs", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "permissions.allow", scope: Scope::Global, merge: MergeRule::Intersection, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "permissions.deny", scope: Scope::Global, merge: MergeRule::Union, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "permissions.extra_roots", scope: Scope::Global, merge: MergeRule::Intersection, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "RuleToml.tool", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "RuleToml.pattern", scope: Scope::Global, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: true },
    // 1.12 [sandbox] (8)
    FieldScope { path: "sandbox.cargo", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.cargo.mode", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.cargo.cargo_bin", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.cargo.rustup_home", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.cargo.cargo_home", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.tmux", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.tmux.mode", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "sandbox.tmux.socket_path", scope: Scope::Global, merge: MergeRule::GlobalOnly, ordering: None, intersection_key: None, security_critical: true },
    // 1.13 [internal_models] (13)
    FieldScope { path: "internal_models.use_openrouter_defaults", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.session_title", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.compaction_summary", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.memory_consolidation", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.dream_reflection", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.explorer", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.research", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.worker_simple", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.worker_complex", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.root_orchestrator", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.sub_orchestrator", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // Runde 5, Teil E: Modell des Auto-Modus-Klassifizierers.
    FieldScope { path: "internal_models.auto_classifier", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "internal_models.work_driver_judge", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "InternalModelChoice.provider/.model", scope: Scope::Profile, merge: MergeRule::CompositeMember, ordering: None, intersection_key: None, security_critical: false },
    // 1.14 [compaction] (2)
    FieldScope { path: "compaction.absolute_ceiling_tokens", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "compaction.max_history_bytes", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.15 [reasoning] (6)
    FieldScope { path: "reasoning.uia", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "reasoning.root_orchestrator", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "reasoning.root_orchestrator_with_subs", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "reasoning.sub_orchestrator", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "reasoning.worker_complex", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "reasoning.worker_simple", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.16 [guards] (8)
    FieldScope { path: "guards.enabled", scope: Scope::Global, merge: MergeRule::OrBool, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.repeated_failure_warn", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.repeated_failure_abort", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.no_progress_rounds_warn", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.no_progress_rounds_abort", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.plan_stale_rounds", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    // Runde 7, Teil A2: Orchestrator-Lesebudget; ein Profil darf nur verengen.
    FieldScope { path: "guards.orchestrator_read_warn", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "guards.orchestrator_read_limit", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    // 1.17 [knowledge] (1) — Plan D3: Diary-Aufbewahrung; kein Sicherheitsbezug
    // (die Wartung verschiebt ins Rollup, sie löscht nichts).
    FieldScope { path: "knowledge.diary.retention_days", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.18 [dream] (5) — Plan D5: Traum-Scheduler; kein Sicherheitsbezug
    // (Träume schreiben nur review-gated Berichte, keine Werkzeuge).
    FieldScope { path: "dream.enabled", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "dream.budget", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "dream.idle_minutes", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "dream.cooldown_minutes", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "dream.schedule", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.19 [host] (1) — Runde 5, Teil B: Merkfrist des sudo-Passworts in der
    // TUI; sicherheitsrelevant, ein Profil darf sie nur verkürzen.
    FieldScope { path: "host.sudo_session_minutes", scope: Scope::Global, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    // 1.20 [uia_worker_models] (5) — Runde 5, Teil G: eigene Modellwahl je
    // UIA-Worker-Rolle (`"uia"` oder `"provider/modell"`).
    FieldScope { path: "uia_worker_models.uia_worker", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_worker_models.uia_shell_worker", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_worker_models.uia_writer", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_worker_models.uia_latex_writer", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "uia_worker_models.uia_explorer", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    // 1.21 [agents] (4) — Runde 5, Teil K: Orchestrierungsgrenzen. Home und
    // Profil setzen frei (auch nach oben), ein nicht vertrautes Projekt darf
    // nur senken (`merge_agent_limits`); Klemmen erst beim Lesen.
    FieldScope { path: "agents.max_root_orchestrators", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "agents.max_sub_orchestrators", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "agents.max_sub_orchestrator_depth", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    FieldScope { path: "agents.max_spawn_depth", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    // 1.22 [shell] (1) — Runde 5, Teil N: Obergrenze für `shell.exec`-
    // Zeitlimits. Home und Profil setzen frei, ein nicht vertrautes Projekt
    // senkt nur (`merge_shell_limits`); Klemmen erst beim Lesen.
    FieldScope { path: "shell.max_timeout_secs", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: true },
    // [jobs] (1) — Höchstzahl laufender Hintergrund-Jobs. Home und Profil
    // setzen frei, ein nicht vertrautes Projekt senkt nur (`merge_jobs`);
    // 0 oder > 256 ist schon beim Parsen ein Fehler.
    FieldScope { path: "jobs.max_running", scope: Scope::Profile, merge: MergeRule::MinBound, ordering: None, intersection_key: None, security_critical: false },
    // [agent_compiler] (3) — #22 Welle 2B: Build-Cache, Versionsaufbewahrung
    // und UIA-Autobuild. Reine Komfort-Grenzen ohne Rechtewirkung; ein nicht
    // vertrautes Projekt darf sie nicht setzen (`merge_agent_compiler`).
    FieldScope { path: "agent_compiler.cache_max_bytes", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "agent_compiler.keep_versions", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
    FieldScope { path: "agent_compiler.auto_build_uia", scope: Scope::Profile, merge: MergeRule::ProfileReplaces, ordering: None, intersection_key: None, security_critical: false },
];

#[cfg(test)]
mod merge_rule_tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_field_table_has_exactly_106_entries() {
        // Runde 5, Teil B: +1 für `host.sudo_session_minutes`.
        // Runde 5, Teil E: +1 für `internal_models.auto_classifier`.
        // Runde 5, Teil G: +5 für `uia_worker_models.*`.
        // Runde 5, Teil I: +1 für `tui.child_stream`.
        // Runde 5, Teil K: +4 für `agents.*`.
        // Runde 5, Teil N: +1 für `shell.max_timeout_secs`.
        // Runde 7, Teil A2: +2 für `guards.orchestrator_read_*`.
        // Runde 7, Teil L4: +1 für `permissions.auto_classifier_timeout_secs`.
        // `[tools.doc]`: +1 für `tools.doc.remote_ocr`.
        // #22 Welle 2B: +3 für `agent_compiler.*`.
        // R14: +1 für `internal_models.work_driver_judge`.
        // [jobs]: +1 für `jobs.max_running`.
        assert_eq!(FIELD_TABLE.len(), 120);
    }

    #[test]
    fn test_field_table_paths_are_unique() {
        let mut paths: Vec<&str> = FIELD_TABLE.iter().map(|f| f.path).collect();
        let before = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), before, "FIELD_TABLE enthaelt doppelte Pfade");
    }

    #[test]
    fn test_merge_rule_variant_control_sum_matches_abschnitt_6_3() {
        let count = |rule: MergeRule| FIELD_TABLE.iter().filter(|f| f.merge == rule).count();
        // Runde 5: Teil E +1 (`internal_models.auto_classifier`), Teil G +5
        // (`uia_worker_models.*`), Teil I +1 (`tui.child_stream`).
        // #22 Welle 2B: +3 (`agent_compiler.*`).
        // R14: +1 (`internal_models.work_driver_judge`).
        assert_eq!(count(MergeRule::ProfileReplaces), 61);
        assert_eq!(count(MergeRule::GlobalOnly), 11);
        // Runde 5, Teil K: +4 (`agents.*`); Teil N: +1 (`shell.max_timeout_secs`).
        // Runde 7: Teil A2 +2 (`guards.orchestrator_read_*`), Teil L4 +1
        // (`permissions.auto_classifier_timeout_secs`).
        // [jobs]: +1 (`jobs.max_running`).
        assert_eq!(count(MergeRule::MinBound), 22);
        assert_eq!(count(MergeRule::CompositeMember), 11);
        assert_eq!(count(MergeRule::Intersection), 4);
        assert_eq!(count(MergeRule::OrBool), 3);
        assert_eq!(count(MergeRule::Union), 2);
        assert_eq!(count(MergeRule::AndBool), 2);
        // `[tools.doc]`: +1 (`tools.doc.remote_ocr`).
        assert_eq!(count(MergeRule::StricterOf), 3);
        assert_eq!(count(MergeRule::PerFileValidated), 1);
    }

    #[test]
    fn test_only_stricter_of_entries_carry_an_ordering() {
        for entry in FIELD_TABLE {
            if entry.merge == MergeRule::StricterOf {
                assert!(
                    entry.ordering.is_some(),
                    "{} sollte eine ordering tragen",
                    entry.path
                );
            } else {
                assert!(
                    entry.ordering.is_none(),
                    "{} sollte keine ordering tragen",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn test_only_mcp_listener_principals_carries_an_intersection_key() {
        for entry in FIELD_TABLE {
            if entry.path == "mcp_listener.principals" {
                assert_eq!(entry.intersection_key, Some("id"));
            } else {
                assert!(
                    entry.intersection_key.is_none(),
                    "{} sollte keinen intersection_key tragen",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn test_security_critical_fields_match_abschnitt_2_lock_marks() -> TestResult {
        assert!(
            FIELD_TABLE
                .iter()
                .find(|f| f.path == "workspace_root")
                .ok_or(TestError::Missing("workspace_root"))?
                .security_critical
        );
        assert!(
            FIELD_TABLE
                .iter()
                .find(|f| f.path == "policy_profile")
                .ok_or(TestError::Missing("policy_profile"))?
                .security_critical
        );
        assert!(
            FIELD_TABLE
                .iter()
                .find(|f| f.path == "mcp_listener.principals")
                .ok_or(TestError::Missing("mcp_listener.principals"))?
                .security_critical
        );
        assert!(
            !FIELD_TABLE
                .iter()
                .find(|f| f.path == "session.retention_days")
                .ok_or(TestError::Missing("session.retention_days"))?
                .security_critical
        );
        assert!(
            !FIELD_TABLE
                .iter()
                .find(|f| f.path == "compaction.absolute_ceiling_tokens")
                .ok_or(TestError::Missing("compaction.absolute_ceiling_tokens"))?
                .security_critical
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::cmp::Ordering;

    #[test]
    fn test_precedence_ordering_global_lt_project_lt_session() {
        assert!(SettingScope::Global < SettingScope::Project);
        assert!(SettingScope::Project < SettingScope::Session);
        assert!(SettingScope::Global < SettingScope::Session);
        assert_eq!(
            SettingScope::Session.cmp(&SettingScope::Global),
            Ordering::Greater
        );
    }

    #[test]
    fn test_as_str_matches_from_str_round_trip() -> TestResult {
        for scope in [
            SettingScope::Session,
            SettingScope::Project,
            SettingScope::Global,
        ] {
            let parsed: SettingScope = scope.as_str().parse().map_err(ctx("valid scope text"))?;
            assert_eq!(parsed, scope);
        }
        Ok(())
    }

    #[test]
    fn test_from_str_rejects_unknown_value() -> TestResult {
        let error = match "world".parse::<SettingScope>() {
            Err(e) => e,
            Ok(_) => return Err(TestError::Unexpected("Err erwartet".into())),
        };
        assert!(error.to_string().contains("world"));
        Ok(())
    }

    #[test]
    fn test_display_uses_lowercase_text() {
        assert_eq!(SettingScope::Global.to_string(), "global");
        assert_eq!(SettingScope::Project.to_string(), "project");
        assert_eq!(SettingScope::Session.to_string(), "session");
    }

    #[test]
    fn test_serde_uses_lowercase_encoding() -> TestResult {
        let encoded =
            serde_json::to_string(&SettingScope::Project).map_err(ctx("serialize scope"))?;
        assert_eq!(encoded, "\"project\"");
        let decoded: SettingScope =
            serde_json::from_str("\"session\"").map_err(ctx("deserialize scope"))?;
        assert_eq!(decoded, SettingScope::Session);
        Ok(())
    }
}
