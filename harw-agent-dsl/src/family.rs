//! Auflösung von Agent-Familien über die Layer-Kaskade (§14 DSL-Spec).
//!
//! Dieses Modul implementiert das Family-Konzept aus §14 der Harwness Agent
//! Definition DSL. Familien sind wiederverwendbare Design- und Policy-Bündel,
//! die Orchestrator- und Worker-Definitionen gruppieren sowie familienweite
//! Invarianten und Standardwerte festlegen.
//!
//! # Verantwortlichkeit
//! - Definition der Roh-Struktur [`RawFamilyDefinition`] (TOML-nah, vor Merge/Resolve)
//! - Definition von [`FamilyRoster`] (erlaubte Definitions-Referenzen)
//! - Definition der aufgelösten Intermediate-Representation [`ResolvedFamily`]
//! - Auflösungsfunktion [`resolve_family`] über geordnete Schichten
//!
//! # Schlüsseltypen
//! - [`RawFamilyDefinition`] — TOML-nahe Rohstruktur einer Family
//! - [`FamilyRoster`] — Liste erlaubter Definitions-Referenzen
//! - [`ResolvedFamily`] — aufgelöste Family (IR für Consumers)
//! - [`RawFamilyMembership`] — Mitgliedschaftsantrag einer Rolle, eigene Datei
//!   je Rolle statt Eintrag in der Familiendatei (Knoten AW6-02; Begründung an
//!   der Typ-Dokumentation von [`RawFamilyMembership`])
//! - [`admit_family_member`] / [`merge_memberships_into`] — prüfen und führen
//!   Mitgliedschaften additiv in eine aufgelöste Familie ein
//!
//! # Merge-Semantik (§7)
//! Patches werden über explizite Sub-Tabellen in `patch` ausgedrückt:
//! - `patch.orchestrators.allowed` — Array-MergeOp (append/remove/replace/intersect)
//! - `patch.workers.allowed` — Array-MergeOp analog
//! - `patch.invariants` — append/prepend/remove auf Strings
//! - `patch.defaults` — flacher Replace tabellen-tief
//! - `patch.name` — optionaler Replace des Family-Namens
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`; [`resolve_family`] ist zustandslos und thread-sicher.
//!
//! # Fehler
//! - [`crate::error::DslError::MissingBase`]: wenn `target_id` oder `extends` nicht in `layers`

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::authority::AuthorityCeiling;
use crate::error::{DslError, DslResult};
use crate::ids::{DefinitionId, DefinitionRef};
use crate::layers::DefinitionLayer;
use crate::resolved::{ResolutionStep, ResolutionTrace};

// ---------------------------------------------------------------------------
// Roh-Typen (TOML-nah, vor Merge/Resolve)
// ---------------------------------------------------------------------------

/// Rohe TOML-nahe Family-Definition — vor Merge/Resolve (§14).
///
/// # Beschreibung
/// Entspricht einem Family-TOML-Dokument mit dem Schema `harwness.family/v1`.
/// Alle Felder außer `schema`, `id`, `version` und `name` sind optional und
/// haben sinnvolle Defaults (leere Roster, leere Tabellen).
///
/// # Felder
/// - `schema` (`String`): Schema-Version, z. B. `"harwness.family/v1"`.
/// - `id` ([`DefinitionId`]): namensraum-qualifizierte Familien-ID.
/// - `version` ([`crate::ids::Version`]): semantische Version.
/// - `extends` (`Option<DefinitionRef>`): optionale Basis-Familie.
/// - `name` (`String`): menschenlesbarer Name der Familie.
/// - `orchestrators` ([`FamilyRoster`]): erlaubte Orchestrator-Definitionen.
/// - `workers` ([`FamilyRoster`]): erlaubte Worker-Definitionen.
/// - `defaults` (`toml::Table`): benannte Policy-Defaults.
/// - `invariants` (`Vec<String>`): familienweite Invarianten (freie Strings).
/// - `universe` ([`AuthorityCeiling`]): obere Schranke der Capabilities, die
///   irgendein Mitglied dieser Familie tragen darf (Knoten AW6-01, additiv).
///   Default: leere Menge (kein `[universe]` deklariert).
/// - `patch` (`toml::Table`): Merge-Patches für Vererbung (§7).
///
/// # Erweiterung Knoten AW6-01 — `universe`
/// `universe` ist rein additiv: fehlt `[universe]` in der TOML-Quelle, liefert
/// `#[serde(default)]` eine leere [`AuthorityCeiling`] — bestehende
/// Family-Dateien (`agents/family/research.toml`, `agents/family/coding.toml`)
/// bleiben unverändert gültig und lösen unverändert auf. `universe` fließt
/// nicht in [`crate::executable::compute_snapshot_id`] ein (dieses Modul ist
/// von `resolve_definition`/`lower` vollständig entkoppelt — kein
/// `family::`-Verweis existiert dort), also bleibt die `SnapshotId`
/// bestehender Agentendefinitionen durch diese Erweiterung unverändert.
///
/// Der Zweck: eine Familie deklariert damit, welche Capabilities ihre
/// künftigen Mitglieder — über alle Rollen hinweg, nicht pro Agent — höchstens
/// tragen dürfen. Zwei Familien mit überschneidendem `universe` eröffnen einen
/// Pfad von angreiferkontrolliertem Material in der einen Familie zu
/// produktiven Werkzeugen der anderen; [`AuthorityCeiling::is_disjoint_from`]
/// macht diese Eigenschaft prüfbar (siehe die Tests am Ende dieser Datei sowie
/// `harw-registry-defaults/agents/families/security/security.toml`).
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::family::RawFamilyDefinition;
///
/// let src = r#"
/// schema = "harwness.family/v1"
/// id = "harwness.family.focused-coding@1"
/// version = "1.0.0"
/// name = "Focused Coding Family"
/// "#;
/// let raw: RawFamilyDefinition = toml::from_str(src).unwrap();
/// assert_eq!(raw.name, "Focused Coding Family");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFamilyDefinition {
    /// Schema-Version des Dokuments, z. B. `"harwness.family/v1"`.
    pub schema: String,
    /// Namensraum-qualifizierte ID der Familie (§5).
    pub id: DefinitionId,
    /// Semantische Vollversion der Familie.
    pub version: crate::ids::Version,
    /// Optionale Basis-Familie (single inheritance, §6).
    #[serde(default)]
    pub extends: Option<DefinitionRef>,
    /// Menschenlesbarer Name der Familie.
    pub name: String,
    /// Erlaubte Orchestrator-Definitionen.
    #[serde(default)]
    pub orchestrators: FamilyRoster,
    /// Erlaubte Worker-Definitionen.
    #[serde(default)]
    pub workers: FamilyRoster,
    /// Benannte Policy-Defaults (freie TOML-Tabelle).
    #[serde(default)]
    pub defaults: toml::Table,
    /// Familienweite Invarianten als freie Strings.
    #[serde(default)]
    pub invariants: Vec<String>,
    /// Obere Schranke der Capabilities, die irgendein Mitglied dieser Familie
    /// tragen darf (Knoten AW6-01, additiv). Default: leere Menge.
    #[serde(default)]
    pub universe: AuthorityCeiling,
    /// Merge-Patches für Vererbung (§7); ausgewertet durch [`resolve_family`].
    #[serde(default)]
    pub patch: toml::Table,
}

/// Liste erlaubter Definitions-Referenzen innerhalb einer Familie (§14).
///
/// # Beschreibung
/// Wird sowohl für `orchestrators` als auch für `workers` verwendet.
/// Standardmäßig leer; Einträge sind [`DefinitionRef`]-Werte.
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::family::FamilyRoster;
///
/// let roster = FamilyRoster::default();
/// assert!(roster.allowed.is_empty());
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyRoster {
    /// Explizit erlaubte Definitions-Referenzen.
    #[serde(default)]
    pub allowed: Vec<DefinitionRef>,
}

// ---------------------------------------------------------------------------
// Mitgliedschaft — Knoten AW6-02
// ---------------------------------------------------------------------------

/// Ein Mitgliedschaftsantrag einer Rolle für eine Familie — eigene Datei je
/// Rolle statt Eintrag in der Familiendatei (Knoten AW6-02, §14 additiv).
///
/// # Warum dieser Weg (und nicht Glob-Aufnahme)
/// AW6-01 hat den Befund gemeldet, dass `FamilyRoster.allowed` ein
/// ausdrückliches `Vec<DefinitionRef>` ist, kein Glob — die
/// Verzeichniskonvention aus AW6-00 entkoppelt nur die *Auffindbarkeit* von
/// Rollen, nicht die *Aufnahme* in eine Familie. Ohne Gegenmaßnahme trüge
/// jede Aufnahme in `harwness.family.security@1` denselben Eintrag in
/// derselben `security.toml` — genau die Kollision zwischen AW6-03 (vier
/// Triage-Rollen), AW6-08 (Context Steward) und AW7-06 (Intel-Scout), für die
/// AW6-00 überhaupt gebaut wurde. AW6-03 und AW7-06 können sogar nebenläufig
/// laufen.
///
/// Zwei Wege standen zur Wahl:
/// - **(a) Glob-Aufnahme**: `allowed` nimmt zusätzlich Muster an
///   (`"harwness.agent.security-*@1"`); eine Rolle träte bei, indem sie
///   existiert und passt. Verworfen aus zwei Gründen. Erstens die
///   Kantenrichtung: eine naheliegende Wiederverwendung wäre
///   `harw_plan::ScopeMatcher::matches_glob` gewesen — aber `harw-plan-bridge`
///   existiert als eigene Crate exakt als Naht zwischen `harw-plan` und
///   `harw-agent-dsl`/`harw-core` (siehe deren Kommentar zu `harw-context`:
///   "Kein Zyklus … keines davon kennt harw-plan-bridge"); `harw-agent-dsl`
///   selbst hat und soll keine Abhängigkeit auf `harw-plan` haben — sonst
///   bräuchte es die Brücke nicht. Eine zweite, parallele Glob-Auflösung in
///   `harw-agent-dsl` nachzubauen, hätte genau die Kollision reproduziert, die
///   `harw-plan-bridge` vermeiden soll (zwei Implementierungen derselben
///   Regel). Zweitens die Autoritätsgefahr: eine Familie ist eine
///   Autoritätsgrenze — ihr `universe` bestimmt, was Mitglieder dürfen. Ein
///   Muster wie `"harwness.agent.security-*@1"` nimmt auch jede künftige
///   Rolle auf, die zufällig passt, ohne dass irgendjemand ihre Capabilities
///   gegen das `universe` geprüft hätte — ein zu weites Muster erteilt damit
///   Rechte an eine Rolle, die niemand geprüft hat.
/// - **(b) Bei der ausdrücklichen Liste bleiben**, aber die Aufnahme in eine
///   **eigene Datei je Rolle** verlegen statt in die Familiendatei — genau der
///   Weg dieses Typs. `RawFamilyMembership` ist das Schema dieser Datei
///   (`harwness.family-membership/v1`); [`admit_family_member`] prüft beim
///   Zusammenführen ausdrücklich, dass `capabilities` eine Teilmenge des
///   `universe` der Ziel-Familie ist — **jede** Aufnahme wird geprüft, keine
///   kommt über ein ungeprüftes Muster durch. Damit legt jeder Folgeknoten
///   wieder nur eine eigene Datei an (`agents/families/security/members/<rolle>.toml`),
///   nie die geteilte `security.toml`.
///
/// # Wie eine ungeprüfte Rolle verhindert wird
/// [`admit_family_member`] lehnt jede Mitgliedschaft ab, deren `capabilities`
/// nicht vollständig im `universe` der Ziel-Familie liegen
/// ([`AuthorityCeiling::is_reduction_of`]) — mit
/// [`crate::error::DslError::AuthorityElevation`] (dieselbe Variante, die der
/// Resolver für einen Patch benutzt, der unzulässig Capabilities hinzufügt;
/// eine Rolle, die mehr beansprucht als das `universe` erlaubt, ist
/// semantisch derselbe Fall). Eine Familie ohne `[universe]` (leere Menge)
/// lässt dadurch **keine** Mitgliedschaft mit nichtleeren `capabilities` zu —
/// bewusst strikt, denn eine leere Menge als Autoritätsgrenze heißt "nichts
/// ist geprüft worden", nicht "alles ist erlaubt".
///
/// # Reihenfolgestabilität
/// Dieser Typ trägt selbst keine Reihenfolge — die Stabilität liegt in der
/// Pflicht des Aufrufers ([`merge_memberships_into`]): er verarbeitet
/// `memberships` in der übergebenen Reihenfolge und hängt jede neue Referenz
/// ans Ende des jeweiligen Rosters an. `harw-registry-defaults` sortiert die
/// eingesammelten Mitgliedschafts-Dateien nach ihrem vollen Pfad, bevor sie
/// hier ankommen — dieselbe Regel wie für Families selbst (AW6-00).
///
/// # Felder
/// - `schema` (`String`): Schema-Version, `"harwness.family-membership/v1"`.
/// - `family` ([`DefinitionRef`]): welche Familie die Rolle aufnehmen soll.
/// - `role` ([`DefinitionRef`]): welche Rolle beitritt (`id.kind == "agent"`).
/// - `as_orchestrator` (`bool`): tritt die Rolle als Orchestrator statt als
///   Worker bei? Default `false`.
/// - `capabilities` ([`AuthorityCeiling`]): die von der Rolle beanspruchten
///   Capabilities — geprüft gegen das `universe` der Ziel-Familie. Default:
///   leere Menge (dann muss auch das `universe` leer sein, sonst ist die
///   Teilmengen-Prüfung trivial erfüllt und meldet nichts Falsches — eine
///   leere Beanspruchung ist immer eine Teilmenge).
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::family::RawFamilyMembership;
///
/// let src = r#"
/// schema = "harwness.family-membership/v1"
/// family = "harwness.family.security@1"
/// role = "harwness.agent.security-triage-1@1"
///
/// [capabilities]
/// capabilities = ["security.sensor.read"]
/// "#;
/// let membership: RawFamilyMembership = toml::from_str(src).unwrap();
/// assert!(!membership.as_orchestrator);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFamilyMembership {
    /// Schema-Version des Dokuments, `"harwness.family-membership/v1"`.
    pub schema: String,
    /// Referenz auf die Familie, der die Rolle beitritt.
    pub family: DefinitionRef,
    /// Referenz auf die beitretende Rolle (`id.kind == "agent"`).
    pub role: DefinitionRef,
    /// Tritt die Rolle als Orchestrator bei (statt als Worker)?
    #[serde(default)]
    pub as_orchestrator: bool,
    /// Von der Rolle beanspruchte Capabilities — geprüft gegen das `universe`
    /// der Ziel-Familie ([`admit_family_member`]).
    #[serde(default)]
    pub capabilities: AuthorityCeiling,
}

/// Prüft eine Mitgliedschaftsanfrage gegen das `universe` ihrer Ziel-Familie
/// (Knoten AW6-02) — der wichtigste Baustein dieses Knotens.
///
/// # Beschreibung
/// Lehnt `membership` ab, wenn ihre `capabilities` nicht vollständig im
/// `universe` von `family` liegen. Eine Familie ohne `[universe]` (leere
/// Menge) lässt dadurch nur Mitgliedschaften mit ebenfalls leeren
/// `capabilities` zu.
///
/// # Argumente
/// - `family` (`&ResolvedFamily`): die aufgelöste Ziel-Familie, gegen deren
///   `universe` geprüft wird.
/// - `membership` (`&RawFamilyMembership`): die zu prüfende Mitgliedschaft.
///
/// # Rückgabe
/// `Ok(())`, wenn `membership.capabilities` eine Teilmenge von
/// `family.universe` ist.
///
/// # Fehler
/// - [`crate::error::DslError::AuthorityElevation`] — `membership.capabilities`
///   enthält mindestens eine Capability außerhalb von `family.universe`. Die
///   Variante nennt die beitretende Rolle als `of` und die abgelehnten
///   Capabilities als `added_capabilities`.
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::family::admit_family_member;
///
/// // Siehe die Tests am Ende dieser Datei für einen vollständigen Aufbau
/// // einer `ResolvedFamily` und einer abgelehnten Mitgliedschaft.
/// let _ = admit_family_member;
/// ```
pub fn admit_family_member(
    family: &ResolvedFamily,
    membership: &RawFamilyMembership,
) -> DslResult<()> {
    if membership.capabilities.is_reduction_of(&family.universe) {
        Ok(())
    } else {
        Err(DslError::AuthorityElevation {
            of: Box::new(membership.role.id.clone()),
            added_capabilities: membership.capabilities.added_relative_to(&family.universe),
            location: crate::error::DiagLocation::field("membership.capabilities"),
        })
    }
}

/// Führt geprüfte Mitgliedschaften additiv in eine aufgelöste Familie ein
/// (Knoten AW6-02).
///
/// # Beschreibung
/// Verarbeitet `memberships` in der übergebenen Reihenfolge. Jede
/// Mitgliedschaft, deren `family.id` nicht `resolved.id` entspricht, wird
/// übersprungen (sie gehört einer anderen Familie). Für jede passende
/// Mitgliedschaft wird zuerst [`admit_family_member`] geprüft; bei Erfolg wird
/// `membership.role` — sofern noch nicht enthalten — an `resolved.orchestrators`
/// oder `resolved.workers` angehängt (je nach `as_orchestrator`).
///
/// Diese Funktion ändert **nie** `resolved.universe` — Mitgliedschaft ist
/// Aufnahme in Roster-Listen, keine Erweiterung der Autoritätsgrenze.
///
/// # Argumente
/// - `resolved` (`&mut ResolvedFamily`): die aufzuweitende, bereits über
///   [`resolve_family`] aufgelöste Familie.
/// - `memberships` (`&[RawFamilyMembership]`): alle bekannten
///   Mitgliedschaftsanträge, in der Reihenfolge, in der sie eingelesen wurden
///   (siehe Modul-Dokumentation zu Reihenfolgestabilität).
///
/// # Rückgabe
/// `Ok(())` bei Erfolg; `resolved` ist dann um die zulässigen Mitgliedschaften
/// erweitert.
///
/// # Fehler
/// - [`crate::error::DslError::AuthorityElevation`]: mindestens eine passende
///   Mitgliedschaft beansprucht Capabilities außerhalb von `resolved.universe`
///   (siehe [`admit_family_member`]). Die Verarbeitung bricht bei der ersten
///   Ablehnung ab; bereits angewandte frühere Mitgliedschaften in `resolved`
///   bleiben stehen — der Aufrufer verwirft in diesem Fall die gesamte
///   Auflösung, statt mit einem teilweise gemischten Ergebnis weiterzumachen.
///
/// # Nebenläufigkeit
/// Zustandslos außer der `&mut`-Ausleihe von `resolved`; thread-sicher, wenn
/// jeder Aufrufer seine eigene `ResolvedFamily`-Instanz besitzt.
pub fn merge_memberships_into(
    resolved: &mut ResolvedFamily,
    memberships: &[RawFamilyMembership],
) -> DslResult<()> {
    for membership in memberships {
        if membership.family.id != resolved.id {
            continue;
        }
        admit_family_member(resolved, membership)?;
        let roster = if membership.as_orchestrator {
            &mut resolved.orchestrators
        } else {
            &mut resolved.workers
        };
        if !roster.contains(&membership.role) {
            roster.push(membership.role.clone());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Aufgelöste IR-Typen
// ---------------------------------------------------------------------------

/// Aufgelöste Family-Definition — Intermediate Representation für Consumers (§14, §17).
///
/// # Beschreibung
/// Enthält das Ergebnis nach vollständiger Schichtenauflösung über [`resolve_family`].
/// Alle Roster-Listen sind de-dupliziert und in der durch die Layer-Priorisierung
/// bestimmten Reihenfolge. Der `trace` dokumentiert jeden angewandten Schritt.
///
/// # Felder
/// - `id` — kanonische ID der aufgelösten Familie.
/// - `version` — Version aus dem topmost Layer.
/// - `name` — Name aus dem topmost Layer (überschreibbar per `patch.name.replace`).
/// - `orchestrators` — alle erlaubten Orchestrator-Referenzen.
/// - `workers` — alle erlaubten Worker-Referenzen.
/// - `defaults` — zusammengeführte Policy-Defaults.
/// - `invariants` — alle familienweiten Invarianten.
/// - `universe` — obere Schranke der Capabilities aller Mitglieder dieser
///   Familie (Knoten AW6-01, additiv; siehe [`RawFamilyDefinition::universe`]).
/// - `trace` — Auditpfad der Auflösung.
///
/// # Nebenläufigkeit
/// `Send + Sync`; unveränderlich nach Erstellung.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::family::resolve_family;
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// // Aufbau und Auflösung einer Familie — siehe [`resolve_family`].
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFamily {
    /// Eindeutige Namensraum-ID der aufgelösten Familie.
    pub id: DefinitionId,
    /// Semantische Version aus dem topmost Layer.
    pub version: crate::ids::Version,
    /// Menschenlesbarer Name der Familie.
    pub name: String,
    /// Alle erlaubten Orchestrator-Referenzen nach Merge.
    pub orchestrators: Vec<DefinitionRef>,
    /// Alle erlaubten Worker-Referenzen nach Merge.
    pub workers: Vec<DefinitionRef>,
    /// Zusammengeführte Policy-Defaults.
    pub defaults: toml::Table,
    /// Alle familienweiten Invarianten nach Merge.
    pub invariants: Vec<String>,
    /// Obere Schranke der Capabilities aller Mitglieder dieser Familie
    /// (Knoten AW6-01, additiv). Leer, wenn keine Schicht ein `[universe]`
    /// deklariert.
    pub universe: AuthorityCeiling,
    /// Auditpfad der Auflösung (base + jeder patch-Layer).
    pub trace: ResolutionTrace,
}

// ---------------------------------------------------------------------------
// Auflösungslogik
// ---------------------------------------------------------------------------

/// Löst eine Familie über die Layer-Kaskade auf (§14, §7, §4).
///
/// # Beschreibung
/// Führt die vollständige Familien-Auflösung durch:
/// 1. Sortiert `layers` aufsteigend nach [`DefinitionLayer`]-Priorität (§4).
/// 2. Sucht den niedrigsten Layer mit der `target_id` als Basis-Definition.
/// 3. Löst `extends` rekursiv auf (Fehler [`DslError::MissingBase`] wenn nicht gefunden).
/// 4. Wendet `patch.orchestrators.allowed`, `patch.workers.allowed`,
///    `patch.invariants` und `patch.defaults` jedes weiteren Layers an (§7).
/// 5. Der Name kann per `patch.name.replace` durch einen höheren Layer überschrieben werden.
/// 6. Baut [`ResolutionTrace`] auf: Basis-Schritt + ein Schritt pro Patch-Layer.
///
/// # Patch-Subformat
/// Ein Layer, der die Basis patches, trägt Einträge in seiner `patch`-Tabelle:
/// ```toml
/// [patch.orchestrators.allowed]
/// append = ["mia.agent.custom-orchestrator@1"]
///
/// [patch.workers.allowed]
/// remove = ["harwness.agent.focused-docs-update@1"]
///
/// [patch.invariants]
/// append = ["custom_invariant"]
///
/// [patch.name]
/// replace = "Erweiterter Name"
/// ```
///
/// # Argumente
/// - `target_id` (`&DefinitionId`): ID der aufzulösenden Familie.
/// - `layers` (`&[(DefinitionLayer, RawFamilyDefinition)]`): alle verfügbaren Layer-Einträge.
/// - `now` (`OffsetDateTime`): Zeitstempel für [`ResolutionStep`]-Einträge.
///
/// # Rückgabe
/// [`ResolvedFamily`] bei Erfolg.
///
/// # Fehler
/// - [`DslError::MissingBase`]: wenn `target_id` nicht in `layers` gefunden wird,
///   oder wenn eine `extends`-Referenz nicht auflösbar ist.
///
/// # `universe`-Merge (Knoten AW6-01, additiv)
/// Ohne `extends` ist `resolved.universe` genau die deklarierte
/// [`RawFamilyDefinition::universe`] der Basis-Definition. Mit `extends` gilt
/// dieselbe Philosophie wie für [`AuthorityCeiling`] selbst (§7: nur
/// Schnittbildung, kein Hinzufügen): erklärt die direkte Definition kein
/// `[universe]` (leere Menge), wird das `universe` der aufgelösten Basis
/// unverändert übernommen; erklärt sie eines, wird es mit dem `universe` der
/// Basis GESCHNITTEN (`AuthorityCeiling::intersect`) statt vereinigt — eine
/// abgeleitete Familie kann ihr `universe` also nur einengen, nie erweitern.
/// Ein `append`-Merge (wie bei `orchestrators`/`workers`/`invariants`) würde
/// genau die Zusicherung unterlaufen, die `universe` herstellen soll:
/// Erweiterung entlang von `extends` wäre eine stille Möglichkeit, die
/// Disjunktheit zweier Familien nachträglich zu brechen.
///
/// **Limitation:** `patch.universe.*` wird von dieser Funktion (noch) nicht
/// ausgewertet — anders als `patch.orchestrators.allowed`,
/// `patch.workers.allowed` und `patch.invariants`. Ein Patch-Layer kann
/// `universe` heute nicht verändern. Das ist eine bewusste Beschränkung
/// dieses additiven Knotens (AW6-01 hat keinen Bedarf an Patch-Semantik für
/// `universe`), keine vergessene Funktion; eine künftige Erweiterung müsste
/// dieselbe Intersect-only-Regel wie oben durchsetzen.
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher. Keine Locks, keine Threads.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_agent_dsl::family::{RawFamilyDefinition, resolve_family};
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let src = r#"
/// schema = "harwness.family/v1"
/// id = "harwness.family.focused-coding@1"
/// version = "1.0.0"
/// name = "Focused Coding Family"
/// "#;
/// let raw: RawFamilyDefinition = toml::from_str(src).unwrap();
/// let id = DefinitionId::parse("harwness.family.focused-coding@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_family(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.name, "Focused Coding Family");
/// ```
pub fn resolve_family(
    target_id: &DefinitionId,
    layers: &[(DefinitionLayer, RawFamilyDefinition)],
    now: OffsetDateTime,
) -> DslResult<ResolvedFamily> {
    // Sortiere Layer aufsteigend (niedrigste Priorität zuerst).
    let mut sorted: Vec<&(DefinitionLayer, RawFamilyDefinition)> = layers.iter().collect();
    sorted.sort_by_key(|(layer, _)| *layer);

    // Finde den ersten (niedrigsten) Layer, der die target_id definiert.
    let base_entry = sorted
        .iter()
        .find(|(_, def)| &def.id == target_id)
        .ok_or_else(|| DslError::MissingBase {
            of: Box::new(target_id.clone()),
            referenced: Box::new(DefinitionRef {
                id: target_id.clone(),
                version: None,
            }),
            location: crate::error::DiagLocation::none(),
        })?;

    let base_def = &base_entry.1;

    // Wenn `extends` gesetzt: Basis-Familie rekursiv auflösen.
    let mut resolved_orchestrators: Vec<DefinitionRef>;
    let mut resolved_workers: Vec<DefinitionRef>;
    let mut resolved_invariants: Vec<String>;
    let mut resolved_defaults: toml::Table;
    let mut resolved_universe: AuthorityCeiling;
    let mut resolved_name: String;
    let mut resolved_version: crate::ids::Version;
    let mut trace_steps: Vec<ResolutionStep>;

    if let Some(extends_ref) = &base_def.extends {
        // Rekursiv die Basis auflösen.
        let parent = resolve_family(&extends_ref.id, layers, now)?;
        // Name und Version kommen aus der direkten Definition (überschreibt die Basis).
        resolved_name = base_def.name.clone();
        resolved_version = base_def.version.clone();
        resolved_orchestrators = parent.orchestrators;
        resolved_workers = parent.workers;
        resolved_invariants = parent.invariants;
        resolved_defaults = parent.defaults;
        resolved_universe = parent.universe;
        trace_steps = parent.trace.steps;

        // Eigene Basiswerte der aktuellen Definition (ohne patch) aufnehmen.
        // Die Roster der direkten Definition überschreibt/ergänzt die geerbte nicht —
        // das geschieht nur über patch. Die direkte Definition ist der "Schritt" selbst.
        // Gemäß §14: Der direkte Layer fügt seine eigenen allowed-Einträge hinzu (append-Semantik).
        for r in &base_def.orchestrators.allowed {
            if !resolved_orchestrators.contains(r) {
                resolved_orchestrators.push(r.clone());
            }
        }
        for r in &base_def.workers.allowed {
            if !resolved_workers.contains(r) {
                resolved_workers.push(r.clone());
            }
        }
        for inv in &base_def.invariants {
            if !resolved_invariants.contains(inv) {
                resolved_invariants.push(inv.clone());
            }
        }
        // Defaults: flacher Merge (base_def.defaults überschreibt geerbte Einträge).
        for (k, v) in &base_def.defaults {
            resolved_defaults.insert(k.clone(), v.clone());
        }

        // universe: nur Schnittbildung, kein Anhängen (siehe Funktionsdoku
        // "universe-Merge"). Eine leere direkte Deklaration bedeutet "keine
        // eigene Aussage" und lässt das geerbte universe unverändert — sonst
        // würde jede Familie ohne eigenes `[universe]` ihr geerbtes universe
        // auf die leere Menge kollabieren, was `extends` ohne `[universe]`
        // faktisch zu einer verdeckten Kappung machen würde.
        if !base_def.universe.capabilities.is_empty() {
            resolved_universe = resolved_universe.intersect(&base_def.universe);
        }

        // Schritt für die direkte Base-Definition.
        trace_steps.push(ResolutionStep {
            source: target_id.as_string(),
            kind: "base".to_owned(),
            applied_at: now,
        });
    } else {
        // Kein extends: direkte Definition ist die Basis.
        resolved_orchestrators = base_def.orchestrators.allowed.clone();
        resolved_workers = base_def.workers.allowed.clone();
        resolved_invariants = base_def.invariants.clone();
        resolved_defaults = base_def.defaults.clone();
        resolved_universe = base_def.universe.clone();
        resolved_name = base_def.name.clone();
        resolved_version = base_def.version.clone();
        trace_steps = vec![ResolutionStep {
            source: target_id.as_string(),
            kind: "base".to_owned(),
            applied_at: now,
        }];
    }

    // Alle Layer über der Basis (d. h. mit höherem Layer-Wert als dem Base-Layer)
    // die ebenfalls die target_id referenzieren und Patches tragen, anwenden.
    let base_layer = base_entry.0;
    for (layer, def) in &sorted {
        // Nur Layer, die nach der Basis kommen und dieselbe target_id betreffen.
        if *layer <= base_layer || &def.id != target_id {
            continue;
        }

        let patch = &def.patch;
        if patch.is_empty() {
            continue;
        }

        // Patch: orchestrators.allowed
        if let Some(orch_patch) = patch.get("orchestrators")
            && let Some(allowed_op) = orch_patch.get("allowed")
        {
            apply_roster_patch(allowed_op, &mut resolved_orchestrators, layers)?;
        }

        // Patch: workers.allowed
        if let Some(workers_patch) = patch.get("workers")
            && let Some(allowed_op) = workers_patch.get("allowed")
        {
            apply_roster_patch(allowed_op, &mut resolved_workers, layers)?;
        }

        // Patch: invariants
        if let Some(inv_patch) = patch.get("invariants") {
            apply_string_patch(inv_patch, &mut resolved_invariants)?;
        }

        // Patch: defaults (flacher Replace)
        if let Some(toml::Value::Table(defaults_patch)) = patch.get("defaults") {
            for (k, v) in defaults_patch {
                resolved_defaults.insert(k.clone(), v.clone());
            }
        }

        // Patch: name
        if let Some(name_patch) = patch.get("name")
            && let Some(toml::Value::String(new_name)) = name_patch.get("replace")
        {
            resolved_name = new_name.clone();
        }

        // Höchste Layer-Version und Name werden bevorzugt.
        resolved_version = def.version.clone();

        trace_steps.push(ResolutionStep {
            source: target_id.as_string(),
            kind: "patch".to_owned(),
            applied_at: now,
        });
    }

    Ok(ResolvedFamily {
        id: target_id.clone(),
        version: resolved_version,
        name: resolved_name,
        orchestrators: resolved_orchestrators,
        workers: resolved_workers,
        defaults: resolved_defaults,
        invariants: resolved_invariants,
        universe: resolved_universe,
        trace: ResolutionTrace { steps: trace_steps },
    })
}

// ---------------------------------------------------------------------------
// Hilfsfunktionen (private)
// ---------------------------------------------------------------------------

/// Wendet eine Roster-Patch-Operation (append/remove/replace/intersect) an.
///
/// # Beschreibung
/// Liest aus `op_value` (einem TOML-Wert) die Operationsart und die Ziel-Referenzen
/// als Strings, konvertiert sie zu [`DefinitionRef`]-Werten und wendet die Operation
/// auf `roster` an.
///
/// # Fehler
/// - [`DslError::MissingBase`]: nie (nur für Roster-Ops verwendet).
/// - [`DslError::UnknownMergeOp`]: wenn `op_value` keine bekannte Operation enthält.
fn apply_roster_patch(
    op_value: &toml::Value,
    roster: &mut Vec<DefinitionRef>,
    _layers: &[(DefinitionLayer, RawFamilyDefinition)],
) -> DslResult<()> {
    let op_table = match op_value.as_table() {
        Some(t) => t,
        None => return Ok(()),
    };

    if let Some(toml::Value::Array(items)) = op_table.get("append") {
        for item in items {
            if let Some(s) = item.as_str() {
                let r = parse_def_ref(s)?;
                if !roster.contains(&r) {
                    roster.push(r);
                }
            }
        }
    }

    if let Some(toml::Value::Array(items)) = op_table.get("prepend") {
        let mut new_entries: Vec<DefinitionRef> = Vec::new();
        for item in items {
            if let Some(s) = item.as_str() {
                let r = parse_def_ref(s)?;
                if !roster.contains(&r) {
                    new_entries.push(r);
                }
            }
        }
        new_entries.append(roster);
        *roster = new_entries;
    }

    if let Some(toml::Value::Array(items)) = op_table.get("remove") {
        let to_remove: DslResult<Vec<DefinitionRef>> = items
            .iter()
            .filter_map(|v| v.as_str())
            .map(parse_def_ref)
            .collect();
        let to_remove = to_remove?;
        roster.retain(|r| !to_remove.contains(r));
    }

    if let Some(toml::Value::Array(items)) = op_table.get("replace") {
        let new_roster: DslResult<Vec<DefinitionRef>> = items
            .iter()
            .filter_map(|v| v.as_str())
            .map(parse_def_ref)
            .collect();
        *roster = new_roster?;
    }

    if let Some(toml::Value::Array(items)) = op_table.get("intersect") {
        let intersect_set: DslResult<Vec<DefinitionRef>> = items
            .iter()
            .filter_map(|v| v.as_str())
            .map(parse_def_ref)
            .collect();
        let intersect_set = intersect_set?;
        roster.retain(|r| intersect_set.contains(r));
    }

    Ok(())
}

/// Wendet eine Invarianten-Patch-Operation (append/prepend/remove) an.
///
/// # Beschreibung
/// Liest aus `op_value` die String-Listen und modifiziert `invariants` in-place.
///
/// # Fehler
/// Keine (ungültige Typen werden ignoriert).
fn apply_string_patch(op_value: &toml::Value, invariants: &mut Vec<String>) -> DslResult<()> {
    let op_table = match op_value.as_table() {
        Some(t) => t,
        None => return Ok(()),
    };

    if let Some(toml::Value::Array(items)) = op_table.get("append") {
        for item in items {
            if let Some(s) = item.as_str() {
                let owned = s.to_owned();
                if !invariants.contains(&owned) {
                    invariants.push(owned);
                }
            }
        }
    }

    if let Some(toml::Value::Array(items)) = op_table.get("prepend") {
        let mut new_entries: Vec<String> = Vec::new();
        for item in items {
            if let Some(s) = item.as_str() {
                let owned = s.to_owned();
                if !invariants.contains(&owned) {
                    new_entries.push(owned);
                }
            }
        }
        new_entries.append(invariants);
        *invariants = new_entries;
    }

    if let Some(toml::Value::Array(items)) = op_table.get("remove") {
        let to_remove: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        invariants.retain(|s| !to_remove.contains(s));
    }

    Ok(())
}

/// Parst einen Definitions-Referenz-String zu einem [`DefinitionRef`].
///
/// # Beschreibung
/// Erwartet das Format einer [`DefinitionId`] (§5). Die Version ist immer `None`.
///
/// # Fehler
/// - [`DslError::InvalidId`]: wenn `s` kein gültiges `DefinitionId`-Format hat.
fn parse_def_ref(s: &str) -> DslResult<DefinitionRef> {
    let id = DefinitionId::parse(s)?;
    Ok(DefinitionRef { id, version: None })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Version;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Erzeugt eine minimale [`RawFamilyDefinition`] mit gegebener ID und Name.
    fn make_raw(id_str: &str, name: &str) -> TestResult<RawFamilyDefinition> {
        Ok(RawFamilyDefinition {
            schema: "harwness.family/v1".to_owned(),
            id: DefinitionId::parse(id_str)?,
            version: Version(semver::Version::new(1, 0, 0)),
            extends: None,
            name: name.to_owned(),
            orchestrators: FamilyRoster::default(),
            workers: FamilyRoster::default(),
            defaults: toml::Table::new(),
            invariants: Vec::new(),
            universe: AuthorityCeiling::default(),
            patch: toml::Table::new(),
        })
    }

    /// Erzeugt eine [`DefinitionRef`] aus einem ID-String.
    fn def_ref(id_str: &str) -> TestResult<DefinitionRef> {
        Ok(DefinitionRef {
            id: DefinitionId::parse(id_str)?,
            version: None,
        })
    }

    fn now() -> OffsetDateTime {
        time::OffsetDateTime::now_utc()
    }

    // -----------------------------------------------------------------------
    // Test 1: TOML-Roundtrip einer RawFamilyDefinition
    // -----------------------------------------------------------------------

    /// Verifiziert, dass eine [`RawFamilyDefinition`] korrekt zu TOML serialisiert
    /// und wieder deserialisiert werden kann.
    #[test]
    fn raw_family_serde_roundtrip() -> TestResult {
        // TOML ohne Array-of-inline-tables (einfacher zu serialisieren).
        let src2 = r#"
schema = "harwness.family/v1"
id = "harwness.family.focused-coding@1"
version = "1.0.0"
name = "Focused Coding Family"
invariants = ["disjoint_write_sets", "workers_are_pure_implementers"]

[defaults]
context_policy = "harwness.context.coding-orchestrator@1"
"#;
        let raw: RawFamilyDefinition = toml::from_str(src2)?;
        assert_eq!(raw.schema, "harwness.family/v1");
        assert_eq!(raw.name, "Focused Coding Family");
        assert_eq!(raw.invariants.len(), 2);
        assert_eq!(
            raw.defaults["context_policy"]
                .as_str()
                .ok_or(TestError::Missing("defaults.context_policy as str"))?,
            "harwness.context.coding-orchestrator@1"
        );

        // Serialisierung und Re-Deserialisierung
        let serialized = toml::to_string(&raw).map_err(ctx("toml::to_string should succeed"))?;
        let recovered: RawFamilyDefinition = toml::from_str(&serialized)?;
        assert_eq!(recovered.name, raw.name);
        assert_eq!(recovered.invariants, raw.invariants);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 2: Fehlender target_id-Layer → MissingBase
    // -----------------------------------------------------------------------

    /// Verifiziert, dass [`resolve_family`] [`DslError::MissingBase`] zurückgibt,
    /// wenn `target_id` in keinem der Layer vorhanden ist.
    #[test]
    fn resolve_missing_base_errors() -> TestResult {
        let id = DefinitionId::parse("harwness.family.nonexistent@1")?;
        let layers: Vec<(DefinitionLayer, RawFamilyDefinition)> = vec![];
        let result = resolve_family(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::MissingBase { .. })),
            "Erwartet MissingBase, bekommen: {:?}",
            result
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 3: Kein extends — direkte Definition wird zurückgegeben
    // -----------------------------------------------------------------------

    /// Verifiziert, dass eine Familie ohne `extends` direkt aufgelöst wird
    /// und alle Felder korrekt übernommen werden.
    #[test]
    fn resolve_no_extends_returns_direct() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;
        let mut raw = make_raw(id_str, "Focused Coding Family")?;
        raw.workers
            .allowed
            .push(def_ref("harwness.agent.focused-pure-coding@1")?);
        raw.workers
            .allowed
            .push(def_ref("harwness.agent.focused-coding-planning@1")?);
        raw.invariants.push("disjoint_write_sets".to_owned());

        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.id, id);
        assert_eq!(resolved.name, "Focused Coding Family");
        assert_eq!(resolved.workers.len(), 2);
        assert_eq!(resolved.invariants, vec!["disjoint_write_sets"]);
        assert_eq!(resolved.trace.steps.len(), 1);
        assert_eq!(resolved.trace.steps[0].kind, "base");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 4: Append-Patch auf workers
    // -----------------------------------------------------------------------

    /// Verifiziert, dass `patch.workers.allowed.append` einen neuen Worker hinzufügt.
    /// Base hat 2 Worker, Patch-Layer fügt 1 hinzu → Result hat 3 Worker.
    #[test]
    fn resolve_applies_append_patch_to_workers() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        // Base-Layer: 2 Worker
        let mut base = make_raw(id_str, "Focused Coding Family")?;
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-pure-coding@1")?);
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-coding-planning@1")?);

        // Patch-Layer: append 1 Worker
        let mut patch_def = make_raw(id_str, "Focused Coding Family")?;
        let mut patch_table = toml::Table::new();
        let mut workers_table = toml::Table::new();
        let mut allowed_table = toml::Table::new();
        allowed_table.insert(
            "append".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "mia.agent.rust-pqc-pure-coder@1".to_owned(),
            )]),
        );
        workers_table.insert("allowed".to_owned(), toml::Value::Table(allowed_table));
        patch_table.insert("workers".to_owned(), toml::Value::Table(workers_table));
        patch_def.patch = patch_table;

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, patch_def),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.workers.len(), 3);
        assert!(
            resolved
                .workers
                .contains(&def_ref("mia.agent.rust-pqc-pure-coder@1")?)
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 5: Remove-Patch auf workers
    // -----------------------------------------------------------------------

    /// Verifiziert, dass `patch.workers.allowed.remove` einen Worker entfernt.
    #[test]
    fn resolve_applies_remove_patch() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        let mut base = make_raw(id_str, "Focused Coding Family")?;
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-pure-coding@1")?);
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-coding-planning@1")?);
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-docs-update@1")?);

        let mut patch_def = make_raw(id_str, "Focused Coding Family")?;
        let mut patch_table = toml::Table::new();
        let mut workers_table = toml::Table::new();
        let mut allowed_table = toml::Table::new();
        allowed_table.insert(
            "remove".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "harwness.agent.focused-docs-update@1".to_owned(),
            )]),
        );
        workers_table.insert("allowed".to_owned(), toml::Value::Table(allowed_table));
        patch_table.insert("workers".to_owned(), toml::Value::Table(workers_table));
        patch_def.patch = patch_table;

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::Project, patch_def),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.workers.len(), 2);
        assert!(
            !resolved
                .workers
                .contains(&def_ref("harwness.agent.focused-docs-update@1")?)
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 6: Append-Patch auf Invarianten
    // -----------------------------------------------------------------------

    /// Verifiziert, dass `patch.invariants.append` neue Invarianten hinzufügt.
    #[test]
    fn resolve_appends_invariants() -> TestResult {
        let id_str = "harwness.family.crypt-guard@1";
        let id = DefinitionId::parse(id_str)?;

        let mut base = make_raw(id_str, "Crypt Guard Family")?;
        base.invariants.push("disjoint_write_sets".to_owned());

        let mut patch_def = make_raw(id_str, "Crypt Guard Family")?;
        let mut patch_table = toml::Table::new();
        let mut inv_table = toml::Table::new();
        inv_table.insert(
            "append".to_owned(),
            toml::Value::Array(vec![
                toml::Value::String("secret_types_never_clone".to_owned()),
                toml::Value::String("zeroize_on_drop".to_owned()),
            ]),
        );
        patch_table.insert("invariants".to_owned(), toml::Value::Table(inv_table));
        patch_def.patch = patch_table;

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::Workspace, patch_def),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.invariants.len(), 3);
        assert!(
            resolved
                .invariants
                .contains(&"secret_types_never_clone".to_owned())
        );
        assert!(resolved.invariants.contains(&"zeroize_on_drop".to_owned()));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 7: Layer-Reihenfolge respektiert Priorität
    // -----------------------------------------------------------------------

    /// Verifiziert, dass bei unsortierter Eingabe die Layer-Priorisierung
    /// korrekt ist: höherer Layer (RunLocal) überschreibt niedrigeren (BuiltIn).
    #[test]
    fn resolve_layer_ordering_respects_priority() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        let mut base = make_raw(id_str, "Focused Coding Family")?;
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-pure-coding@1")?);

        // Patch-Layer mit höherer Priorität fügt Worker hinzu.
        let mut patch_def = make_raw(id_str, "Focused Coding Family")?;
        let mut patch_table = toml::Table::new();
        let mut workers_table = toml::Table::new();
        let mut allowed_table = toml::Table::new();
        allowed_table.insert(
            "append".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "harwness.agent.focused-verification@1".to_owned(),
            )]),
        );
        workers_table.insert("allowed".to_owned(), toml::Value::Table(allowed_table));
        patch_table.insert("workers".to_owned(), toml::Value::Table(workers_table));
        patch_def.patch = patch_table;

        // Eingabe in umgekehrter Reihenfolge — RunLocal zuerst, BuiltIn zuletzt.
        let layers = vec![
            (DefinitionLayer::RunLocal, patch_def),
            (DefinitionLayer::BuiltIn, base),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.workers.len(), 2);
        assert!(
            resolved
                .workers
                .contains(&def_ref("harwness.agent.focused-verification@1")?)
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 8: Trace enthält alle Schritte
    // -----------------------------------------------------------------------

    /// Verifiziert, dass der [`ResolutionTrace`] bei Base + 1 Patch-Layer
    /// genau 2 Schritte enthält.
    #[test]
    fn resolve_trace_contains_all_steps() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        let base = make_raw(id_str, "Focused Coding Family")?;

        let mut patch_def = make_raw(id_str, "Focused Coding Family")?;
        let mut patch_table = toml::Table::new();
        let mut inv_table = toml::Table::new();
        inv_table.insert(
            "append".to_owned(),
            toml::Value::Array(vec![toml::Value::String("test_invariant".to_owned())]),
        );
        patch_table.insert("invariants".to_owned(), toml::Value::Table(inv_table));
        patch_def.patch = patch_table;

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, patch_def),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.trace.steps.len(), 2);
        assert_eq!(resolved.trace.steps[0].kind, "base");
        assert_eq!(resolved.trace.steps[1].kind, "patch");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 9: Name wird vom höchsten Layer überschrieben
    // -----------------------------------------------------------------------

    /// Verifiziert, dass `patch.name.replace` den Namen der Familie überschreiben kann.
    #[test]
    fn family_name_taken_from_topmost_layer() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        let base = make_raw(id_str, "Ursprünglicher Name")?;

        let mut patch_def = make_raw(id_str, "Ursprünglicher Name")?;
        let mut patch_table = toml::Table::new();
        let mut name_table = toml::Table::new();
        name_table.insert(
            "replace".to_owned(),
            toml::Value::String("Überschriebener Name".to_owned()),
        );
        patch_table.insert("name".to_owned(), toml::Value::Table(name_table));
        patch_def.patch = patch_table;

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::Project, patch_def),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.name, "Überschriebener Name");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 10: Standard-Roster ist leer
    // -----------------------------------------------------------------------

    /// Verifiziert, dass eine [`FamilyRoster`]-Instanz ohne explizite Konfiguration
    /// eine leere `allowed`-Liste hat.
    #[test]
    fn raw_default_roster_is_empty() -> TestResult {
        let src = r#"
schema = "harwness.family/v1"
id = "harwness.family.minimal@1"
version = "1.0.0"
name = "Minimal Family"
"#;
        let raw: RawFamilyDefinition = toml::from_str(src)?;
        assert!(raw.orchestrators.allowed.is_empty());
        assert!(raw.workers.allowed.is_empty());
        assert!(raw.invariants.is_empty());
        assert!(raw.defaults.is_empty());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 11: Multi-Layer-Komposition — 3 Layer, jede fügt einen Worker hinzu
    // -----------------------------------------------------------------------

    /// Verifiziert, dass 3 aufeinanderfolgende Layer jeweils einen Worker hinzufügen
    /// und das Ergebnis alle 3 Worker enthält.
    #[test]
    fn resolve_multi_layer_composition() -> TestResult {
        let id_str = "harwness.family.focused-coding@1";
        let id = DefinitionId::parse(id_str)?;

        // Layer 1 (BuiltIn): 1 Worker
        let mut base = make_raw(id_str, "Focused Coding Family")?;
        base.workers
            .allowed
            .push(def_ref("harwness.agent.focused-pure-coding@1")?);

        // Layer 2 (UserGlobal): append 1 Worker
        let patch_def2 = {
            let mut def = make_raw(id_str, "Focused Coding Family")?;
            let mut patch_table = toml::Table::new();
            let mut workers_table = toml::Table::new();
            let mut allowed_table = toml::Table::new();
            allowed_table.insert(
                "append".to_owned(),
                toml::Value::Array(vec![toml::Value::String(
                    "harwness.agent.focused-coding-planning@1".to_owned(),
                )]),
            );
            workers_table.insert("allowed".to_owned(), toml::Value::Table(allowed_table));
            patch_table.insert("workers".to_owned(), toml::Value::Table(workers_table));
            def.patch = patch_table;
            def
        };

        // Layer 3 (Workspace): append 1 weiteren Worker
        let patch_def3 = {
            let mut def = make_raw(id_str, "Focused Coding Family")?;
            let mut patch_table = toml::Table::new();
            let mut workers_table = toml::Table::new();
            let mut allowed_table = toml::Table::new();
            allowed_table.insert(
                "append".to_owned(),
                toml::Value::Array(vec![toml::Value::String(
                    "mia.agent.rust-pqc-pure-coder@1".to_owned(),
                )]),
            );
            workers_table.insert("allowed".to_owned(), toml::Value::Table(allowed_table));
            patch_table.insert("workers".to_owned(), toml::Value::Table(workers_table));
            def.patch = patch_table;
            def
        };

        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, patch_def2),
            (DefinitionLayer::Workspace, patch_def3),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(resolved.workers.len(), 3);
        assert!(
            resolved
                .workers
                .contains(&def_ref("harwness.agent.focused-pure-coding@1")?)
        );
        assert!(
            resolved
                .workers
                .contains(&def_ref("harwness.agent.focused-coding-planning@1")?)
        );
        assert!(
            resolved
                .workers
                .contains(&def_ref("mia.agent.rust-pqc-pure-coder@1")?)
        );
        // Trace: 1 base + 2 patches
        assert_eq!(resolved.trace.steps.len(), 3);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Knoten AW6-01 — `universe`-Feld: Deserialisierung
    // -----------------------------------------------------------------------

    /// Fehlt `[universe]` in der TOML-Quelle, liefert `#[serde(default)]` eine
    /// leere [`AuthorityCeiling`] — rein additiv, bestehende Family-Dateien
    /// ohne `[universe]` bleiben gültig.
    #[test]
    fn universe_defaults_to_empty_when_omitted() -> TestResult {
        let src = r#"
schema = "harwness.family/v1"
id = "harwness.family.minimal@1"
version = "1.0.0"
name = "Minimal Family"
"#;
        let raw: RawFamilyDefinition = toml::from_str(src)?;
        assert!(raw.universe.capabilities.is_empty());
        Ok(())
    }

    /// Ein deklariertes `[universe]` wird korrekt zu [`AuthorityCeiling`] deserialisiert.
    #[test]
    fn universe_declared_explicitly_parses() -> TestResult {
        let src = r#"
schema = "harwness.family/v1"
id = "harwness.family.security@1"
version = "1.0.0"
name = "Security Family"

[universe]
capabilities = ["security.sensor.read", "security.verdict.propose"]
"#;
        let raw: RawFamilyDefinition = toml::from_str(src)?;
        assert_eq!(
            raw.universe.capabilities,
            vec![
                "security.sensor.read".to_owned(),
                "security.verdict.propose".to_owned()
            ]
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Knoten AW6-01 — `universe`-Feld: Auflösung über `resolve_family`
    // -----------------------------------------------------------------------

    /// Ohne `extends` ist `resolved.universe` genau die deklarierte Menge der
    /// Basis-Definition.
    #[test]
    fn resolve_no_extends_carries_universe_through() -> TestResult {
        let id_str = "harwness.family.security@1";
        let id = DefinitionId::parse(id_str)?;
        let mut raw = make_raw(id_str, "Security Family")?;
        raw.universe = AuthorityCeiling {
            capabilities: vec!["security.sensor.read".to_owned()],
        };

        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(
            resolved.universe.capabilities,
            vec!["security.sensor.read".to_owned()]
        );
        Ok(())
    }

    /// `extends`, Kind erklärt kein eigenes `[universe]` (leere Menge) → das
    /// `universe` der Basis wird unverändert übernommen, nicht auf leer
    /// kollabiert.
    #[test]
    fn resolve_extends_inherits_parent_universe_when_child_declares_none() -> TestResult {
        let base_id = "harwness.family.security-base@1";
        let derived_id = "harwness.family.security-derived@1";

        let mut base = make_raw(base_id, "Security Base")?;
        base.universe = AuthorityCeiling {
            capabilities: vec![
                "security.sensor.read".to_owned(),
                "security.context.read".to_owned(),
            ],
        };

        let mut derived = make_raw(derived_id, "Security Derived")?;
        derived.extends = Some(def_ref(base_id)?);

        let id = DefinitionId::parse(derived_id)?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, derived),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        let mut caps = resolved.universe.capabilities.clone();
        caps.sort();
        assert_eq!(
            caps,
            vec![
                "security.context.read".to_owned(),
                "security.sensor.read".to_owned()
            ]
        );
        Ok(())
    }

    /// `extends`, Kind erklärt ein ENGERES `[universe]` → das Ergebnis ist der
    /// Schnitt (nicht die Vereinigung) aus Basis- und Kind-`universe`. Ein
    /// `append`-Merge hier wäre eine stille Möglichkeit, die
    /// Disjunktheits-Zusicherung zweier Familien über `extends` zu brechen.
    #[test]
    fn resolve_extends_intersects_when_child_declares_narrower_universe() -> TestResult {
        let base_id = "harwness.family.security-base2@1";
        let derived_id = "harwness.family.security-derived2@1";

        let mut base = make_raw(base_id, "Security Base 2")?;
        base.universe = AuthorityCeiling {
            capabilities: vec![
                "security.sensor.read".to_owned(),
                "security.context.read".to_owned(),
                "security.verdict.propose".to_owned(),
            ],
        };

        let mut derived = make_raw(derived_id, "Security Derived 2")?;
        derived.extends = Some(def_ref(base_id)?);
        derived.universe = AuthorityCeiling {
            capabilities: vec![
                "security.context.read".to_owned(),
                "security.verdict.propose".to_owned(),
                "security.advisory.correlate".to_owned(), // NICHT in der Basis — darf nicht durchschlagen
            ],
        };

        let id = DefinitionId::parse(derived_id)?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, derived),
        ];
        let resolved = resolve_family(&id, &layers, now())?;

        assert_eq!(
            resolved.universe.capabilities,
            vec![
                "security.context.read".to_owned(),
                "security.verdict.propose".to_owned()
            ],
            "das Ergebnis muss der Schnitt sein: 'security.advisory.correlate' \
             war nicht in der Basis und darf die effektive Menge nicht erweitern"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Knoten AW6-01 — Disjunktheit der `universe`-Mengen ALLER realen Families
    // -----------------------------------------------------------------------
    //
    // Diese drei TOML-Dateien werden über `include_str!` eingebettet — genau
    // wie `harw-agent-dsl/tests/fixtures/*.toml` — nicht zur Laufzeit gelesen.
    // `harw-agent-dsl` bekommt dadurch KEINE neue Cargo-Abhängigkeit auf
    // `harw-registry-defaults` (umgekehrt wäre das ohnehin ein Zyklus:
    // `harw-registry-defaults` hängt bereits von `harw-agent-dsl` ab) — es
    // werden nur Daten-Dateien gelesen, keine Rust-Typen importiert.
    const RESEARCH_FAMILY_TOML: &str =
        include_str!("../../harw-registry-defaults/agents/family/research.toml");
    const CODING_FAMILY_TOML: &str =
        include_str!("../../harw-registry-defaults/agents/family/coding.toml");
    const SECURITY_FAMILY_TOML: &str =
        include_str!("../../harw-registry-defaults/agents/families/security/security.toml");

    /// Die tragende Zusicherung von Knoten AW6-01: die `universe`-Mengen aller
    /// eingebauten Families sind paarweise disjunkt.
    ///
    /// Vor Knoten AW6-01 deklarieren `family/research` und `family/coding`
    /// kein `[universe]` (leere Menge per Default) — der Schnitt mit JEDER
    /// anderen Menge ist dann trivial leer. Das prüft `security`s `universe`
    /// heute real gegen zwei leere Mengen; die eigentliche Beißkraft des
    /// Prädikats selbst ist in `authority.rs::test_is_disjoint_from_false_for_deliberately_constructed_overlap`
    /// bewiesen (dort wird absichtlich eine Überschneidung konstruiert und
    /// verifiziert, dass die Prüfung sie erkennt) — dieser Test hier belegt
    /// nur, dass die REALEN, ausgelieferten Definitionen die Zusicherung
    /// heute erfüllen, nicht, dass das Prädikat scharf ist.
    #[test]
    fn all_real_registry_family_universes_are_pairwise_disjoint() -> TestResult {
        let research: RawFamilyDefinition = toml::from_str(RESEARCH_FAMILY_TOML)
            .map_err(ctx("family/research.toml muss parsen"))?;
        let coding: RawFamilyDefinition =
            toml::from_str(CODING_FAMILY_TOML).map_err(ctx("family/coding.toml muss parsen"))?;
        let security: RawFamilyDefinition = toml::from_str(SECURITY_FAMILY_TOML)
            .map_err(ctx("families/security/security.toml muss parsen"))?;

        let families = [
            ("family/research", &research.universe),
            ("family/coding", &coding.universe),
            ("families/security/security", &security.universe),
        ];

        for i in 0..families.len() {
            for j in (i + 1)..families.len() {
                let (name_a, universe_a) = &families[i];
                let (name_b, universe_b) = &families[j];
                assert!(
                    universe_a.is_disjoint_from(universe_b),
                    "universe von '{name_a}' und '{name_b}' ueberschneiden sich: \
                     {:?} vs {:?}",
                    universe_a.capabilities,
                    universe_b.capabilities
                );
            }
        }
        Ok(())
    }

    /// Die Security-Familie deklariert ein nicht-leeres `universe` — sonst
    /// wäre der Disjunktheitstest oben für sie trivial und würde nichts
    /// beweisen.
    #[test]
    fn security_family_declares_a_nonempty_universe() -> TestResult {
        let security: RawFamilyDefinition = toml::from_str(SECURITY_FAMILY_TOML)
            .map_err(ctx("families/security/security.toml muss parsen"))?;
        assert!(
            !security.universe.capabilities.is_empty(),
            "die Security-Familie muss ein nicht-leeres universe deklarieren, \
             sonst ist die Disjunktheit trivial und ungeprüft"
        );
        Ok(())
    }

    /// AW6-03 ist gelandet: das Worker-Roster der Security-Familie ist nicht
    /// mehr leer, sondern nennt die vier Triage-Rollendateien unter
    /// `agents/roles/security-*/`. Der Vorgänger-Test behauptete
    /// `resolved.workers.is_empty()` und sagte im eigenen Kommentar voraus,
    /// dass genau das eintreten würde — diese Fassung prüft jetzt eine
    /// Invariante statt eine (überholte) Zahl oder einen (überholten)
    /// Leerzustand.
    ///
    /// # Warum eine Menge statt einer Zahl
    /// Der Vergleichswert kommt aus der EINEN Seite — den vier
    /// Rollendateien, hier über `include_str!` + `parse_toml` als
    /// tatsächliche `DefinitionId`s gelesen — und wird gegen die ANDERE Seite
    /// geprüft: `[workers].allowed` aus `security.toml`, über
    /// `resolve_family` aufgelöst. Eine feste Zahl `4` neben der Liste, die
    /// sie zählt, wäre keine Prüfung, sondern eine Wiederholung — sie würde
    /// beim nächsten Bewohner (`context-steward`/`intel-scout`, sobald deren
    /// Mitgliedschaft zusammengeführt ist) wieder rot, ohne zu sagen, WAS
    /// falsch ist. Diese Fassung bricht dagegen genau dann, wenn das Roster
    /// eine existierende Rollendatei vergisst oder eine erfundene ID nennt,
    /// die zu keiner Rollendatei gehört — unabhängig davon, wie viele
    /// Rollendateien es gerade gibt.
    ///
    /// `orchestrators.allowed` bleibt weiterhin tatsächlich leer (keine
    /// Mitgliedschaft trägt `as_orchestrator = true` in diese Familie) — das
    /// ist ein weiterhin zutreffender Fakt dieser Datei, keine überholte
    /// Vorhersage wie zuvor beim Worker-Roster.
    #[test]
    fn security_family_workers_roster_matches_the_declared_triage_role_files() -> TestResult {
        const EGRESS_TRIAGE_TOML: &str = include_str!(
            "../../harw-registry-defaults/agents/roles/security-egress-triage/security-egress-triage.toml"
        );
        const BASELINE_TRIAGE_TOML: &str = include_str!(
            "../../harw-registry-defaults/agents/roles/security-baseline-triage/security-baseline-triage.toml"
        );
        const STRUCTURE_TRIAGE_TOML: &str = include_str!(
            "../../harw-registry-defaults/agents/roles/security-structure-triage/security-structure-triage.toml"
        );
        const ENDPOINT_TRIAGE_TOML: &str = include_str!(
            "../../harw-registry-defaults/agents/roles/security-endpoint-triage/security-endpoint-triage.toml"
        );

        // Die EINE Seite: die tatsächlichen Rollendateien, unabhängig von
        // `security.toml` eingelesen.
        let declared_role_ids: std::collections::HashSet<DefinitionId> = [
            EGRESS_TRIAGE_TOML,
            BASELINE_TRIAGE_TOML,
            STRUCTURE_TRIAGE_TOML,
            ENDPOINT_TRIAGE_TOML,
        ]
        .into_iter()
        .map(|source| {
            crate::parse::parse_toml(source)
                .map_err(ctx("jede Security-Triage-Rollendatei muss parsen"))
                .map(|raw| raw.id)
        })
        .collect::<TestResult<_>>()?;

        // Die ANDERE Seite: das aufgelöste Roster aus `security.toml`.
        let id = DefinitionId::parse("harwness.family.security@1")?;
        let raw: RawFamilyDefinition =
            toml::from_str(SECURITY_FAMILY_TOML).map_err(ctx("security.toml muss parsen"))?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_family(&id, &layers, now())?;

        let roster_ids: std::collections::HashSet<DefinitionId> = resolved
            .workers
            .iter()
            .map(|reference| reference.id.clone())
            .collect();

        assert_eq!(
            roster_ids, declared_role_ids,
            "das Worker-Roster muss exakt die vorhandenen Security-Triage-Rollendateien \
             nennen — weder eine erfundene ID noch eine vergessene Rollendatei"
        );

        assert!(
            resolved.orchestrators.is_empty(),
            "orchestrators.allowed ist heute leer, weil keine Mitgliedschaft \
             as_orchestrator = true trägt — kein vorhergesagter Zustand, ein \
             aktueller Fakt dieser Datei"
        );
        Ok(())
    }

    /// Die Security-Familie löst mit der erwarteten `id` und dem erwarteten
    /// `return_contract`-Default auf. Ein direkter Test der
    /// Verzeichniskonvention selbst (`harw_registry_defaults::embedded_agents`)
    /// liegt außerhalb des Schreibbereichs dieses Knotens
    /// (`harw-registry-defaults/src/**`) — dieser Test verifiziert stattdessen
    /// die Namensableitungsregel manuell gegen den literalen Pfad, an dem die
    /// Datei liegt, und dass ihr Inhalt vollständig auflöst.
    #[test]
    fn security_family_resolves_with_expected_id_and_derived_name() -> TestResult {
        // Namensableitung laut `harw_registry_defaults::embedded_agents`
        // Modul-Dokumentation: "der volle Pfad relativ zu `agents/`, ohne
        // `.toml`". Dieser Test bildet die Regel nach, statt den echten Loader
        // aufzurufen (der liegt in einer Crate außerhalb des Schreibbereichs
        // dieses Knotens).
        let relative_path = "families/security/security.toml";
        let derived_name = relative_path
            .strip_suffix(".toml")
            .ok_or(TestError::Missing("'.toml' suffix on relative_path"))?;
        assert_eq!(derived_name, "families/security/security");

        let id = DefinitionId::parse("harwness.family.security@1")?;
        let raw: RawFamilyDefinition =
            toml::from_str(SECURITY_FAMILY_TOML).map_err(ctx("security.toml muss parsen"))?;
        assert_eq!(raw.id, id);

        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_family(&id, &layers, now())?;
        assert_eq!(
            resolved
                .defaults
                .get("return_contract")
                .and_then(|v| v.as_str()),
            Some("harwness.security-verdict/v1")
        );
        assert_eq!(resolved.trace.steps.len(), 1);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Knoten AW6-02 — Mitgliedschaft: eine Rolle tritt bei, ohne die
    // Familiendatei zu ändern.
    // -----------------------------------------------------------------------

    /// Baut eine minimal aufgelöste Familie mit gegebenem `universe`, ohne
    /// TOML zu parsen — Hilfsfunktion nur für die Mitgliedschafts-Tests.
    fn resolved_with_universe(id_str: &str, universe_caps: &[&str]) -> TestResult<ResolvedFamily> {
        let mut raw = make_raw(id_str, "Security Family")?;
        raw.universe = AuthorityCeiling {
            capabilities: universe_caps.iter().map(|s| (*s).to_owned()).collect(),
        };
        let id = DefinitionId::parse(id_str)?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        Ok(resolve_family(&id, &layers, now())?)
    }

    /// Baut einen Mitgliedschaftsantrag für Tests.
    fn membership(
        family_id: &str,
        role_id: &str,
        as_orchestrator: bool,
        caps: &[&str],
    ) -> TestResult<RawFamilyMembership> {
        Ok(RawFamilyMembership {
            schema: "harwness.family-membership/v1".to_owned(),
            family: def_ref(family_id)?,
            role: def_ref(role_id)?,
            as_orchestrator,
            capabilities: AuthorityCeiling {
                capabilities: caps.iter().map(|s| (*s).to_owned()).collect(),
            },
        })
    }

    /// `RawFamilyMembership` parst per TOML-Roundtrip mit den erwarteten Defaults.
    #[test]
    fn family_membership_toml_roundtrip_has_expected_defaults() -> TestResult {
        let src = r#"
schema = "harwness.family-membership/v1"
family = "harwness.family.security@1"
role = "harwness.agent.security-triage-1@1"
"#;
        let parsed: RawFamilyMembership = toml::from_str(src)?;
        assert!(!parsed.as_orchestrator);
        assert!(parsed.capabilities.capabilities.is_empty());
        assert_eq!(
            parsed.family.id,
            DefinitionId::parse("harwness.family.security@1")?
        );
        Ok(())
    }

    /// Eine Mitgliedschaft, deren Capabilities vollständig im `universe`
    /// liegen, wird zugelassen.
    #[test]
    fn admit_family_member_accepts_capabilities_within_universe() -> TestResult {
        let family = resolved_with_universe(
            "harwness.family.security@1",
            &["security.sensor.read", "security.verdict.propose"],
        )?;
        let request = membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &["security.sensor.read"],
        )?;
        assert!(admit_family_member(&family, &request).is_ok());
        Ok(())
    }

    /// Der wichtigste Test dieses Knotens: eine Rolle mit einer Fähigkeit
    /// außerhalb des `universe` wird abgelehnt — mit `AuthorityElevation` und
    /// der beitretenden Rolle als `of`.
    #[test]
    fn admit_family_member_rejects_capability_outside_universe() -> TestResult {
        let family =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        let request = membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &["security.sensor.read", "filesystem.write"],
        )?;
        let result = admit_family_member(&family, &request);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "Capability außerhalb des universe muss abgelehnt werden".into(),
            ));
        };
        match error {
            DslError::AuthorityElevation {
                of,
                added_capabilities,
                ..
            } => {
                assert_eq!(
                    *of,
                    DefinitionId::parse("harwness.agent.security-triage-1@1")?
                );
                assert_eq!(added_capabilities, vec!["filesystem.write".to_owned()]);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet AuthorityElevation, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Eine Familie ohne `[universe]` (leere Menge) lässt keine Mitgliedschaft
    /// mit nichtleeren Capabilities zu — eine leere Autoritätsgrenze heißt
    /// "nichts ist geprüft worden", nicht "alles ist erlaubt".
    #[test]
    fn admit_family_member_rejects_any_capability_when_universe_is_empty() -> TestResult {
        let family = resolved_with_universe("harwness.family.security@1", &[])?;
        let request = membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &["security.sensor.read"],
        )?;
        assert!(admit_family_member(&family, &request).is_err());
        Ok(())
    }

    /// Eine Mitgliedschaft ohne beanspruchte Capabilities ist immer eine
    /// Teilmenge — auch eines leeren `universe`.
    #[test]
    fn admit_family_member_accepts_empty_capabilities_against_empty_universe() -> TestResult {
        let family = resolved_with_universe("harwness.family.security@1", &[])?;
        let request = membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &[],
        )?;
        assert!(admit_family_member(&family, &request).is_ok());
        Ok(())
    }

    /// `merge_memberships_into` nimmt eine zulässige Rolle ins Worker-Roster
    /// auf, ohne dass die Familiendatei selbst geändert wurde (`resolved` kam
    /// aus einer Familie mit leerem Roster).
    #[test]
    fn merge_memberships_into_admits_role_without_touching_family_file() -> TestResult {
        let mut family =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        assert!(family.workers.is_empty(), "Vorbedingung: leeres Roster");

        let memberships = vec![membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &["security.sensor.read"],
        )?];
        merge_memberships_into(&mut family, &memberships)?;

        assert_eq!(
            family.workers,
            vec![def_ref("harwness.agent.security-triage-1@1")?]
        );
        Ok(())
    }

    /// Ein Mitgliedschaftsantrag für eine andere Familie wird übersprungen.
    #[test]
    fn merge_memberships_into_skips_membership_for_other_family() -> TestResult {
        let mut family =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        let memberships = vec![membership(
            "harwness.family.coding@1",
            "harwness.agent.some-coder@1",
            false,
            &[],
        )?];
        merge_memberships_into(&mut family, &memberships)?;
        assert!(family.workers.is_empty());
        assert!(family.orchestrators.is_empty());
        Ok(())
    }

    /// Eine Mitgliedschaft mit `as_orchestrator = true` landet im
    /// Orchestrator-Roster, nicht im Worker-Roster.
    #[test]
    fn merge_memberships_into_respects_as_orchestrator_flag() -> TestResult {
        let mut family =
            resolved_with_universe("harwness.family.security@1", &["security.verdict.propose"])?;
        let memberships = vec![membership(
            "harwness.family.security@1",
            "harwness.agent.security-lead@1",
            true,
            &["security.verdict.propose"],
        )?];
        merge_memberships_into(&mut family, &memberships)?;
        assert_eq!(
            family.orchestrators,
            vec![def_ref("harwness.agent.security-lead@1")?]
        );
        assert!(family.workers.is_empty());
        Ok(())
    }

    /// Mehrere zulässige Mitgliedschaften werden additiv und in der
    /// übergebenen Reihenfolge angehängt (Reihenfolgestabilität).
    #[test]
    fn merge_memberships_into_appends_multiple_in_given_order() -> TestResult {
        let mut family = resolved_with_universe(
            "harwness.family.security@1",
            &["security.sensor.read", "security.context.read"],
        )?;
        let memberships = vec![
            membership(
                "harwness.family.security@1",
                "harwness.agent.security-triage-1@1",
                false,
                &["security.sensor.read"],
            )?,
            membership(
                "harwness.family.security@1",
                "harwness.agent.context-steward@1",
                false,
                &["security.context.read"],
            )?,
        ];
        merge_memberships_into(&mut family, &memberships)?;
        assert_eq!(
            family.workers,
            vec![
                def_ref("harwness.agent.security-triage-1@1")?,
                def_ref("harwness.agent.context-steward@1")?,
            ]
        );
        Ok(())
    }

    /// Eine abgelehnte Mitgliedschaft bricht `merge_memberships_into` ab und
    /// ändert `resolved.universe` nicht.
    #[test]
    fn merge_memberships_into_never_changes_universe() -> TestResult {
        let mut family =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        let original_universe = family.universe.clone();
        let memberships = vec![membership(
            "harwness.family.security@1",
            "harwness.agent.security-triage-1@1",
            false,
            &["security.sensor.read", "filesystem.write"],
        )?];
        let result = merge_memberships_into(&mut family, &memberships);
        assert!(result.is_err());
        assert_eq!(family.universe, original_universe);
        Ok(())
    }

    /// Repeated resolution (zweimaliges Laden derselben Family + Memberships)
    /// liefert dasselbe Roster in derselben Reihenfolge — Voraussetzung für
    /// stabile `SnapshotId`/Golden-Tests stromabwärts.
    #[test]
    fn merge_memberships_into_is_stable_across_repeated_resolution() -> TestResult {
        let memberships = vec![
            membership(
                "harwness.family.security@1",
                "harwness.agent.security-triage-1@1",
                false,
                &["security.sensor.read"],
            )?,
            membership(
                "harwness.family.security@1",
                "harwness.agent.security-triage-2@1",
                false,
                &["security.sensor.read"],
            )?,
        ];

        let mut first =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        merge_memberships_into(&mut first, &memberships)?;

        let mut second =
            resolved_with_universe("harwness.family.security@1", &["security.sensor.read"])?;
        merge_memberships_into(&mut second, &memberships)?;

        assert_eq!(first.workers, second.workers);
        Ok(())
    }
}
