//! Kontextprogramm-Definitionen: Grammatik, Auflösungskaskade und Deckenprüfung.
//!
//! Dieses Modul ist Knoten **AW2-01**. Es beantwortet die Frage, die
//! [`crate::executable::ContextProgram`] offen lässt: dort **existiert** bereits
//! ein `context_policy`-Label plus `must_include`/`exclude`-Selektoren auf der
//! Executable-IR-Ebene (siehe `executable.rs`), aber niemand kann ein solches
//! Kontextprogramm bislang *deklarieren*. Dieses Modul liefert die TOML-Grammatik
//! (§ Grammatik unten) und die Auflösungskaskade dafür — als eigene Definitionsklasse
//! `harwness.context.<name>@<v>`, analog zu [`crate::family`] und
//! [`crate::organization`].
//!
//! # Verantwortungsbereich
//! Besitzt:
//! - [`RawContextProgramDefinition`] — TOML-nahe Rohstruktur eines Kontextprogramms
//! - [`RawContextSectionSpec`] — eine einzelne `[[sections]]`-Deklaration
//! - [`SectionStrength`] — „normal" oder „must-include" je Sektion
//! - [`ResolvedContextProgramDefinition`] — aufgelöstes Kontextprogramm (IR)
//! - [`resolve_context_program`] — die vierstufige Auflösungskaskade (§4, `extends`,
//!   Mixins, Patch) für Kontextprogramme
//! - [`ContextCeilingAdmission`] — Erweiterungstrait, der ein aufgelöstes Programm
//!   gegen eine [`harw_context::ContextCeiling`] prüft
//! - [`ProgramCeilingViolation`], [`ContextProgramError`] — Fehlertypen dieses Moduls
//!
//! Delegiert bewusst NICHT:
//! - die tatsächliche Fragment-Montage (welche Fragmente ein Budget ausschöpfen,
//!   wie eine Sektion tatsächlich gerendert wird) — das bleibt `Assembly<...>` in
//!   `harw-core` (Knoten AW1-03).
//!
//! ## Verdrahtung in die Executable-IR
//! [`crate::executable::ContextProgram`] bekommt über
//! [`crate::executable::ContextProgram::from_resolved_program`] einen zusätzlichen,
//! rein additiven Konstruktor (kein neues Feld, keine Änderung an
//! `crate::executable`s privater `compute_snapshot_id`-Funktion): er prüft ein
//! [`ResolvedContextProgramDefinition`] über [`ContextCeilingAdmission`] gegen
//! eine Decke und bildet nur dessen `must-include`-Sektionen sowie `exclude` ab.
//! [`crate::executable::ContextProgram`] hat (Stand dieses Knotens) kein Feld für
//! `detail`/`trust` je Sektion oder für eine `Normal`-starke Sektion, die nicht
//! ausgeschlossen ist — solche Sektionen fließen zwar weiterhin in die
//! Deckenprüfung ein (ein zu weitreichendes Programm wird also trotzdem
//! abgewiesen), tauchen aber sonst nicht im Ergebnis auf. Sie vollständig zu
//! tragen, verlangt neue Felder auf `ContextProgram` plus eine Anhebung von
//! dessen `SNAPSHOT_HASH_DOMAIN` — eine bewusste Restrukturierung mit Wirkung
//! auf alle bestehenden Snapshots, die dieser Knoten nicht vornimmt (siehe
//! Abschlussbericht).
//!
//! # Grammatik
//!
//! Ein Kontextprogramm ist ein TOML-Dokument mit **festen** Top-Level-Feldern
//! (`deny_unknown_fields` — Kontextprogramme sind Definitionen, keine offenen
//! Erweiterungsformate wie `RawAgentDefinition`):
//!
//! | Feld | Typ | Pflicht | Bedeutung |
//! |---|---|---|---|
//! | `schema` | `String` | ja | Format-Tag, z. B. `"harwness.context/v1"`. |
//! | `id` | [`DefinitionId`] | ja | Namensraum-ID, `kind` konventionell `"context"`. |
//! | `version` | [`Version`] | ja | Semantische Vollversion. |
//! | `extends` | `Option<`[`DefinitionRef`]`>` | nein | Eine Basis-Definition (Single Inheritance, §6). |
//! | `mixins` | `Vec<`[`DefinitionRef`]`>` | nein | Wiederverwendbare Sektions-Bündel, in Deklarationsreihenfolge angewandt. |
//! | `sections` | `Vec<`[`RawContextSectionSpec`]`>` | nein | `[[sections]]`-Array; **Reihenfolge ist bedeutungstragend** (§ unten). |
//! | `exclude` | `Vec<String>` | nein | Selektor-Muster, die nie in den Kontext gelangen dürfen. |
//! | `patch` | `toml::Table` | nein | Merge-Patches für Vererbung (§7): `patch.sections.*`, `patch.exclude.*`. |
//!
//! Jede `[[sections]]`-Tabelle ([`RawContextSectionSpec`]) trägt:
//!
//! | Feld | Typ | Default | Bedeutung |
//! |---|---|---|---|
//! | `name` | `String` | — (Pflicht) | Sektionsname, z. B. `"history.tail"`. Wird bei der Deckenprüfung als [`harw_context::SectionName`] validiert. |
//! | `strength` | [`SectionStrength`] | `"normal"` | `"normal"` oder `"must-include"`. |
//! | `detail` | [`harw_context::DetailMode`] | `"summary"` | `"full"`, `"summary"` oder `"references"`. |
//! | `trust` | [`harw_context::TrustClass`] | `"data"` | Welche Vertrauensklasse diese Sektion mindestens beansprucht — `"instruction"`, `"evidence"` oder `"data"` (die geringste Anforderung, deshalb der Default). |
//!
//! ## Warum die Reihenfolge zählt
//! `sections` wird **weder sortiert noch dedupliziert** — die Deklarationsreihenfolge
//! bestimmt später die Aufnahmereihenfolge in der Montage (genau wie
//! `ContextProgram::must_include`/`exclude` auf der Executable-IR-Ebene, siehe
//! `executable.rs`). [`resolve_context_program`] erhält diese Reihenfolge über
//! `extends` → Mixins → eigene Sektionen → Patches hinweg.
//!
//! ## Vollständiges Beispiel
//! **Achtung TOML-Fallstrick:** ein Array-of-Tables (`[[sections]]`) bleibt
//! „offen", bis der nächste Tabellen-Header erscheint. `exclude = [...]` muss
//! deshalb **vor** dem ersten `[[sections]]`-Block stehen — ein nachgestelltes
//! bare `exclude = [...]` würde sonst stillschweigend als unbekanntes Feld der
//! zuletzt geöffneten `[[sections]]`-Tabelle geparst und dort wegen
//! `#[serde(deny_unknown_fields)]` auf [`RawContextSectionSpec`] abgewiesen.
//! Dieses Beispiel ist deshalb **kein** reiner ```toml-Fließtext mehr, sondern
//! wortgleich als Fixture im Testmodul hinterlegt
//! (`tests::full_example_from_module_doc_parses`) — es wird bei jedem Testlauf
//! tatsächlich geparst, damit dieser Fallstrick nicht erneut unbemerkt
//! hineinrutscht.
//! ```toml
//! schema = "harwness.context/v1"
//! id = "harwness.context.security-triage@1"
//! version = "1.0.0"
//! extends = { id = "harwness.context.base@1" }
//! mixins = [{ id = "harwness.context.mixin.audit-trail@1" }]
//!
//! # `exclude` steht **vor** den `[[sections]]`: in TOML gehört ein
//! # nachgestelltes Feld zur letzten Tabelle, und `deny_unknown_fields` weist
//! # es dann als unbekanntes Sektionsfeld ab.
//! exclude = ["memory.*", "plan.other-clans/**"]
//!
//! [[sections]]
//! name = "goal.invariants"
//! strength = "must-include"
//! detail = "full"
//! trust = "instruction"
//!
//! [[sections]]
//! name = "history.tail"
//! detail = "summary"
//!
//! [[sections]]
//! name = "plan.current"
//! strength = "must-include"
//! detail = "references"
//!
//! [patch.sections]
//! append = [
//!     { name = "goal.invariants", strength = "must-include", detail = "full", trust = "instruction" },
//! ]
//! remove = ["history.tail"]
//!
//! [patch.exclude]
//! append = ["secrets/**"]
//! ```
//!
//! `patch.sections.append` fügt neue Sektionen an; trägt ein Eintrag denselben
//! `name` wie eine bereits vorhandene Sektion, **überschreibt** er sie an
//! derselben Position (gezielte Überschreibung, keine Duplizierung — siehe
//! [`resolve_context_program`]). `patch.sections.remove` entfernt Sektionen nach
//! Name; `patch.sections.replace` ersetzt die gesamte Liste. `patch.exclude.*`
//! nutzt die vorhandene [`crate::merge::MergeOp`]-Maschinerie unverändert
//! (`append`/`prepend`/`remove`/`replace`/`intersect`).
//!
//! ## Welche Achsen `extends` schneidet, und welche additiv bleiben
//! Eine Sektion trägt mindestens vier Achsen: `name`, `strength`, `detail`,
//! `trust`. Der Halbverband-Grundsatz „nie über die Basis hinaus" trägt an
//! vier anderen Stellen dieses Programms bereits eine Sicherheitszusage
//! (`NetworkScope`, `ReadScope`, `ContextCeiling::intersect`,
//! `AuthorityCeiling`) — die Frage für dieses Modul ist, für welche dieser
//! vier Sektions-Achsen „nie über die Basis hinaus" überhaupt eine
//! wohldefinierte, erzwingbare Richtung hat.
//!
//! - **`trust` — geschnitten.** [`harw_context::TrustClass::trust_rank`] ist
//!   eine totale, sicherheitsrelevante Ordnung: eine höhere Klasse bedeutet
//!   *mehr* Vertrauen, das die Montage später gewährt (z. B. ob ein Fragment
//!   als bindende Weisung statt als bloße Evidenz behandelt wird). Ein Kind,
//!   das über `extends` (direkt oder über ein Mixin) für eine **geerbte**
//!   Sektion eine höhere Klasse deklariert als die Basis, weitet damit eine
//!   Rechtevergabe aus, die die Basis bewusst begrenzt hat — genau das Muster,
//!   das bei `AuthorityCeiling`/`NetworkScope`/`ReadScope` bereits als
//!   Rechteausweitung gilt. [`resolve_inner`] erzwingt deshalb: das
//!   *aufgelöste* `trust` einer geerbten Sektion darf den `trust_rank` der
//!   `extends`-Basis nie überschreiten (Fehler:
//!   [`ContextProgramError::TrustEscalation`]). Ein Kind darf jederzeit
//!   **absenken** (z. B. Basis `instruction` → Kind `evidence`) — das ist
//!   Verzicht auf Vertrauen, keine Ausweitung.
//! - **`detail` — additiv.** `Full` liefert mehr Text als `Summary`, `Summary`
//!   mehr als `References` — aber „mehr Text" ist keine Rechteausweitung: es
//!   ändert nicht, WELCHE Daten mit welchem Vertrauen in den Kontext dürfen,
//!   nur WIE VOLLSTÄNDIG eine bereits zugelassene Sektion gerendert wird.
//!   `plan.toml` und `triage.toml` (AW6-05) heben `history.tail` bewusst von
//!   der Basis-`summary` auf `full` an, weil ihre Rolle den exakten Wortlaut
//!   des letzten Turns braucht; `research-web.toml`/`review.toml` senken
//!   stattdessen auf `references`. Beide Richtungen sind gewollt — eine
//!   Schnitt-Regel auf `detail` würde die erste Gruppe brechen, ohne einen
//!   Sicherheitsgewinn zu erzielen, der `TrustClass` nicht ohnehin schon
//!   liefert.
//! - **`strength` — additiv.** `MustInclude` statt `Normal` ändert nur die
//!   Montage-Priorität einer bereits zugelassenen, bereits mit ihrem `trust`
//!   geprüften Sektion (ob sie das Budget unbedingt bekommt) — nicht, welche
//!   Daten mit welchem Vertrauen überhaupt sichtbar werden. Kein Sicherheits-
//!   grund, das zu deckeln.
//! - **Eine neue Sektion (Name, den die Basis nicht kennt) — zulässige
//!   Spezialisierung, keine Ausweitung.** Die Basis definiert keinen Floor für
//!   einen Namen, den sie nicht kennt; ihr `trust`-Schnitt gilt nur für
//!   Namen, die sie tatsächlich deklariert (siehe `inherited_trust` in
//!   [`resolve_inner`]). Alle neun Rollenprogramme (AW6-05) fügen eigene
//!   Sektionen mit eigenem `trust` hinzu, teils `instruction`
//!   (`goal.invariants`, `web.fetch_allowlist`) — das ist die Rolle, die eine
//!   eigene, für sie neue Sicherheitsentscheidung trifft, keine Erweiterung
//!   einer fremden.
//! - **`exclude` — geschnitten, in der bereits vorhandenen Richtung.** Der
//!   Ausschluss-Fußboden der `extends`-Basis ist unentfernbar
//!   ([`append_unique`] kann nur hinzufügen, nie einen geerbten Ausschluss
//!   aufheben) — das war schon vor diesem Knoten korrekt und bleibt
//!   unverändert.
//!
//! Wer eine fünfte Achse ergänzt: die Prüfung ist „hat dieses Feld eine
//! totale Ordnung, bei der `größer` eine Rechteausweitung bedeutet, die
//! nirgendwo sonst schon geprüft wird?" — nur dann gehört sie geschnitten.
//!
//! # Auflösungskaskade
//! [`resolve_context_program`] übernimmt exakt die Mechanik von
//! [`crate::family::resolve_family`] (Vorbild dieses Moduls): Layer aufsteigend
//! sortieren (§4 `DefinitionLayer`), den niedrigsten Layer mit `target_id` als
//! Basis nehmen, `extends` rekursiv auflösen, dann Mixins und die eigenen
//! Sektionen der Basis übernehmen, und schließlich `patch`-Tabellen aller
//! höheren Layer mit derselben `target_id` anwenden. Anders als `resolve_family`
//! erkennt diese Fassung zusätzlich Vererbungszyklen über
//! [`crate::error::DslError::InheritanceCycle`] (dieselbe Fehlervariante, die
//! auch `resolve_definition` für Agenten nutzt) — eine bewusste Härtung, kein
//! Nachbau einer zweiten Mechanik.
//!
//! # Deckenprüfung
//! [`ContextCeilingAdmission::admits_program`] prüft ein aufgelöstes Programm
//! gegen eine [`harw_context::ContextCeiling`]: verlangt eine Sektion mehr, als
//! die Decke zulässt (`ceiling.sections`) oder eine höhere Vertrauensklasse als
//! `ceiling.max_trust` (verglichen ausschließlich über
//! [`harw_context::TrustClass::trust_rank`], niemals über die aus der
//! Deklarationsreihenfolge abgeleitete `Ord`-Instanz — siehe die Warnung in
//! `harw_context::fragment`), wird das **gesamte Programm abgewiesen**, nicht
//! stillschweigend beschnitten. [`harw_context::ContextCeiling::admits`] prüft
//! nur ein einzelnes [`harw_context::Fragment`]; eine Programm-Ebenen-Prüfung
//! bietet `harw-context` (Stand dieses Knotens) nicht an — dieser Trait schließt
//! die Lücke lokal, gehört inhaltlich aber nach `harw-context` (siehe
//! Abschlussbericht des Knotens).
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und klonierbar. [`resolve_context_program`] und
//! [`ContextCeilingAdmission::admits_program`] sind zustandslos und thread-sicher.
//!
//! # Fehler
//! [`resolve_context_program`] liefert [`ContextProgramResult`]
//! (`Result<_, `[`ContextProgramError`]`>`), nicht mehr nur den strukturellen
//! [`crate::error::DslResult`] — seit dieser Knoten die `trust`-Schnitt-Regel
//! oben durchsetzt, kann die Auflösung selbst (nicht erst die Deckenprüfung)
//! ein Programm ablehnen, ohne dass `crate::error::DslError` dafür eine
//! passende Variante trägt:
//! - [`ContextProgramError::Resolution`] (bündelt `crate::error::DslError`):
//!     - [`crate::error::DslError::MissingBase`]: `target_id` oder eine
//!       `extends`-Referenz ist in keiner Schicht auffindbar.
//!     - [`crate::error::DslError::MissingMixin`]: eine `mixins`-Referenz ist
//!       nicht auffindbar.
//!     - [`crate::error::DslError::InheritanceCycle`]: `extends` bildet einen
//!       Zyklus.
//!     - [`crate::error::DslError::Toml`]: eine `patch.sections.*`-Tabelle
//!       lässt sich nicht in [`RawContextSectionSpec`] deserialisieren.
//! - [`ContextProgramError::TrustEscalation`]: ein Kind deklariert für eine
//!   geerbte Sektion eine höhere [`harw_context::TrustClass`], als die
//!   `extends`-Basis dafür vorsah (§ „Welche Achsen `extends` schneidet" oben).
//! - [`ContextProgramError::RejectedByCeiling`] (bündelt
//!   [`ProgramCeilingViolation`]): das aufgelöste Programm verlangt mehr, als
//!   die geprüfte Decke zulässt — nur über
//!   [`resolve_context_program_within_ceiling`] erreichbar.
//!
//! # Beispiele
//! ```rust
//! use harw_agent_dsl::context_program::{resolve_context_program, RawContextProgramDefinition};
//! use harw_agent_dsl::ids::DefinitionId;
//! use harw_agent_dsl::layers::DefinitionLayer;
//!
//! let src = r#"
//! schema = "harwness.context/v1"
//! id = "harwness.context.minimal@1"
//! version = "1.0.0"
//!
//! [[sections]]
//! name = "history.tail"
//! "#;
//! let raw: RawContextProgramDefinition = toml::from_str(src).unwrap();
//! let id = DefinitionId::parse("harwness.context.minimal@1").unwrap();
//! let layers = vec![(DefinitionLayer::BuiltIn, raw)];
//! let resolved =
//!     resolve_context_program(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
//! assert_eq!(resolved.sections.len(), 1);
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{DiagLocation, DslError, DslResult};
use crate::ids::{DefinitionId, DefinitionRef, Version};
use crate::layers::DefinitionLayer;
use crate::merge::{MergeOp, apply_merge_op};
use crate::resolved::{ResolutionStep, ResolutionTrace};

// ---------------------------------------------------------------------------
// Roh-Typen (TOML-nah, vor Merge/Resolve)
// ---------------------------------------------------------------------------

/// Stärke einer Sektion innerhalb eines Kontextprogramms.
///
/// # Description
/// `MustInclude` markiert eine Sektion, die bei der Montage zwingend
/// aufgenommen werden muss; `Normal` (Default) unterliegt der gewöhnlichen
/// Budget-/Ranking-Entscheidung der Montage (`harw-core`, außerhalb dieses
/// Knotens).
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::{RawContextSectionSpec, SectionStrength};
///
/// let toml_str = r#"
/// name = "goal.invariants"
/// strength = "must-include"
/// "#;
/// let section: RawContextSectionSpec = toml::from_str(toml_str).unwrap();
/// assert_eq!(section.strength, SectionStrength::MustInclude);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SectionStrength {
    /// Unterliegt der gewöhnlichen Budget-/Ranking-Entscheidung der Montage.
    Normal,
    /// Muss bei der Montage zwingend aufgenommen werden.
    MustInclude,
}

/// Default-Stärke einer Sektion, wenn `strength` fehlt (§ Grammatik).
fn default_strength() -> SectionStrength {
    SectionStrength::Normal
}

/// Default-Rendermodus einer Sektion, wenn `detail` fehlt (§ Grammatik).
fn default_detail() -> harw_context::DetailMode {
    harw_context::DetailMode::Summary
}

/// Default-Vertrauensanforderung einer Sektion, wenn `trust` fehlt (§ Grammatik).
///
/// `Data` ist die am wenigsten anspruchsvolle Klasse (niedrigster
/// [`harw_context::TrustClass::trust_rank`]): eine Sektion ohne explizite
/// Angabe verlangt dadurch nie mehr, als jede Decke ohnehin zulässt — nur eine
/// *explizite* höhere Anforderung kann bei der Deckenprüfung scheitern.
fn default_trust() -> harw_context::TrustClass {
    harw_context::TrustClass::Data
}

/// Eine einzelne `[[sections]]`-Deklaration eines Kontextprogramms (§ Grammatik).
///
/// # Description
/// Wird sowohl in [`RawContextProgramDefinition::sections`] (vor Auflösung) als
/// auch in [`ResolvedContextProgramDefinition::sections`] (nach Auflösung)
/// verwendet — analog dazu, wie `RawClanSpec` in `crate::organization` sowohl
/// roh als auch aufgelöst auftritt.
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::RawContextSectionSpec;
///
/// let toml_str = r#"
/// name = "goal.invariants"
/// strength = "must-include"
/// detail = "full"
/// trust = "instruction"
/// "#;
/// let section: RawContextSectionSpec = toml::from_str(toml_str).unwrap();
/// assert_eq!(section.name, "goal.invariants");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawContextSectionSpec {
    /// Sektionsname, z. B. `"history.tail"`. Wird bei der Deckenprüfung als
    /// [`harw_context::SectionName`] validiert; ein leerer oder
    /// steuerzeichenhaltiger Name führt dort fail-closed zur Ablehnung des
    /// gesamten Programms, nicht bereits beim Parsen dieser Struktur.
    pub name: String,
    /// Stärke der Sektion (§ Grammatik). Default: [`SectionStrength::Normal`].
    #[serde(default = "default_strength")]
    pub strength: SectionStrength,
    /// Rendermodus der Sektion. Default: [`harw_context::DetailMode::Summary`].
    #[serde(default = "default_detail")]
    pub detail: harw_context::DetailMode,
    /// Mindest-Vertrauensanforderung dieser Sektion. Default:
    /// [`harw_context::TrustClass::Data`] (die geringste Anforderung).
    #[serde(default = "default_trust")]
    pub trust: harw_context::TrustClass,
}

/// Rohe TOML-nahe Kontextprogramm-Definition — vor Merge/Resolve (§ Grammatik).
///
/// # Description
/// Entspricht einem Kontextprogramm-TOML-Dokument mit dem Schema
/// `harwness.context/v1`. Anders als [`crate::raw::RawAgentDefinition`] ist
/// dieser Typ **kein** offenes Erweiterungsformat: `deny_unknown_fields` gilt
/// hier, weil Kontextprogramme — wie Familien und Organisationen — abgeschlossene
/// Definitionen sind, keine durch beliebige TOML-Tabellen erweiterbaren
/// Basisformate.
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::RawContextProgramDefinition;
///
/// let src = r#"
/// schema = "harwness.context/v1"
/// id = "harwness.context.base@1"
/// version = "1.0.0"
///
/// [[sections]]
/// name = "history.tail"
/// "#;
/// let raw: RawContextProgramDefinition = toml::from_str(src).unwrap();
/// assert_eq!(raw.sections.len(), 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawContextProgramDefinition {
    /// Schema-Version des Dokuments, z. B. `"harwness.context/v1"`.
    pub schema: String,
    /// Namensraum-qualifizierte ID des Kontextprogramms (§5); `kind`
    /// konventionell `"context"`.
    pub id: DefinitionId,
    /// Semantische Vollversion des Kontextprogramms.
    pub version: Version,
    /// Optionale Basis-Definition (§6, `extends`; single inheritance).
    #[serde(default)]
    pub extends: Option<DefinitionRef>,
    /// Geordnete Liste von Mixins (§6); jedes Mixin trägt eigene `sections`/
    /// `exclude`, die in Deklarationsreihenfolge übernommen werden.
    #[serde(default)]
    pub mixins: Vec<DefinitionRef>,
    /// Sektionen dieses Kontextprogramms in Deklarationsreihenfolge — die
    /// Reihenfolge ist bedeutungstragend (§ Grammatik) und wird von der
    /// Auflösung nicht verändert.
    #[serde(default)]
    pub sections: Vec<RawContextSectionSpec>,
    /// Selektor-Muster, die nie in den Kontext gelangen dürfen.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Merge-Patches für Vererbung (§7); ausgewertet durch [`resolve_context_program`].
    #[serde(default)]
    pub patch: toml::Table,
}

// ---------------------------------------------------------------------------
// Aufgelöste IR-Typen
// ---------------------------------------------------------------------------

/// Aufgelöste Kontextprogramm-Definition — IR für Consumers (§ Grammatik).
///
/// # Description
/// Enthält das Ergebnis nach vollständiger Schichtenauflösung über
/// [`resolve_context_program`]. `sections` ist reihenfolgetreu (nicht sortiert,
/// nicht dedupliziert nach Erstellung) — die Deklarationsreihenfolge ist Inhalt.
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedContextProgramDefinition {
    /// Eindeutige Namensraum-ID des aufgelösten Kontextprogramms.
    pub id: DefinitionId,
    /// Semantische Version aus dem topmost Layer, der einen Patch beigetragen hat
    /// (sonst die Version der Basis — siehe [`resolve_context_program`]).
    pub version: Version,
    /// Alle Sektionen nach vollständiger Auflösung, in Aufnahmereihenfolge.
    pub sections: Vec<RawContextSectionSpec>,
    /// Alle Ausschluss-Selektoren nach Merge.
    pub exclude: Vec<String>,
    /// Auditpfad der Auflösung (base → mixins → patches).
    pub trace: ResolutionTrace,
}

// ---------------------------------------------------------------------------
// Auflösungslogik
// ---------------------------------------------------------------------------

/// Fügt `new_section` gezielt in `sections` ein (§ Grammatik).
///
/// Existiert bereits eine Sektion mit demselben `name`, wird sie **an ihrer
/// bisherigen Position ersetzt** — das ist der Mechanismus, über den sowohl ein
/// `patch.sections.append`-Eintrag als auch eine direkt redeklarierte Sektion
/// eine geerbte Sektion gezielt überschreiben, ohne die Reihenfolge zu stören.
/// Ist der Name neu, wird die Sektion ans Ende angehängt.
fn merge_section(sections: &mut Vec<RawContextSectionSpec>, new_section: RawContextSectionSpec) {
    if let Some(existing) = sections.iter_mut().find(|s| s.name == new_section.name) {
        *existing = new_section;
    } else {
        sections.push(new_section);
    }
}

/// Hängt `value` an `list` an, sofern noch nicht enthalten (dedupliziertes Append).
fn append_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|existing| existing == value) {
        list.push(value.to_owned());
    }
}

/// Selects a reference target among context-program layers. Mirrors
/// `crate::resolve::find_reference`, adapted to [`RawContextProgramDefinition`]:
/// an explicit full version must match exactly; otherwise the newest version in
/// the highest-priority matching layer is selected.
fn find_reference<'a>(
    sorted: &[&'a (DefinitionLayer, RawContextProgramDefinition)],
    reference: &DefinitionRef,
) -> Option<&'a RawContextProgramDefinition> {
    if let Some(version) = &reference.version {
        return sorted
            .iter()
            .rev()
            .find(|(_, definition)| definition.id == reference.id && definition.version == *version)
            .map(|(_, definition)| definition);
    }

    sorted
        .iter()
        .filter(|(_, definition)| definition.id == reference.id)
        .max_by(|(left_layer, left), (right_layer, right)| {
            left_layer
                .cmp(right_layer)
                .then_with(|| left.version.0.cmp(&right.version.0))
        })
        .map(|(_, definition)| definition)
}

/// Applies the mixins declared by one context-program definition, in
/// declaration order. Mirrors `crate::resolve::apply_mixins`, minus the
/// role-compatibility check (context programs have no role).
fn apply_mixins(
    definition: &RawContextProgramDefinition,
    sorted: &[&(DefinitionLayer, RawContextProgramDefinition)],
    now: OffsetDateTime,
    sections: &mut Vec<RawContextSectionSpec>,
    exclude: &mut Vec<String>,
    trace_steps: &mut Vec<ResolutionStep>,
) -> DslResult<()> {
    for (mixin_idx, mixin_ref) in definition.mixins.iter().enumerate() {
        let mixin = find_reference(sorted, mixin_ref).ok_or_else(|| DslError::MissingMixin {
            of: Box::new(definition.id.clone()),
            referenced: Box::new(mixin_ref.clone()),
            location: DiagLocation::field(format!("mixins[{mixin_idx}]")),
        })?;

        for section in mixin.sections.clone() {
            merge_section(sections, section);
        }
        for ex in &mixin.exclude {
            append_unique(exclude, ex);
        }

        trace_steps.push(ResolutionStep {
            source: mixin_ref.id.to_string(),
            kind: "mixin".to_owned(),
            applied_at: now,
        });
    }
    Ok(())
}

/// Applies `patch.sections.{append,remove,replace}` to the accumulated section list.
///
/// `append` uses [`merge_section`] per entry, so an appended section with a
/// `name` already present overwrites that entry in place (targeted override) —
/// this is the mechanism behind "a patch overwrites a specific section".
fn apply_sections_patch(
    patch: &toml::Table,
    sections: &mut Vec<RawContextSectionSpec>,
) -> DslResult<()> {
    let Some(sections_patch) = patch.get("sections").and_then(toml::Value::as_table) else {
        return Ok(());
    };

    if let Some(append_val) = sections_patch.get("append") {
        let appended: Vec<RawContextSectionSpec> = append_val
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| DslError::Toml(e.to_string()))?;
        for section in appended {
            merge_section(sections, section);
        }
    }

    if let Some(toml::Value::Array(items)) = sections_patch.get("remove") {
        let to_remove: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        sections.retain(|s| !to_remove.contains(&s.name));
    }

    if let Some(replace_val) = sections_patch.get("replace") {
        let replaced: Vec<RawContextSectionSpec> = replace_val
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| DslError::Toml(e.to_string()))?;
        *sections = replaced;
    }

    Ok(())
}

/// Applies `patch.exclude.*` to the accumulated exclude list, using the shared
/// [`crate::merge::apply_merge_op`] machinery unchanged (§7) — this field does
/// not need a dedicated patch mechanic, unlike `sections`.
fn apply_exclude_patch(patch: &toml::Table, exclude: &mut Vec<String>) -> DslResult<()> {
    let Some(exclude_patch) = patch.get("exclude").and_then(toml::Value::as_table) else {
        return Ok(());
    };

    let mut current =
        toml::Value::Array(exclude.iter().cloned().map(toml::Value::String).collect());

    if let Some(val) = exclude_patch.get("replace") {
        apply_merge_op(&mut current, MergeOp::Replace { value: val.clone() })?;
    } else if let Some(vals) = exclude_patch.get("append") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(&mut current, MergeOp::Append { values })?;
    } else if let Some(vals) = exclude_patch.get("prepend") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(&mut current, MergeOp::Prepend { values })?;
    } else if let Some(vals) = exclude_patch.get("remove") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(&mut current, MergeOp::Remove { values })?;
    } else if let Some(vals) = exclude_patch.get("intersect") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(&mut current, MergeOp::Intersect { values })?;
    }

    *exclude = current
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    Ok(())
}

/// Löst ein Kontextprogramm über die Layer-Kaskade auf (§4, §6, §7).
///
/// # Description
/// Übernimmt die Mechanik von [`crate::family::resolve_family`] (Vorbild dieses
/// Knotens):
/// 1. Sortiert `layers` aufsteigend nach [`DefinitionLayer`]-Priorität.
/// 2. Sucht den niedrigsten Layer mit `target_id` als Basis.
/// 3. Löst `extends` rekursiv auf, mit Zyklenerkennung (anders als
///    `resolve_family`, das keine Zyklenerkennung durchführt — eine bewusste
///    Härtung über die bereits vorhandene [`crate::error::DslError::InheritanceCycle`]).
/// 4. Wendet die Mixins der Basis-Definition an (in Deklarationsreihenfolge),
///    dann die eigenen `sections`/`exclude` der Basis-Definition.
/// 5. Wendet `patch.sections.*` und `patch.exclude.*` jedes höheren Layers an,
///    der dieselbe `target_id` trägt (nur `patch` wird von diesen Overlay-Layern
///    gelesen — ihre eigenen `sections`/`exclude`/`mixins`/`extends`-Felder
///    bleiben unberücksichtigt, exakt wie bei `resolve_family`).
/// 6. Baut den [`ResolutionTrace`] auf: ein `"base"`-Schritt, ein `"mixin"`-Schritt
///    je Mixin, ein `"patch"`-Schritt je Overlay-Layer mit nicht-leerem Patch.
///
/// Sektionen werden nie dedupliziert weggelassen, sondern gezielt überschrieben
/// (siehe [`merge_section`]): eine Sektion mit demselben `name` wie eine bereits
/// vorhandene ersetzt diese an ihrer Position, statt eine zweite mit gleichem
/// Namen anzuhängen. Das ist der Mechanismus für "ein Patch überschreibt gezielt".
///
/// Bei `extends` gilt zusätzlich der Halbverband-Schnitt auf `trust` (§ Moduldoku
/// „Welche Achsen `extends` schneidet"): eine geerbte Sektion darf gezielt
/// namensgleich überschrieben werden (durch dieses Kind selbst oder eines
/// seiner Mixins), aber ihre [`harw_context::TrustClass`] darf dabei nie über
/// die der `extends`-Basis hinauswachsen — `detail` und `strength` bleiben
/// dagegen additiv (dieselbe Quelle, ausführliche Begründung je Achse).
///
/// # Arguments
/// - `target_id` (`&DefinitionId`): ID des aufzulösenden Kontextprogramms.
/// - `layers` (`&[(DefinitionLayer, RawContextProgramDefinition)]`): alle
///   verfügbaren Layer-Einträge.
/// - `now` (`OffsetDateTime`): Zeitstempel für [`ResolutionStep`]-Einträge.
///
/// # Returns
/// [`ResolvedContextProgramDefinition`] bei Erfolg.
///
/// # Errors
/// - [`ContextProgramError::Resolution`] (bündelt [`DslError`]):
///     - [`DslError::MissingBase`]: `target_id` ist in keiner Schicht
///       auffindbar, oder eine `extends`-Referenz zeigt auf eine nicht
///       vorhandene Definition.
///     - [`DslError::MissingMixin`]: eine `mixins`-Referenz ist nicht
///       auffindbar.
///     - [`DslError::InheritanceCycle`]: `extends` bildet einen Zyklus.
///     - [`DslError::Toml`]: `patch.sections.append`/`replace` lässt sich
///       nicht in [`RawContextSectionSpec`] deserialisieren.
/// - [`ContextProgramError::TrustEscalation`]: ein Kind (direkt oder über ein
///   Mixin) deklariert für eine geerbte Sektion eine höhere
///   [`harw_context::TrustClass`], als die `extends`-Basis dafür vorsah.
///
/// # Concurrency
/// Zustandslos; thread-sicher. Keine Locks, keine Threads.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::{resolve_context_program, RawContextProgramDefinition};
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let src = r#"
/// schema = "harwness.context/v1"
/// id = "harwness.context.base@1"
/// version = "1.0.0"
///
/// [[sections]]
/// name = "history.tail"
/// "#;
/// let raw: RawContextProgramDefinition = toml::from_str(src).unwrap();
/// let id = DefinitionId::parse("harwness.context.base@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved =
///     resolve_context_program(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.sections[0].name, "history.tail");
/// ```
pub fn resolve_context_program(
    target_id: &DefinitionId,
    layers: &[(DefinitionLayer, RawContextProgramDefinition)],
    now: OffsetDateTime,
) -> ContextProgramResult<ResolvedContextProgramDefinition> {
    let mut sorted: Vec<&(DefinitionLayer, RawContextProgramDefinition)> = layers.iter().collect();
    sorted.sort_by_key(|(layer, _)| *layer);

    let mut active_extends: Vec<DefinitionId> = Vec::new();
    resolve_inner(target_id, &sorted, now, &mut active_extends)
}

/// Recursive worker behind [`resolve_context_program`]; `active_extends` tracks
/// the chain of `target_id`s currently being resolved through `extends`, so a
/// cycle is reported as [`DslError::InheritanceCycle`] instead of overflowing
/// the stack.
///
/// Enforces the one structural boundary `extends` must not cross (§ Halbverband-
/// Disziplin in der Moduldoku): a section inherited from the `extends`-base can
/// be redeclared at this level (by a mixin or by this definition's own
/// `sections`) with a **lower or equal** [`harw_context::TrustClass`], never a
/// higher one. The check compares this level's *final* per-name trust (after
/// its own mixins and sections are merged in) against the trust the `extends`
/// parent handed down for that same name — never against `Ord`, always via
/// [`harw_context::TrustClass::trust_rank`], mirroring the warning in
/// `harw_context::fragment`.
fn resolve_inner(
    target_id: &DefinitionId,
    sorted: &[&(DefinitionLayer, RawContextProgramDefinition)],
    now: OffsetDateTime,
    active_extends: &mut Vec<DefinitionId>,
) -> ContextProgramResult<ResolvedContextProgramDefinition> {
    let base_entry = sorted
        .iter()
        .find(|(_, def)| &def.id == target_id)
        .ok_or_else(|| DslError::MissingBase {
            of: Box::new(target_id.clone()),
            referenced: Box::new(DefinitionRef {
                id: target_id.clone(),
                version: None,
            }),
            location: DiagLocation::none(),
        })?;
    let base_def = &base_entry.1;
    let base_layer = base_entry.0;

    let (mut sections, mut exclude, mut trace_steps, mut resolved_version) =
        if let Some(extends_ref) = &base_def.extends {
            if !sorted.iter().any(|(_, def)| def.id == extends_ref.id) {
                return Err(ContextProgramError::from(DslError::MissingBase {
                    of: Box::new(target_id.clone()),
                    referenced: Box::new(extends_ref.clone()),
                    location: DiagLocation::field("extends"),
                }));
            }

            if let Some(cycle_start) = active_extends.iter().position(|id| id == &extends_ref.id) {
                let mut cycle: Vec<DefinitionId> = active_extends[cycle_start..].to_vec();
                cycle.push(extends_ref.id.clone());
                return Err(ContextProgramError::from(DslError::InheritanceCycle {
                    cycle,
                    location: DiagLocation::field("extends"),
                }));
            }

            active_extends.push(target_id.clone());
            let parent = resolve_inner(&extends_ref.id, sorted, now, active_extends)?;
            active_extends.pop();

            (
                parent.sections,
                parent.exclude,
                parent.trace.steps,
                base_def.version.clone(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), base_def.version.clone())
        };

    // Snapshot of the per-name trust this level inherits through `extends`,
    // taken BEFORE this level's own mixins/sections are merged in. Empty when
    // there is no `extends` — nothing is inherited, so nothing can be
    // escalated. This is the one axis this module cuts rather than lets grow
    // (§ Halbverband-Disziplin): `TrustClass` is the only field on a section
    // with a security-relevant, enforceable ordering (`trust_rank`); `detail`,
    // `strength`, and a genuinely new section name are deliberately left
    // additive (see the module doc for the per-axis reasoning).
    let inherited_trust: Vec<(String, harw_context::TrustClass)> = sections
        .iter()
        .map(|section| (section.name.clone(), section.trust))
        .collect();

    apply_mixins(
        base_def,
        sorted,
        now,
        &mut sections,
        &mut exclude,
        &mut trace_steps,
    )?;

    for section in base_def.sections.clone() {
        merge_section(&mut sections, section);
    }
    for ex in &base_def.exclude {
        append_unique(&mut exclude, ex);
    }

    for (name, base_trust) in &inherited_trust {
        if let Some(current) = sections.iter().find(|s| &s.name == name)
            && current.trust.trust_rank() > base_trust.trust_rank()
        {
            {
                return Err(ContextProgramError::TrustEscalation {
                    child: Box::new(target_id.clone()),
                    section: name.clone(),
                    base_trust: *base_trust,
                    declared_trust: current.trust,
                });
            }
        }
    }

    trace_steps.push(ResolutionStep {
        source: target_id.as_string(),
        kind: "base".to_owned(),
        applied_at: now,
    });

    // Overlay-Layer über der Basis: nur `patch` wird gelesen (exakt wie
    // `resolve_family`) — direkte `sections`/`exclude`/`mixins` eines
    // Overlay-Layers werden bewusst ignoriert.
    for (layer, def) in sorted {
        if *layer <= base_layer || def.id != *target_id {
            continue;
        }
        let patch = &def.patch;
        if patch.is_empty() {
            continue;
        }

        apply_sections_patch(patch, &mut sections)?;
        apply_exclude_patch(patch, &mut exclude)?;
        resolved_version = def.version.clone();

        trace_steps.push(ResolutionStep {
            source: target_id.as_string(),
            kind: "patch".to_owned(),
            applied_at: now,
        });
    }

    Ok(ResolvedContextProgramDefinition {
        id: target_id.clone(),
        version: resolved_version,
        sections,
        exclude,
        trace: ResolutionTrace { steps: trace_steps },
    })
}

// ---------------------------------------------------------------------------
// Deckenprüfung
// ---------------------------------------------------------------------------

/// Warum ein aufgelöstes Kontextprogramm von einer [`harw_context::ContextCeiling`]
/// abgewiesen wurde.
///
/// # Description
/// Mirrors the shape of [`harw_context::CeilingViolation`] (per-fragment), but
/// at program granularity: each variant names the one aspect of
/// [`ContextCeilingAdmission::admits_program`] that failed, plus the values
/// that caused the rejection — no fragment-level `OverBudget` case, because a
/// static section declaration carries no cost estimate (that only exists once
/// real fragments are assembled, in `harw-core`, outside this node's scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramCeilingViolation {
    /// A section the program declares is not part of [`harw_context::ContextCeiling::sections`].
    SectionNotAllowed {
        /// The rejected program's id.
        program: DefinitionId,
        /// The section name that is not part of the ceiling.
        section: String,
    },
    /// A section's declared [`harw_context::TrustClass`] exceeds the ceiling's
    /// `max_trust`, compared via [`harw_context::TrustClass::trust_rank`].
    TrustExceeded {
        /// The rejected program's id.
        program: DefinitionId,
        /// The section name that declares the excessive trust requirement.
        section: String,
        /// The trust class the section declares.
        trust: harw_context::TrustClass,
        /// The ceiling's maximum permitted trust class.
        max_trust: harw_context::TrustClass,
    },
}

impl std::fmt::Display for ProgramCeilingViolation {
    /// Human-readable rejection reason, without internal jargon.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProgramCeilingViolation::SectionNotAllowed { program, section } => write!(
                f,
                "context program '{program}' requires section '{section}', which is not part of the agent's context ceiling"
            ),
            ProgramCeilingViolation::TrustExceeded {
                program,
                section,
                trust,
                max_trust,
            } => write!(
                f,
                "context program '{program}' requires section '{section}' at trust {trust:?}, which exceeds the ceiling's max_trust {max_trust:?}"
            ),
        }
    }
}

impl std::error::Error for ProgramCeilingViolation {}

/// Erweitert [`harw_context::ContextCeiling`] um eine Programm-Ebenen-Prüfung.
///
/// # Description
/// `harw-context` bietet mit [`harw_context::ContextCeiling::admits`] nur eine
/// Prüfung auf Ebene eines einzelnen [`harw_context::Fragment`] an; für ein
/// ganzes aufgelöstes Kontextprogramm gibt es dort (Stand dieses Knotens) keine
/// Entsprechung. Dieser Trait schließt die Lücke lokal in `harw-agent-dsl`,
/// gehört aber inhaltlich nach `harw-context` — siehe den Abschlussbericht
/// dieses Knotens (AW2-01).
///
/// Die Erweiterung ist als Trait statt als freie Funktion modelliert, damit
/// Aufrufer `ceiling.admits_program(&resolved)` genauso schreiben können wie
/// das bestehende `ceiling.admits(&fragment)`.
pub trait ContextCeilingAdmission {
    /// Prüft, ob `program` vollständig innerhalb dieser Decke liegt.
    ///
    /// # Description
    /// Prüft jede Sektion von `program` in Deklarationsreihenfolge gegen zwei
    /// Aspekte: liegt ihr Name in [`harw_context::ContextCeiling::sections`];
    /// übersteigt ihr [`harw_context::TrustClass::trust_rank`] den Rang von
    /// [`harw_context::ContextCeiling::max_trust`]. Beim ersten Verstoß bricht
    /// die Prüfung ab — das **gesamte Programm** wird abgewiesen, nicht nur die
    /// verstoßende Sektion stillschweigend entfernt (die Zusage dieses Knotens).
    ///
    /// Budget-/Kostenprüfung (`harw_context::ContextCeiling::admits`s dritter
    /// Aspekt) findet hier bewusst nicht statt: eine Sektionsdeklaration trägt
    /// keine Kostenschätzung, die entsteht erst mit echten Fragmenten zur
    /// Montagezeit (`harw-core`, außerhalb dieses Knotens).
    ///
    /// # Arguments
    /// - `program` (`&ResolvedContextProgramDefinition`): das zu prüfende,
    ///   bereits vollständig aufgelöste Kontextprogramm.
    ///
    /// # Returns
    /// `Ok(())`, wenn jede Sektion beide Aspekte erfüllt.
    ///
    /// # Errors
    /// - [`ProgramCeilingViolation::SectionNotAllowed`]: eine Sektion liegt
    ///   nicht in [`harw_context::ContextCeiling::sections`], oder ihr Name ist
    ///   als [`harw_context::SectionName`] ungültig (fail-closed: ein
    ///   ungültiger Name wird nie zugelassen).
    /// - [`ProgramCeilingViolation::TrustExceeded`]: eine Sektion verlangt eine
    ///   höhere Vertrauensklasse als [`harw_context::ContextCeiling::max_trust`].
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_agent_dsl::context_program::{ContextCeilingAdmission, ResolvedContextProgramDefinition};
    /// use harw_context::ContextCeiling;
    ///
    /// fn check(ceiling: &ContextCeiling, program: &ResolvedContextProgramDefinition) {
    ///     if let Err(violation) = ceiling.admits_program(program) {
    ///         eprintln!("program rejected: {violation}");
    ///     }
    /// }
    /// ```
    fn admits_program(
        &self,
        program: &ResolvedContextProgramDefinition,
    ) -> Result<(), ProgramCeilingViolation>;
}

impl ContextCeilingAdmission for harw_context::ContextCeiling {
    fn admits_program(
        &self,
        program: &ResolvedContextProgramDefinition,
    ) -> Result<(), ProgramCeilingViolation> {
        for section in &program.sections {
            let allowed = harw_context::SectionName::try_new(section.name.clone())
                .is_ok_and(|name| self.sections.contains(&name));
            if !allowed {
                return Err(ProgramCeilingViolation::SectionNotAllowed {
                    program: program.id.clone(),
                    section: section.name.clone(),
                });
            }

            if section.trust.trust_rank() > self.max_trust.trust_rank() {
                return Err(ProgramCeilingViolation::TrustExceeded {
                    program: program.id.clone(),
                    section: section.name.clone(),
                    trust: section.trust,
                    max_trust: self.max_trust,
                });
            }
        }
        Ok(())
    }
}

/// Vereinigt die strukturelle Auflösung und die Deckenprüfung eines Kontextprogramms.
///
/// # Description
/// Hand-geschriebener Error-Enum (kein `anyhow`/`thiserror`, § Projektkonvention):
/// entweder ist die strukturelle Auflösung (`extends`/Mixins/Patch) gescheitert,
/// oder das strukturell gültige Ergebnis wurde von der geprüften Decke
/// abgewiesen. `Debug` wird abgeleitet (beide Varianten leiten selbst bereits
/// von `Debug` ab); `Display` delegiert an die jeweils innere Fehlermeldung.
#[derive(Debug)]
pub enum ContextProgramError {
    /// Die strukturelle Auflösung (`extends`/Mixins/Patch) ist gescheitert.
    Resolution(DslError),
    /// Das strukturell aufgelöste Programm wurde von der Decke abgewiesen.
    RejectedByCeiling(ProgramCeilingViolation),
    /// Ein Kind hat über `extends` (direkt oder über ein Mixin) eine höhere
    /// [`harw_context::TrustClass`] für eine geerbte Sektion deklariert, als
    /// die Basis dafür vorsah — die einzige Achse, die diese Grammatik
    /// erzwingt (§ Halbverband-Disziplin in der Moduldoku), verglichen
    /// ausschließlich über [`harw_context::TrustClass::trust_rank`].
    TrustEscalation {
        /// ID des Kindes, das die Eskalation versucht (geboxt, wie
        /// `DslError`s eigene `of`/`referenced`-Felder).
        child: Box<DefinitionId>,
        /// Name der betroffenen, geerbten Sektion.
        section: String,
        /// Die von der `extends`-Basis für diese Sektion deklarierte Klasse.
        base_trust: harw_context::TrustClass,
        /// Die vom Kind (direkt oder über ein Mixin) deklarierte, zu hohe Klasse.
        declared_trust: harw_context::TrustClass,
    },
}

/// Alias für `Result<T, ContextProgramError>`.
pub type ContextProgramResult<T> = Result<T, ContextProgramError>;

impl std::fmt::Display for ContextProgramError {
    /// Delegiert an die innere Fehlermeldung — keine doppelte Formatierung.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextProgramError::Resolution(e) => write!(f, "{e}"),
            ContextProgramError::RejectedByCeiling(e) => write!(f, "{e}"),
            ContextProgramError::TrustEscalation {
                child,
                section,
                base_trust,
                declared_trust,
            } => write!(
                f,
                "context program '{child}' declares trust {declared_trust:?} for inherited section '{section}', which exceeds the extends-base's {base_trust:?}"
            ),
        }
    }
}

impl std::error::Error for ContextProgramError {
    /// Verkettet zur jeweils inneren Fehlerursache.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ContextProgramError::Resolution(e) => Some(e),
            ContextProgramError::RejectedByCeiling(e) => Some(e),
            ContextProgramError::TrustEscalation { .. } => None,
        }
    }
}

impl From<DslError> for ContextProgramError {
    /// Konvertiert einen strukturellen Auflösungsfehler in [`ContextProgramError::Resolution`].
    fn from(e: DslError) -> Self {
        ContextProgramError::Resolution(e)
    }
}

impl From<ProgramCeilingViolation> for ContextProgramError {
    /// Konvertiert eine Deckenabweisung in [`ContextProgramError::RejectedByCeiling`].
    fn from(e: ProgramCeilingViolation) -> Self {
        ContextProgramError::RejectedByCeiling(e)
    }
}

/// Löst ein Kontextprogramm auf und prüft es unmittelbar gegen eine Decke.
///
/// # Description
/// Kombiniert [`resolve_context_program`] und
/// [`ContextCeilingAdmission::admits_program`] zu einem einzigen Aufruf: ein
/// Programm, das mehr verlangt, als `ceiling` zulässt, kommt hier **nie** als
/// `Ok` zurück — es gibt keinen Pfad, der es stillschweigend beschneidet.
///
/// # Arguments
/// - `target_id` (`&DefinitionId`): ID des aufzulösenden Kontextprogramms.
/// - `layers` (`&[(DefinitionLayer, RawContextProgramDefinition)]`): alle
///   verfügbaren Layer-Einträge.
/// - `ceiling` (`&harw_context::ContextCeiling`): die Decke des Agenten, gegen
///   die das aufgelöste Programm geprüft wird.
/// - `now` (`OffsetDateTime`): Zeitstempel für [`ResolutionStep`]-Einträge.
///
/// # Returns
/// [`ResolvedContextProgramDefinition`], wenn die Auflösung gelingt **und** das
/// Ergebnis innerhalb von `ceiling` liegt.
///
/// # Errors
/// - [`ContextProgramError::Resolution`] — siehe [`resolve_context_program`].
/// - [`ContextProgramError::RejectedByCeiling`] — siehe
///   [`ContextCeilingAdmission::admits_program`].
///
/// # Concurrency
/// Zustandslos; thread-sicher.
///
/// # Examples
/// ```rust,no_run
/// use harw_agent_dsl::context_program::{
///     resolve_context_program_within_ceiling, RawContextProgramDefinition,
/// };
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
/// use harw_context::ContextCeiling;
///
/// fn run(
///     id: &DefinitionId,
///     layers: &[(DefinitionLayer, RawContextProgramDefinition)],
///     ceiling: &ContextCeiling,
/// ) {
///     match resolve_context_program_within_ceiling(id, layers, ceiling, time::OffsetDateTime::now_utc()) {
///         Ok(resolved) => println!("admitted: {} sections", resolved.sections.len()),
///         Err(e) => eprintln!("rejected: {e}"),
///     }
/// }
/// ```
pub fn resolve_context_program_within_ceiling(
    target_id: &DefinitionId,
    layers: &[(DefinitionLayer, RawContextProgramDefinition)],
    ceiling: &harw_context::ContextCeiling,
    now: OffsetDateTime,
) -> ContextProgramResult<ResolvedContextProgramDefinition> {
    let resolved = resolve_context_program(target_id, layers, now)?;
    ceiling.admits_program(&resolved)?;
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Golden-Rendering (AW6-05 Golden-Vergleich)
// ---------------------------------------------------------------------------

/// Kleinbuchstaben-Bezeichnung einer [`harw_context::TrustClass`] für den
/// Sektionskopf des Golden-Renders (§ [`render_context_program`]).
fn trust_label(trust: harw_context::TrustClass) -> &'static str {
    match trust {
        harw_context::TrustClass::Instruction => "instruction",
        harw_context::TrustClass::Evidence => "evidence",
        harw_context::TrustClass::Data => "data",
    }
}

/// Kleinbuchstaben-Bezeichnung eines [`harw_context::DetailMode`] für den
/// Sektionskopf-Zusatz und den Fallback-Platzhaltertext (§ [`render_context_program`]).
fn detail_label(detail: harw_context::DetailMode) -> &'static str {
    match detail {
        harw_context::DetailMode::Full => "full",
        harw_context::DetailMode::Summary => "summary",
        harw_context::DetailMode::References => "references",
    }
}

/// Bestimmt den Platzhaltertext einer Sektion, rein aus `name` und `detail`.
///
/// # Description
/// Feste, handkuratierte Kataloge je bekanntem Sektionsnamen der neun
/// Rollenprogramme (AW6-05: `curate`, `explore`, `implement`, `orchestrate`,
/// `plan`, `research-deps`, `research-web`, `review`, `triage`). Für
/// `"history.tail"` — die einzige Sektion, die jedes der neun Programme von
/// `base` erbt und die in vier von neun Programmen namensgleich mit anderem
/// `detail` überschrieben wird (§ Moduldoku „Welche Achsen `extends`
/// schneidet") — hängt der Text vom `detail` ab; alle anderen Sektionsnamen
/// in dieser Bibliothek treten je Programm nur mit einem festen `detail` auf.
///
/// **Bewusst KEINE konkreten Beispieldaten** (Turnnummern, Zeitstempel,
/// Digest-Werte) für `detail = References`: der Renderer erhält nur die
/// Struktur ([`ResolvedContextProgramDefinition`]), keine echten Verlaufs-
/// oder Fragmentdaten — diese entstehen erst bei der tatsächlichen Montage in
/// `harw-core` (außerhalb dieses Knotens, siehe Moduldoku „Verantwortungsbereich").
/// Der `[ref]`-Zeilenaufbau selbst folgt wörtlich den Feldnamen aus
/// `harw_context::reference::FragmentReference`s `Display`-Implementierung
/// (`section`, `label`, `trust`, `stability`, `origin`, `cost`, `digest`),
/// nur mit spitzen Platzhaltern statt erfundener Beispielwerte.
///
/// Ein Sektionsname außerhalb dieses Katalogs (z. B. ein künftiges zehntes
/// Programm) erhält einen generischen, aus `name`/`detail` selbst gebildeten
/// Platzhalter — der Renderer verweigert die Ausgabe nie, er rät nur nichts
/// Rollen-Spezifisches, das er nicht wissen kann.
fn section_placeholder_body(name: &str, detail: harw_context::DetailMode) -> String {
    use harw_context::DetailMode::{Full, References, Summary};

    match (name, detail) {
        ("task.objective", _) => "<vollständiger Auftragstext des aktuellen Turns>".to_owned(),
        ("history.tail", Summary) => {
            "<zusammengefasster jüngster Verlauf der letzten Turns, gemäß \
             HISTORY_TAIL_GUARANTEED_GROUPS vollständig, älteres ggf. bereits als [ref] verkürzt>"
                .to_owned()
        }
        ("history.tail", Full) => {
            "<vollständiger jüngster Verlauf, keine [ref]-Kürzung>".to_owned()
        }
        ("history.tail", References) => [
            "[ref] section=\"history.tail\" label=\"<Turn-Label>\" trust=Evidence stability=<Stable|Volatile>",
            " origin=\"<Herkunftsquelle>\"@<Zeitstempel> cost=CostEstimate(<n>)",
            " digest=b3:<hex> (load via context.load)",
            "<garantierter Schwanz (HISTORY_TAIL_GUARANTEED_GROUPS) bleibt vollständig gerendert,",
            " kein [ref] für die jüngsten Gruppen>",
        ]
        .join("\n"),
        ("task.read_scope", _) => {
            "<vollständige Glob-/Verzeichnisliste des erlaubten Lesebereichs>".to_owned()
        }
        ("repo.tree", _) => {
            "<zusammengefasster Verzeichnis-/Modulbaum als Navigationsstartpunkt>".to_owned()
        }
        ("goal.invariants", Full) => "<vollständige Liste der Ziel-Randbedingungen>".to_owned(),
        ("goal.invariants", _) => "<zusammengefasste Liste der Ziel-Randbedingungen>".to_owned(),
        ("plan.current", Full) => "<vollständiger, autoritativer Plan>".to_owned(),
        ("plan.current", _) => {
            "<zusammengefasste Fassung des bereits fixierten Plans>".to_owned()
        }
        ("diff.changeset", _) => "<vollständiger Änderungsdiff, ungekürzt>".to_owned(),
        ("error.trace", _) => {
            "<vollständige Fehlermeldung/Stacktrace/fehlgeschlagene Prüfungsausgabe>".to_owned()
        }
        ("child.returns", _) => "<zusammengefasste, strukturierte Rückgaben bereits \
             abgeschlossener Kinder — keine rohen Kind-Transkripte>"
            .to_owned(),
        ("knowledge.candidates", _) => "<vollständiger, unverifizierter Kandidat — niedrigste \
             Vertrauensstufe, wird von der Deckenprüfung entsprechend behandelt>"
            .to_owned(),
        ("memory.palace_index", _) => {
            "<zusammengefasster Überblick über bereits vorhandene Gedächtniseinträge>".to_owned()
        }
        ("deps.lockfile_index", _) => {
            "<gefilterter Cargo.lock-Auszug: Name, exakte Version, Feature-Flags>".to_owned()
        }
        ("web.fetch_allowlist", _) => {
            "<vollständige Liste erreichbarer Hosts gemäß Sandbox-Allowlist>".to_owned()
        }
        (other, other_detail) => {
            format!("<Platzhalterinhalt für Sektion '{other}' ({})>", detail_label(other_detail))
        }
    }
}

/// Rendert eine einzelne Sektion als `## name [trust[, detail]]` plus Textkörper.
///
/// Der `, {detail}`-Zusatz erscheint ausschließlich bei `"history.tail"`, wenn
/// ihr aufgelöstes `detail` vom Basis-Default (`Summary`, aus `base.toml`)
/// abweicht (§ [`section_placeholder_body`]) — für jeden anderen Sektionsnamen
/// dieser Bibliothek ist `detail` eine Neudeklaration ohne vergleichbaren
/// Basiswert, also gibt es dort nichts, dessen Abweichung meldenswert wäre.
fn render_section(section: &RawContextSectionSpec) -> String {
    let detail_suffix =
        if section.name == "history.tail" && section.detail != harw_context::DetailMode::Summary {
            format!(", {}", detail_label(section.detail))
        } else {
            String::new()
        };
    format!(
        "## {} [{}{}]\n{}",
        section.name,
        trust_label(section.trust),
        detail_suffix,
        section_placeholder_body(&section.name, section.detail)
    )
}

/// Rendert ein aufgelöstes Kontextprogramm als deterministischen
/// Platzhalter-Turn-Kontext für den Golden-Vergleich (§ AW6-05,
/// `harw-registry-defaults/agents/context-programs/golden/*.golden.txt`).
///
/// # Wofür diese Ausgabe da ist
/// Sie ist **kein Modell-Eingabetext**. Die tatsächliche Fragment-Montage
/// (welcher Text eine Sektion wirklich füllt) liegt in `harw-core`, außerhalb
/// dieses Knotens (§ Moduldoku „Verantwortungsbereich"); diese Funktion füllt
/// jede Sektion stattdessen mit einem festen, aus `name`/`detail` bestimmten
/// Platzhalter (§ [`section_placeholder_body`]). Ihr einziger Zweck ist ein
/// Golden-Vergleich: sichtbar machen, WELCHE Sektionen in WELCHER Reihenfolge,
/// mit welchem Vertrauen und welcher Detailstufe ein aufgelöstes
/// Kontextprogramm tatsächlich mitbringt — und einen Regressionsalarm
/// auszulösen, sobald sich `resolve_context_program` oder eine der neun
/// `.toml`-Definitionen unbemerkt ändert.
///
/// # Warum deterministisch
/// Kein Dateizugriff, kein Netz, keine Systemuhr, keine Zufallszahl und keine
/// von einer `HashMap`-Iterationsreihenfolge abhängige Ausgabe — die einzigen
/// Eingaben sind `program.sections` (bereits reihenfolgetreu aus
/// [`resolve_context_program`], niemals sortiert oder in eine Map gelegt) und
/// eine feste `match`-Tabelle. Zwei Aufrufe mit demselben
/// [`ResolvedContextProgramDefinition`] liefern deshalb byte-genau denselben
/// String — ohne das wäre der Golden-Vergleich wertlos (§ Auftrag AW6-05).
///
/// # Warum keine `SnapshotId`
/// `crate::executable::ContextProgram`s `SNAPSHOT_HASH_DOMAIN` steht auf
/// `v3`; eine `SnapshotId` ändert sich, sobald ein Feld auf `ContextProgram`
/// hinzukommt. Stünde sie im gerenderten Text, würde jede künftige,
/// inhaltlich unrelated Erweiterung von `ContextProgram` sämtliche neun
/// Golden-Dateien rot färben — ein Golden-Test, der auf jede Erweiterung statt
/// nur auf eine tatsächliche Bedeutungsänderung dieses Programms reagiert,
/// ist kein nützlicher Regressionsalarm mehr. Diese Funktion rendert deshalb
/// ausschließlich Feldwerte von [`ResolvedContextProgramDefinition`] selbst
/// (`sections`, deren `name`/`strength`/`detail`/`trust`), nie eine Hash-ID.
///
/// # Arguments
/// - `program` (`&ResolvedContextProgramDefinition`): das bereits vollständig
///   aufgelöste Kontextprogramm (nach `extends`/Mixins/Patch).
///
/// # Returns
/// Der gerenderte Text, identisch zum Abschnitt „Gerenderter Turn-Kontext
/// (Platzhalter-Inhalt)" der zugehörigen `golden/*.golden.txt`-Datei (ohne
/// die Kopfzeile/Sektionstabelle davor — die ist Handdokumentation der
/// Fixture-Datei, kein Renderer-Ausgabefeld).
///
/// # Concurrency
/// Zustandslos; thread-sicher.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::{render_context_program, resolve_context_program, RawContextProgramDefinition};
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let src = r#"
/// schema = "harwness.context/v1"
/// id = "harwness.context.minimal@1"
/// version = "1.0.0"
///
/// [[sections]]
/// name = "history.tail"
/// "#;
/// let raw: RawContextProgramDefinition = toml::from_str(src).unwrap();
/// let id = DefinitionId::parse("harwness.context.minimal@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_context_program(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// let rendered_once = render_context_program(&resolved);
/// let rendered_twice = render_context_program(&resolved);
/// assert_eq!(rendered_once, rendered_twice);
/// ```
pub fn render_context_program(program: &ResolvedContextProgramDefinition) -> String {
    program
        .sections
        .iter()
        .map(render_section)
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::collections::BTreeSet;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn parse(src: &str) -> TestResult<RawContextProgramDefinition> {
        toml::from_str(src).map_err(ctx("fixture TOML should parse"))
    }

    fn id(s: &str) -> TestResult<DefinitionId> {
        Ok(DefinitionId::parse(s)?)
    }

    /// Builds a minimal `ContextCeiling` fixture via `serde_json`, avoiding a
    /// direct dependency on `harw-lens-types` (needed only to name
    /// `harw_lens_types::BudgetSpec` for a struct literal) — `admits_program`
    /// never reads `ceiling.budget`, so its exact value is immaterial here.
    fn ceiling(sections: &[&str], max_trust: &str) -> TestResult<harw_context::ContextCeiling> {
        let json = serde_json::json!({
            "sections": sections,
            "max_trust": max_trust,
            "budget": { "total": { "total": 1_000_000 }, "per_section": {} },
        });
        serde_json::from_value(json).map_err(ctx("ceiling fixture should deserialize"))
    }

    // -----------------------------------------------------------------------
    // Test 1: ein vollständiges Programm parst
    // -----------------------------------------------------------------------

    #[test]
    fn full_program_parses() -> TestResult {
        let src = r#"
schema = "harwness.context/v1"
id = "harwness.context.security-triage@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }
mixins = [{ id = "harwness.context.mixin.audit-trail@1" }]

# `exclude` steht vor den `[[sections]]`: ein nachgestelltes Feld gehört in
# TOML zur letzten Tabelle und fällt dann `deny_unknown_fields` zum Opfer.
# Dieselbe Falle, die das "Vollständige Beispiel" der Moduldoku hatte.
exclude = ["memory.*", "plan.other-clans/**"]

[[sections]]
name = "goal.invariants"
strength = "must-include"
detail = "full"
trust = "instruction"

[[sections]]
name = "history.tail"
detail = "summary"

[[sections]]
name = "plan.current"
strength = "must-include"
detail = "references"
"#;
        let raw = parse(src)?;
        assert_eq!(raw.schema, "harwness.context/v1");
        assert_eq!(raw.id.name, "security-triage");
        let extends = raw
            .extends
            .as_ref()
            .ok_or(TestError::Missing("raw.extends"))?;
        assert_eq!(extends.id.name, "base");
        assert_eq!(raw.mixins.len(), 1);
        assert_eq!(raw.sections.len(), 3);
        assert_eq!(raw.sections[0].name, "goal.invariants");
        assert_eq!(raw.sections[0].strength, SectionStrength::MustInclude);
        assert_eq!(raw.sections[0].detail, harw_context::DetailMode::Full);
        assert_eq!(raw.sections[0].trust, harw_context::TrustClass::Instruction);
        // Defaults for the second section.
        assert_eq!(raw.sections[1].strength, SectionStrength::Normal);
        assert_eq!(raw.sections[1].detail, harw_context::DetailMode::Summary);
        assert_eq!(raw.sections[1].trust, harw_context::TrustClass::Data);
        assert_eq!(raw.exclude, vec!["memory.*", "plan.other-clans/**"]);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 2: extends löst auf (Basis-Sektionen werden übernommen)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_extends_inherits_parent_sections() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.base@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.child@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }

[[sections]]
name = "history.tail"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved = resolve_context_program(&id("harwness.context.child@1")?, &layers, now())?;

        let names: Vec<&str> = resolved.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["goal.invariants", "history.tail"]);
        assert!(resolved.trace.steps.iter().any(|s| s.kind == "base"));
        Ok(())
    }

    #[test]
    fn resolve_missing_base_errors() -> TestResult {
        let id_val = id("harwness.context.nonexistent@1")?;
        let layers: Vec<(DefinitionLayer, RawContextProgramDefinition)> = vec![];
        let result = resolve_context_program(&id_val, &layers, now());
        assert!(matches!(
            result,
            Err(ContextProgramError::Resolution(
                DslError::MissingBase { .. }
            ))
        ));
        Ok(())
    }

    #[test]
    fn resolve_extends_missing_target_errors() -> TestResult {
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.orphan@1"
version = "1.0.0"
extends = { id = "harwness.context.nonexistent@1" }
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, child)];
        let result = resolve_context_program(&id("harwness.context.orphan@1")?, &layers, now());
        assert!(matches!(
            result,
            Err(ContextProgramError::Resolution(
                DslError::MissingBase { .. }
            ))
        ));
        Ok(())
    }

    #[test]
    fn resolve_direct_extends_cycle_errors() -> TestResult {
        let cyclic = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.self-cycle@1"
version = "1.0.0"
extends = { id = "harwness.context.self-cycle@1" }
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, cyclic)];
        let result = resolve_context_program(&id("harwness.context.self-cycle@1")?, &layers, now());
        assert!(matches!(
            result,
            Err(ContextProgramError::Resolution(
                DslError::InheritanceCycle { .. }
            ))
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 3: ein Patch überschreibt gezielt
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_patch_overwrites_targeted_section() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.overridden@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
strength = "normal"
detail = "summary"

[[sections]]
name = "history.tail"
"#,
        )?;
        let patch_layer = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.overridden@1"
version = "1.1.0"

[patch.sections]
append = [
    { name = "goal.invariants", strength = "must-include", detail = "full", trust = "instruction" },
]
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::Project, patch_layer),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.overridden@1")?, &layers, now())?;

        // Position preserved (index 0), content overwritten.
        assert_eq!(resolved.sections.len(), 2);
        assert_eq!(resolved.sections[0].name, "goal.invariants");
        assert_eq!(resolved.sections[0].strength, SectionStrength::MustInclude);
        assert_eq!(resolved.sections[0].detail, harw_context::DetailMode::Full);
        assert_eq!(
            resolved.sections[0].trust,
            harw_context::TrustClass::Instruction
        );
        assert_eq!(resolved.sections[1].name, "history.tail");
        assert_eq!(resolved.version.0, semver::Version::new(1, 1, 0));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 4: ein Mixin fügt hinzu
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_mixin_adds_section() -> TestResult {
        let mixin = parse(
            r#"
schema = "harwness.context.mixin/v1"
id = "harwness.context.mixin.audit-trail@1"
version = "1.0.0"

[[sections]]
name = "audit.trail"
"#,
        )?;
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.with-mixin@1"
version = "1.0.0"
mixins = [{ id = "harwness.context.mixin.audit-trail@1" }]

[[sections]]
name = "goal.invariants"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.with-mixin@1")?, &layers, now())?;

        let names: Vec<&str> = resolved.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["audit.trail", "goal.invariants"]);
        assert!(resolved.trace.steps.iter().any(|s| s.kind == "mixin"));
        Ok(())
    }

    #[test]
    fn resolve_missing_mixin_errors() -> TestResult {
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.bad-mixin@1"
version = "1.0.0"
mixins = [{ id = "harwness.context.mixin.nonexistent@1" }]
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, target)];
        let result = resolve_context_program(&id("harwness.context.bad-mixin@1")?, &layers, now());
        assert!(matches!(
            result,
            Err(ContextProgramError::Resolution(
                DslError::MissingMixin { .. }
            ))
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 5: eine Sektion außerhalb der Decke wird abgewiesen
    // -----------------------------------------------------------------------

    #[test]
    fn admits_program_rejects_section_outside_ceiling() -> TestResult {
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.overreaching@1"
version = "1.0.0"

[[sections]]
name = "plan.other-clans"
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, target)];
        let resolved =
            resolve_context_program(&id("harwness.context.overreaching@1")?, &layers, now())?;

        let narrow_ceiling = ceiling(&["history.tail"], "data")?;
        let result = narrow_ceiling.admits_program(&resolved);
        assert!(matches!(
            result,
            Err(ProgramCeilingViolation::SectionNotAllowed { .. })
        ));
        Ok(())
    }

    #[test]
    fn resolve_context_program_within_ceiling_rejects_and_does_not_admit() -> TestResult {
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.overreaching2@1"
version = "1.0.0"

[[sections]]
name = "plan.other-clans"
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, target)];
        let narrow_ceiling = ceiling(&["history.tail"], "data")?;

        let result = resolve_context_program_within_ceiling(
            &id("harwness.context.overreaching2@1")?,
            &layers,
            &narrow_ceiling,
            now(),
        );
        assert!(matches!(
            result,
            Err(ContextProgramError::RejectedByCeiling(_))
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 6: eine höhere Vertrauensklasse als die Decke zulässt wird abgewiesen
    // -----------------------------------------------------------------------

    #[test]
    fn admits_program_rejects_trust_above_ceiling() -> TestResult {
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.too-trusting@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
trust = "instruction"
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, target)];
        let resolved =
            resolve_context_program(&id("harwness.context.too-trusting@1")?, &layers, now())?;

        // Ceiling allows the section by name, but caps trust at "data" —
        // strictly below "instruction" by `trust_rank`, not by derived `Ord`
        // (see module doc "Die Vertrauensfalle" in harw_context::fragment).
        let low_trust_ceiling = ceiling(&["goal.invariants"], "data")?;
        let result = low_trust_ceiling.admits_program(&resolved);
        assert!(matches!(
            result,
            Err(ProgramCeilingViolation::TrustExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn admits_program_accepts_section_within_ceiling() -> TestResult {
        let target = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.well-behaved@1"
version = "1.0.0"

[[sections]]
name = "history.tail"
trust = "evidence"
"#,
        )?;
        let layers = vec![(DefinitionLayer::BuiltIn, target)];
        let resolved =
            resolve_context_program(&id("harwness.context.well-behaved@1")?, &layers, now())?;

        let generous_ceiling = ceiling(&["history.tail"], "instruction")?;
        assert_eq!(generous_ceiling.admits_program(&resolved), Ok(()));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 7: die Sektionsreihenfolge bleibt beim Auflösen erhalten
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_preserves_section_order_across_extends_and_patch() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.ordered-base@1"
version = "1.0.0"

[[sections]]
name = "a"

[[sections]]
name = "b"
"#,
        )?;
        let overlay = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.ordered-child@1"
version = "1.0.0"
extends = { id = "harwness.context.ordered-base@1" }

[[sections]]
name = "c"
"#,
        )?;
        let patch_layer = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.ordered-child@1"
version = "1.1.0"

[patch.sections]
append = [{ name = "d" }]
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, overlay),
            (DefinitionLayer::Project, patch_layer),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.ordered-child@1")?, &layers, now())?;

        let names: Vec<&str> = resolved.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 8: ein unbekannter Schlüssel wird abgewiesen (deny_unknown_fields)
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let src = r#"
schema = "harwness.context/v1"
id = "harwness.context.strict@1"
version = "1.0.0"
unexpected_field = true
"#;
        let result: Result<RawContextProgramDefinition, _> = toml::from_str(src);
        assert!(result.is_err(), "unknown top-level key should be rejected");
    }

    #[test]
    fn unknown_section_key_is_rejected() {
        let src = r#"
name = "history.tail"
unexpected = "nope"
"#;
        let result: Result<RawContextSectionSpec, _> = toml::from_str(src);
        assert!(result.is_err(), "unknown section key should be rejected");
    }

    // -----------------------------------------------------------------------
    // Test 9: dasselbe Programm ergibt dieselbe SnapshotId; ein geändertes eine andere
    // -----------------------------------------------------------------------
    //
    // A context program is referenced from an agent definition only via the
    // pre-existing opaque `context_policy` label (`executable::ContextProgram`,
    // unchanged by this node). This proves that guarantee still holds end to
    // end: the label participates in `ExecutableAgentIr::snapshot_id()`
    // (`executable.rs` §"context_program"), so two agents referencing
    // different context-program ids never collide, and referencing the same
    // one twice is fully deterministic.

    fn agent_with_context_policy(
        policy_id: &str,
    ) -> TestResult<crate::executable::ExecutableAgentIr> {
        let src = format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.snapshot-probe@1"
version = "1.0.0"
role = "worker"
specialization = "snapshot-probe"

[context]
policy = "{policy_id}"
"#
        );
        let raw = crate::parse::parse_toml(&src).map_err(ctx("agent fixture should parse"))?;
        let agent_id = DefinitionId::parse("harwness.agent.snapshot-probe@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = crate::resolve::resolve_definition(&agent_id, &layers, now())
            .map_err(ctx("agent fixture should resolve"))?;
        crate::executable::lower(&resolved).map_err(ctx("agent fixture should lower"))
    }

    #[test]
    fn same_context_program_label_yields_same_snapshot_id() -> TestResult {
        let ir1 = agent_with_context_policy("harwness.context.security-triage@1")?;
        let ir2 = agent_with_context_policy("harwness.context.security-triage@1")?;
        assert_eq!(ir1.snapshot_id(), ir2.snapshot_id());
        Ok(())
    }

    #[test]
    fn changed_context_program_label_yields_different_snapshot_id() -> TestResult {
        let ir1 = agent_with_context_policy("harwness.context.security-triage@1")?;
        let ir2 = agent_with_context_policy("harwness.context.security-triage@2")?;
        assert_ne!(ir1.snapshot_id(), ir2.snapshot_id());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Extra: exclude patch reuses crate::merge::apply_merge_op unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_exclude_patch_appends_via_shared_merge_op() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.exclude-base@1"
version = "1.0.0"
exclude = ["memory.*"]
"#,
        )?;
        let patch_layer = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.exclude-base@1"
version = "1.1.0"

[patch.exclude]
append = ["secrets/**"]
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::Project, patch_layer),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.exclude-base@1")?, &layers, now())?;

        assert_eq!(resolved.exclude, vec!["memory.*", "secrets/**"]);
        Ok(())
    }

    #[test]
    fn ceiling_fixture_helper_builds_expected_set() -> TestResult {
        // Sanity check on the test helper itself: proves the BTreeSet content,
        // independent from `admits_program`.
        let c = ceiling(&["a", "b"], "evidence")?;
        let expected: BTreeSet<harw_context::SectionName> = ["a", "b"]
            .into_iter()
            .map(harw_context::SectionName::try_new)
            .collect::<Result<_, _>>()
            .map_err(ctx("SectionName::try_new should succeed"))?;
        assert_eq!(c.sections, expected);
        assert_eq!(c.max_trust, harw_context::TrustClass::Evidence);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 10 (AW6-05-Befund): `extends` erzwingt den Schnitt bei `trust` —
    // der wichtigste Test dieses Knotens.
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_rejects_trust_escalation_across_extends() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-base@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
trust = "data"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-child-up@1"
version = "1.0.0"
extends = { id = "harwness.context.trust-base@1" }

[[sections]]
name = "goal.invariants"
trust = "instruction"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let result =
            resolve_context_program(&id("harwness.context.trust-child-up@1")?, &layers, now());

        match result {
            Err(ContextProgramError::TrustEscalation {
                section,
                base_trust,
                declared_trust,
                ..
            }) => {
                assert_eq!(section, "goal.invariants");
                assert_eq!(base_trust, harw_context::TrustClass::Data);
                assert_eq!(declared_trust, harw_context::TrustClass::Instruction);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected TrustEscalation, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn resolve_rejects_trust_escalation_via_mixin() -> TestResult {
        // The escalation attempt can arrive through a mixin, not only through
        // the child's own `[[sections]]` — the check compares the FINAL
        // per-name trust after mixins AND own sections are merged in, so
        // neither path is a loophole.
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-base-mixin@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
trust = "data"
"#,
        )?;
        let mixin = parse(
            r#"
schema = "harwness.context.mixin/v1"
id = "harwness.context.mixin.over-trusting@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
trust = "instruction"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-child-mixin@1"
version = "1.0.0"
extends = { id = "harwness.context.trust-base-mixin@1" }
mixins = [{ id = "harwness.context.mixin.over-trusting@1" }]
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, child),
        ];
        let result =
            resolve_context_program(&id("harwness.context.trust-child-mixin@1")?, &layers, now());
        assert!(matches!(
            result,
            Err(ContextProgramError::TrustEscalation { .. })
        ));
        Ok(())
    }

    #[test]
    fn resolve_accepts_trust_reduction_across_extends() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-base-down@1"
version = "1.0.0"

[[sections]]
name = "goal.invariants"
trust = "instruction"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.trust-child-down@1"
version = "1.0.0"
extends = { id = "harwness.context.trust-base-down@1" }

[[sections]]
name = "goal.invariants"
trust = "data"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.trust-child-down@1")?, &layers, now())
                .map_err(ctx("lowering trust across extends must be accepted"))?;
        assert_eq!(resolved.sections[0].trust, harw_context::TrustClass::Data);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 11: der `exclude`-Fußboden der `extends`-Basis bleibt unentfernbar.
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_extends_exclude_floor_is_unremovable() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.exclude-floor-base@1"
version = "1.0.0"
exclude = ["credential.*", "secret.*"]
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.exclude-floor-child@1"
version = "1.0.0"
extends = { id = "harwness.context.exclude-floor-base@1" }
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved = resolve_context_program(
            &id("harwness.context.exclude-floor-child@1")?,
            &layers,
            now(),
        )?;

        // The child declares no `exclude` of its own at all, yet the base's
        // floor survives unabridged — there is no mechanism (short of a
        // `patch` on the SAME id, which does not apply across `extends`) that
        // could strike it.
        assert_eq!(resolved.exclude, vec!["credential.*", "secret.*"]);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Tests 12-14: die additiv belassenen Achsen bleiben additiv (Regressionsschutz,
    // damit niemand sie versehentlich verschärft).
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_allows_detail_escalation_across_extends() -> TestResult {
        // `detail` is deliberately NOT cut: "more text" is not a rights
        // expansion (see the module doc). `plan.toml`/`triage.toml` (AW6-05)
        // rely on exactly this to raise `history.tail` from `summary` to `full`.
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.detail-base@1"
version = "1.0.0"

[[sections]]
name = "history.tail"
detail = "summary"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.detail-child@1"
version = "1.0.0"
extends = { id = "harwness.context.detail-base@1" }

[[sections]]
name = "history.tail"
detail = "full"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.detail-child@1")?, &layers, now())
                .map_err(ctx(
                    "raising detail across extends is deliberately additive",
                ))?;
        assert_eq!(resolved.sections[0].detail, harw_context::DetailMode::Full);
        Ok(())
    }

    #[test]
    fn resolve_allows_strength_escalation_across_extends() -> TestResult {
        // `strength` only changes assembly priority for an already-admitted,
        // already trust-checked section — no security reason to cut it.
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.strength-base@1"
version = "1.0.0"

[[sections]]
name = "repo.tree"
strength = "normal"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.strength-child@1"
version = "1.0.0"
extends = { id = "harwness.context.strength-base@1" }

[[sections]]
name = "repo.tree"
strength = "must-include"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.strength-child@1")?, &layers, now())
                .map_err(ctx(
                    "raising strength across extends is deliberately additive",
                ))?;
        assert_eq!(resolved.sections[0].strength, SectionStrength::MustInclude);
        Ok(())
    }

    #[test]
    fn resolve_allows_new_section_with_high_trust_across_extends() -> TestResult {
        // A section name the base never declared carries no inherited floor —
        // the base has no opinion on it, so declaring it at any trust class is
        // admissible specialization, not an expansion of a base decision.
        // Mirrors `plan.toml`/`web.fetch_allowlist` (AW6-05), which add
        // brand-new `instruction`-trust sections.
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.new-section-base@1"
version = "1.0.0"

[[sections]]
name = "task.objective"
trust = "instruction"
"#,
        )?;
        let child = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.new-section-child@1"
version = "1.0.0"
extends = { id = "harwness.context.new-section-base@1" }

[[sections]]
name = "goal.invariants"
trust = "instruction"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, child),
        ];
        let resolved =
            resolve_context_program(&id("harwness.context.new-section-child@1")?, &layers, now())
                .map_err(ctx(
                "a brand-new section name carries no inherited trust ceiling",
            ))?;
        assert_eq!(resolved.sections.len(), 2);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 15: das korrigierte "Vollständige Beispiel" der Moduldoku parst
    // tatsächlich (§ Bericht: das alte Beispiel platzierte `exclude` nach den
    // `[[sections]]`-Blöcken, was in TOML einem Array-of-Tables-Feld zum Opfer
    // gefallen wäre — hier wortgleich zur Moduldoku, aber als echter Test statt
    // eines unkompilierten ```toml-Blocks).
    // -----------------------------------------------------------------------

    #[test]
    fn full_example_from_module_doc_parses() -> TestResult {
        let src = r#"
schema = "harwness.context/v1"
id = "harwness.context.security-triage@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }
mixins = [{ id = "harwness.context.mixin.audit-trail@1" }]

exclude = ["memory.*", "plan.other-clans/**"]

[[sections]]
name = "goal.invariants"
strength = "must-include"
detail = "full"
trust = "instruction"

[[sections]]
name = "history.tail"
detail = "summary"

[[sections]]
name = "plan.current"
strength = "must-include"
detail = "references"

[patch.sections]
append = [
    { name = "goal.invariants", strength = "must-include", detail = "full", trust = "instruction" },
]
remove = ["history.tail"]

[patch.exclude]
append = ["secrets/**"]
"#;
        let raw = parse(src)?;
        assert_eq!(raw.sections.len(), 3);
        assert_eq!(raw.exclude, vec!["memory.*", "plan.other-clans/**"]);
        assert!(!raw.patch.is_empty());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test 16: die zehn bestehenden Programmdateien (harw-registry-defaults,
    // AW6-05) bleiben unter der neuen `trust`-Schnitt-Regel gültig. Reproduziert
    // hier die BEIDEN Programme, die eine geerbte Sektion (`history.tail`)
    // namensgleich überschreiben (`plan` hebt `detail` an, `research-web` senkt
    // es ab) — keines der zehn ändert dabei `trust` gegenüber der Basis
    // (`evidence` bleibt `evidence`), daher bricht keines an dieser Regel.
    // -----------------------------------------------------------------------

    #[test]
    fn registry_default_base_and_plan_still_resolve_under_trust_cut() -> TestResult {
        let base = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.base@1"
version = "1.0.0"
exclude = ["credential.*", "secret.*", "sibling_transcripts", "full_parent_transcript"]

[[sections]]
name = "task.objective"
strength = "must-include"
detail = "full"
trust = "instruction"

[[sections]]
name = "history.tail"
strength = "must-include"
detail = "summary"
trust = "evidence"
"#,
        )?;
        let plan = parse(
            r#"
schema = "harwness.context/v1"
id = "harwness.context.plan@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }
exclude = ["web.*"]

[[sections]]
name = "history.tail"
strength = "must-include"
detail = "full"
trust = "evidence"

[[sections]]
name = "goal.invariants"
strength = "must-include"
detail = "full"
trust = "instruction"

[[sections]]
name = "plan.current"
strength = "must-include"
detail = "full"
trust = "evidence"
"#,
        )?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, plan),
        ];
        let resolved = resolve_context_program(&id("harwness.context.plan@1")?, &layers, now())
            .map_err(ctx(
                "plan.toml only raises `detail` (additive), never `trust`, on history.tail",
            ))?;

        let history = resolved
            .sections
            .iter()
            .find(|s| s.name == "history.tail")
            .ok_or(TestError::Missing(
                "history.tail must survive the extends merge",
            ))?;
        assert_eq!(history.detail, harw_context::DetailMode::Full);
        assert_eq!(history.trust, harw_context::TrustClass::Evidence);
        Ok(())
    }
}
