//! Kern-Contract für ausführbare Operationen im Harness.
//!
//! Dieses Modul definiert alle primitiven Typen und den [`Operation`]-Trait,
//! der die gemeinsame Schnittstelle für alle konkreten Operationen bildet.
//! Adapter (Command, ModelTool) sind NICHT Teil dieses Moduls — sie werden
//! von separaten Agenten implementiert und koppeln sich gegen diesen Contract.
//!
//! # Schlüsseltypen
//! - [`OperationDomain`] — thematische Gruppierung für Hilfe/Discovery
//! - [`PermissionTier`] — geordnete Berechtigungsstufen
//! - [`Surface`] — deklarierte Expositionsfläche (Command / ModelTool / Web / AgentTool)
//! - [`WebMethod`] — explizite HTTP-Methode einer `Surface::Web`-Route (F-031)
//! - [`OperationMeta`] — statische Metadaten einer Operation
//! - [`BusyAvailability`] — Verfügbarkeit einer Operation während eines laufenden Turns
//! - [`OpInput`] — fläche-neutrale Eingabe
//! - [`OpOutput`] — fläche-neutrales Ergebnis (Text plus optionale strukturierte Nutzlast, F-222)
//! - [`Operation`] — ausführbarer Kern-Trait
//! - [`ArgsSchemaFn`] — Zeiger auf das Argument-Schema einer Operation
//! - [`ArgsSchemaProbe`] — Typ-Sonde, mit der `#[operation]` das Schema bedingt bindet
//!
//! # Namenskonvention für Infrastruktur-Operationen
//! Infrastruktur-Operationen (Auth/Crypto-Hub, NetSec, SecurityHub) heißen
//! verbindlich `infra.<area>.<noun-plural>.<verb>`, z. B.
//! `infra.auth.keys.list`, `infra.auth.keys.rotate`,
//! `infra.network.nodes.drain`, `infra.network.routes.update`,
//! `infra.security.incidents.list`. Das Nomen steht immer im Plural — auch
//! wenn das Verb auf genau ein Objekt wirkt (Drain *eines* Knotens heißt
//! `infra.network.nodes.drain`, nicht `infra.network.node.drain`). Damit ist
//! die Schreibweise aus Masterplan v2 §1.2 kanonisch; das Singular-Beispiel in
//! §12.3 ist überholt. Die Domäne solcher Operationen ist eine der
//! Infrastruktur-Domänen [`OperationDomain::Identity`],
//! [`OperationDomain::Network`], [`OperationDomain::Security`] oder
//! [`OperationDomain::Crypto`] — nicht `CatalogConfig` oder `Misc` (§31).
//!
//! # Nebenläufigkeit
//! [`Operation`] ist `Send + Sync`, damit Implementierungen in `Arc<dyn Operation>`
//! über Thread-Grenzen geteilt werden können. [`OpFuture`] ist `Send`.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use harw_tools::JsonSchema;

use crate::context::OpContext;
use crate::op_schema::OpArgsSchema;

// ── Domänen & Berechtigungen ────────────────────────────────────────────────

/// Thematische Gruppierung einer Operation für Hilfe-Texte und Discovery.
///
/// # Beschreibung
/// Jede Operation gehört genau einer Domäne an. Die Domäne steuert, in welcher
/// Sektion einer `/help`-Ausgabe die Operation erscheint und wie sie in einem
/// Berechtigungsmodell vorgefiltert werden kann.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::OperationDomain;
///
/// let d = OperationDomain::Session;
/// assert_eq!(d, OperationDomain::Session);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationDomain {
    /// Session-Verwaltung: Starten, Beenden, Auflisten von Sessions.
    Session,
    /// Agenten-Verwaltung: Registrierung, Statusabfrage, Rollenzuweisung.
    Agents,
    /// Ausführungssteuerung: Jobs starten, abbrechen, überwachen.
    Execution,
    /// Katalog- und Konfigurationsverwaltung.
    CatalogConfig,
    /// Wissensbasis-Operationen: Suche, Indizierung, Annotation.
    Knowledge,
    /// Identitäten und Schlüssel-Metadaten: Geräte, Principals, Auth-Hub
    /// (z. B. `infra.auth.keys.list`, `infra.auth.devices.list`).
    Identity,
    /// Netzwerk-Topologie: Knoten, Routen, Drain (z. B. `infra.network.nodes.drain`).
    Network,
    /// Sicherheitslage: Incidents, Policies, Containment
    /// (z. B. `infra.security.incidents.list`).
    Security,
    /// Kryptographische Steuerung: Schlüsselrotation, Verschlüsselungsformate
    /// (z. B. `infra.auth.keys.rotate`).
    Crypto,
    /// Sonstige Operationen ohne spezifische Domäne.
    Misc,
}

impl OperationDomain {
    /// Alle Domänen in kanonischer Reihenfolge (Hilfe-/Navigations-Sortierung).
    pub const ALL: [Self; 10] = [
        Self::Session,
        Self::Agents,
        Self::Execution,
        Self::CatalogConfig,
        Self::Knowledge,
        Self::Identity,
        Self::Network,
        Self::Security,
        Self::Crypto,
        Self::Misc,
    ];

    /// Stabiler `snake_case`-Slug; identisch mit dem Serde-Namen und dem
    /// `domain = "..."`-Literal des `#[operation]`-Makros.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::operation::OperationDomain;
    ///
    /// assert_eq!(OperationDomain::CatalogConfig.as_str(), "catalog_config");
    /// assert_eq!(OperationDomain::Crypto.as_str(), "crypto");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Agents => "agents",
            Self::Execution => "execution",
            Self::CatalogConfig => "catalog_config",
            Self::Knowledge => "knowledge",
            Self::Identity => "identity",
            Self::Network => "network",
            Self::Security => "security",
            Self::Crypto => "crypto",
            Self::Misc => "misc",
        }
    }

    /// Parst einen Slug (siehe [`Self::as_str`]); `None` für unbekannte Werte.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == slug)
    }

    /// Abgeleitete Hilfe-Kategorie, wenn `#[operation]` keine explizite
    /// `category` angibt. Muss mit dem Makro-Mapping in
    /// `harw-macros/src/operation.rs` übereinstimmen.
    #[must_use]
    pub const fn default_category(self) -> OperationCategory {
        match self {
            Self::Session => OperationCategory::Session,
            Self::Agents => OperationCategory::Agent,
            Self::Execution | Self::Identity | Self::Network | Self::Security | Self::Crypto => {
                OperationCategory::System
            }
            Self::CatalogConfig => OperationCategory::Model,
            Self::Knowledge => OperationCategory::Knowledge,
            Self::Misc => OperationCategory::Misc,
        }
    }
}

impl std::fmt::Display for OperationDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Command grouping for user-facing /help rendering.
///
/// This is orthogonal to [`OperationDomain`] (internal thematic taxonomy):
/// `category` drives how commands appear grouped in TUI help output;
/// `domain` drives dispatch-side policy checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OperationCategory {
    Model,
    Agent,
    Session,
    System,
    Knowledge,
    #[default]
    Misc,
}

impl OperationCategory {
    /// Stable lowercase slug for serialization and CLI parsing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Agent => "agent",
            Self::Session => "session",
            Self::System => "system",
            Self::Knowledge => "knowledge",
            Self::Misc => "misc",
        }
    }
}

/// Verfügbarkeit einer Operation während eines laufenden ("busy") Turns.
///
/// # Beschreibung
/// Solange ein Turn läuft, gilt je Befehl genau eine von drei Klassen:
/// - [`DeferredUntilTurnEnd`] (Standard): der Befehl wird eingereiht und erst
///   nach Turn-Ende ausgeführt — für alles, was Sitzung, Verlauf oder Wissen
///   schreibt.
/// - [`Immediate`]: der Befehl läuft sofort, nebenläufig zum Turn — nur für
///   reine Lese-/Steuerbefehle ohne Session-Mutation über die Turn-Grenze.
/// - [`Staged`]: der Befehl läuft sofort, merkt seine Änderung aber nur über
///   den `SessionController` bzw. Config-Zellen vor; sie gilt ab dem nächsten
///   Turn (z. B. `/model switch`, `/effort`, `/mode`).
///
/// Eine Operation kann die Klasse je Unterbefehl überschreiben, siehe
/// [`BusySubcommand`] und [`Operation::busy_subcommands`].
///
/// [`DeferredUntilTurnEnd`]: BusyAvailability::DeferredUntilTurnEnd
/// [`Immediate`]: BusyAvailability::Immediate
/// [`Staged`]: BusyAvailability::Staged
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::BusyAvailability;
///
/// assert_eq!(BusyAvailability::default(), BusyAvailability::DeferredUntilTurnEnd);
/// assert!(BusyAvailability::Staged.runs_during_turn());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BusyAvailability {
    /// Standard: Der Befehl wird während eines laufenden Turns eingereiht und
    /// erst nach Turn-Ende ausgeführt.
    #[default]
    DeferredUntilTurnEnd,
    /// Der Befehl darf während eines laufenden ("busy") Turns sofort
    /// ausgeführt werden — nur für reine Lese-/Steuerbefehle ohne
    /// Session-Mutation über die Turn-Grenze gedacht.
    Immediate,
    /// Der Befehl läuft während eines Turns sofort, seine Änderung wird aber
    /// nur vorgemerkt (Controller/Config-Zelle) und gilt ab dem nächsten Turn.
    Staged,
}

impl BusyAvailability {
    /// `true`, wenn ein Befehl dieser Klasse während eines laufenden Turns
    /// sofort ausgeführt wird ([`Self::Immediate`] und [`Self::Staged`]).
    ///
    /// # Rückgabe
    /// `false` nur für [`Self::DeferredUntilTurnEnd`].
    #[must_use]
    pub const fn runs_during_turn(self) -> bool {
        !matches!(self, Self::DeferredUntilTurnEnd)
    }
}

/// Überschreibt die [`BusyAvailability`] einer Operation für einen
/// Unterbefehl (erstes Argument-Token der Befehlszeile).
///
/// # Beschreibung
/// `subcommand = None` steht für den Aufruf **ohne** Argument (bare Form).
/// Unterbefehle ohne Eintrag erben [`OperationMeta::busy`]. Das
/// `#[operation]`-Makro erzeugt die Tabelle aus dem Schlüssel
/// `busy_subcommands = "show=immediate, switch=staged, -=immediate"` (`-` ist
/// die bare Form).
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::{BusyAvailability, BusySubcommand};
///
/// const TABLE: &[BusySubcommand] = &[
///     BusySubcommand::new(Some("show"), BusyAvailability::Immediate),
///     BusySubcommand::new(None, BusyAvailability::Immediate),
/// ];
/// let class = BusySubcommand::resolve(BusyAvailability::Staged, TABLE, Some("show"));
/// assert_eq!(class, BusyAvailability::Immediate);
/// let class = BusySubcommand::resolve(BusyAvailability::Staged, TABLE, Some("switch"));
/// assert_eq!(class, BusyAvailability::Staged);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusySubcommand {
    /// Erstes Argument-Token; `None` für die bare Form.
    pub subcommand: Option<&'static str>,
    /// Klasse dieses Unterbefehls.
    pub busy: BusyAvailability,
}

impl BusySubcommand {
    /// Erzeugt einen Tabelleneintrag (in `const`-Tabellen verwendbar).
    #[must_use]
    pub const fn new(subcommand: Option<&'static str>, busy: BusyAvailability) -> Self {
        Self { subcommand, busy }
    }

    /// Löst die Klasse einer Befehlszeile auf.
    ///
    /// # Argumente
    /// - `default`: [`OperationMeta::busy`] der Operation.
    /// - `table`: die Unterbefehls-Tabelle der Operation.
    /// - `first_arg`: erstes Argument-Token der Zeile, `None` ohne Argument.
    ///
    /// # Rückgabe
    /// Die Klasse des passenden Eintrags (exakter Vergleich), sonst `default`.
    #[must_use]
    pub fn resolve(
        default: BusyAvailability,
        table: &[BusySubcommand],
        first_arg: Option<&str>,
    ) -> BusyAvailability {
        table
            .iter()
            .find(|entry| entry.subcommand == first_arg)
            .map_or(default, |entry| entry.busy)
    }
}

/// Geordnete Mindest-Berechtigungsstufe für eine Operation.
///
/// # Beschreibung
/// Re-Export von [`harw_types::PermissionTier`]: die Definition zog in Welle
/// W0b nach `harw-types` (`harw-types/src/principal.rs`), damit
/// `harw_types::Principal` denselben Typ trägt. Pfad, Varianten und Ordnung
/// bleiben für alle Nutzer von `harw_operations::operation::PermissionTier`
/// unverändert.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::PermissionTier;
///
/// assert!(PermissionTier::Maintainer > PermissionTier::Operator);
/// ```
pub use harw_types::PermissionTier;

// ── Oberflächen ─────────────────────────────────────────────────────────────

/// Sichtbarkeit eines als `/command` exponierten Command-Adapters.
///
/// # Beschreibung
/// Steuert, ob der Command in der TUI, in Channels, oder in beidem erscheint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandVisibility {
    /// Nur in der TUI sichtbar; Channel-Adapter ignoriert diesen Command.
    TuiOnly,
    /// In TUI und Channels mit identischem Verhalten verfügbar.
    ChannelParity,
    /// In Channels verfügbar, aber mit reduziertem Funktionsumfang (z. B.
    /// keine interaktiven Prompts).
    ChannelReduced,
}

/// Approval-Politik für einen Modell-Tool-Adapter.
///
/// # Beschreibung
/// Bestimmt, ob ein Benutzer vor der Ausführung explizit zustimmen muss.
///
/// # Description
///
/// The hardening document (§28) specifies that approval should be triggered by
/// scope violations, effect classes, and risk classes — not just a binary
/// None/Always toggle. The runtime-level approval engine consults the
/// `TurnScope` to determine whether an invocation needs approval based on its
/// requested effects vs. the agent's delegated scope.
///
/// # Security invariant
///
/// Approval never expands authority. An approved action still operates within
/// the agent's monotone authority envelope — approval only permits an
/// in-scope action that would otherwise require explicit confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Keine Bestätigung erforderlich; das Modell kann direkt ausführen.
    None,
    /// Jede Ausführung erfordert explizite Benutzer-Bestätigung.
    Always,
    /// Require approval when the invocation touches resources outside the
    /// agent's declared read scope or write lease.
    RequireForScope,
    /// Require approval when the invocation produces effects that match a
    /// declared effect class (e.g. network access, process spawn, file delete).
    RequireForEffect,
    /// Require approval when the operation's risk class meets or exceeds a
    /// threshold (e.g. all `High` and `Critical` risk operations).
    RequireForRiskClass,
}

/// Eine deklarierte Expositionsfläche einer Operation.
///
/// # Beschreibung
/// Eine Operation kann null, eine oder mehrere Flächen deklarieren.
/// `Command` und `ModelTool` sind unabhängig und werden NICHT automatisch
/// aneinander gekoppelt — jede Fläche muss explizit in [`OperationMeta::surfaces`]
/// aufgeführt werden.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::{Surface, CommandVisibility, ApprovalPolicy};
///
/// let surfaces = vec![
///     Surface::Command { path: "/session/list", visibility: CommandVisibility::TuiOnly },
///     Surface::ModelTool { readonly: true, approval: ApprovalPolicy::None },
/// ];
/// assert_eq!(surfaces.len(), 2);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Exposition als `/slash`-Command in TUI oder Channel.
    ///
    /// - `path`: Befehlspfad, z. B. `"/session/list"` (statisch).
    /// - `visibility`: Steuert Sichtbarkeit je Ausgabe-Kanal.
    Command {
        /// Befehlspfad (statisch; darf keine Leerzeichen enthalten).
        path: &'static str,
        /// Sichtbarkeit in TUI vs. Channel.
        visibility: CommandVisibility,
    },

    /// Exposition als direkt vom Modell aufrufbares Tool.
    ///
    /// - `readonly`: `true` = keine Zustandsänderungen; erlaubt liberalere
    ///   Genehmigungsregeln.
    /// - `approval`: Approval-Politik vor jeder Ausführung.
    ModelTool {
        /// Gibt an, ob die Operation ausschließlich lesend ist.
        readonly: bool,
        /// Approval-Politik für Modell-initiierte Aufrufe.
        approval: ApprovalPolicy,
    },

    /// Exposition als Agent-as-Tool: das Modell ruft einen Child-Agent auf.
    ///
    /// Details siehe `docs/design/agents-as-tools.md`.
    /// Wichtigste Invariante: Child-Authority ⊆ Parent-Authority (monoton fallend).
    ///
    /// - `child_name`: Bezeichner der Child-Agent-Definition (aus einer späteren AgentRegistry).
    /// - `authority_reducer`: Kennung der Reducer-Funktion (Namen registriert in der Extension-Registry).
    /// - `budget_hint`: Freies Budget-Label ("8k", "unlimited", …) — bindende Enforcement erfolgt später.
    AgentTool {
        /// Name des Child-Agents (String-Literal).
        child_name: &'static str,
        /// Name der Reducer-Funktion, die die Sandbox monoton verkleinert.
        authority_reducer: &'static str,
        /// Frei formatierbares Budget-Hint (Token-/Zeit-/Call-Limit-Zeichen).
        budget_hint: &'static str,
    },

    /// Exposition als HTTP-Route über `harw-web` (Unix-Socket, Aufrufer über
    /// `SO_PEERCRED` identifiziert — kein Bearer-Token, kein zweiter
    /// Autoritätspfad).
    ///
    /// # Feldbegründung
    /// - `path`: HTTP-Pfad, z. B. `"/api/session/list"` (statisch, beginnt
    ///   mit `/`) — die einzige Angabe, die `harw-web` laut Auftrag
    ///   mindestens braucht, exakt wie `Command::path`.
    /// - `method`: **Explizite** HTTP-Methode (F-031, `x-findings-register-w1-w3.md`).
    ///   Früher wurde die Methode aus einem `readonly`-Flag abgeleitet
    ///   (`GET` für `readonly = true`, sonst `POST`) — das erlaubte
    ///   `analyze` (`harw-ops/src/analyze.rs:879`), sich trotz dauerhafter
    ///   Schreibwirkung als `readonly` zu deklarieren und dadurch eine
    ///   mutierende Operation über eine per Browser-Prefetch/`<img src>`/CSRF
    ///   auslösbare `GET`-Route zu exponieren. `method` entkoppelt die
    ///   HTTP-Semantik vollständig von jeder Lesbarkeits-Einschätzung der
    ///   Operation: jede `#[operation(web(...))]`-Deklaration muss die
    ///   Methode nennen, es gibt keinen impliziten Default.
    /// - `approval`: Wiederverwendet [`ApprovalPolicy`] — denselben Enum wie
    ///   `ModelTool::approval` — statt eine eigene Web-Genehmigungsachse zu
    ///   erfinden. Das ist keine Bequemlichkeit, sondern die Auflage „kein
    ///   zweiter Autoritätspfad": irreversible Aktionen bekommen über
    ///   `approval != ApprovalPolicy::None` genau dieselbe Behandlung wie am
    ///   Modell-Tool — `harw-web` selbst darf eine solche Operation nicht
    ///   ausführen, sondern muss sie an eine Genehmigung weiterreichen
    ///   (siehe `harw-web`-Moduldoku).
    ///
    /// Bewusst **kein** eigenes Sichtbarkeitsfeld (wie `CommandVisibility`):
    /// eine HTTP-Route kennt weder TUI noch Channel — dieser Begriff ist für
    /// eine dritte Fläche bedeutungslos.
    Web {
        /// HTTP-Pfad (statisch; beginnt mit `/`, keine Leerzeichen).
        path: &'static str,
        /// Explizite HTTP-Methode dieser Route; siehe [`WebMethod`] und die
        /// Feldbegründung oben (F-031).
        method: WebMethod,
        /// Approval-Politik vor jeder Ausführung — identisch zur
        /// Modell-Tool-Achse, keine eigene Web-Genehmigungsachse.
        approval: ApprovalPolicy,
    },
}

/// Explizite HTTP-Methode einer [`Surface::Web`]-Route.
///
/// # Beschreibung
/// Vor dieser Welle (C-OPS, F-031) leitete `harw-web` die HTTP-Methode einer
/// Route aus `Surface::Web`s `readonly`-Flag ab (`GET` für `readonly = true`,
/// sonst `POST`). Das ließ eine Operation, die sich fälschlich als `readonly`
/// deklariert — `analyze` in `harw-ops/src/analyze.rs:879` schreibt trotz
/// `readonly, web(...)` dauerhaft in den Plan-Store und startet Fan-out
/// (`harw-ops/src/analyze.rs:953-973`) —, über eine `GET`-Route erreichbar
/// sein, die ein Browser ohne jede Benutzerinteraktion auslöst (Prefetch,
/// `<img src>`, CSRF: `GET` gilt HTTP-semantisch als sicher/idempotent).
/// `WebMethod` macht die Methode zu einer eigenständigen, verpflichtenden
/// Angabe jeder `#[operation(web(...))]`-Deklaration, unabhängig von jeder
/// `readonly`-Einschätzung.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::WebMethod;
///
/// assert_ne!(WebMethod::Get, WebMethod::Post);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WebMethod {
    /// HTTP `GET` — nur für tatsächlich sichere, idempotente Operationen ohne
    /// Seiteneffekt zulässig.
    Get,
    /// HTTP `POST` — für jede Operation mit Seiteneffekt, unabhängig davon,
    /// ob ihr `model_tool`-`readonly`-Flag gesetzt ist.
    Post,
}

// ── Argument-Schema ──────────────────────────────────────────────────────────

/// Zeiger auf die Schema-Bauroutine des Argument-Typs einer Operation.
///
/// # Beschreibung
/// Entspricht [`OpArgsSchema::json_schema`] als Funktionszeiger. Der Trait ist
/// bewusst nicht objekt-sicher (assoziierte Funktion ohne `self`); ein
/// `fn`-Zeiger ist deshalb die einzige Form, in der ein Schema in einer
/// nicht-generischen Struktur wie [`OperationMeta`] abgelegt werden kann.
///
/// Der Zeiger ist `Copy`, `Send` und `Sync` — [`OperationMeta`] bleibt damit
/// unverändert billig klonbar und über Thread-Grenzen teilbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{OpArgsSchema, object_schema, string_schema};
/// use harw_operations::operation::ArgsSchemaFn;
///
/// struct StopArgs;
///
/// impl OpArgsSchema for StopArgs {
///     fn json_schema() -> harw_tools::JsonSchema {
///         object_schema(vec![("job_id", string_schema("Job-Kennung."))], &["job_id"])
///     }
/// }
///
/// let build: ArgsSchemaFn = <StopArgs as OpArgsSchema>::json_schema;
/// assert_eq!(build().required.as_deref(), Some(&["job_id".to_owned()][..]));
/// ```
pub type ArgsSchemaFn = fn() -> JsonSchema;

/// Typ-Sonde, über die `#[operation]` erfragt, ob ein Argument-Typ
/// [`OpArgsSchema`] implementiert — ohne Operationen zu brechen, deren
/// Argument-Typ den Trait **nicht** trägt.
///
/// # Beschreibung
/// Ein Proc-Makro kennt nur den *Namen* des Argument-Typs, nicht seine
/// Trait-Impls; es kann `<ArgsType as OpArgsSchema>::json_schema` deshalb nicht
/// unbedingt einsetzen — für einen Typ ohne das Derive wäre das ein
/// Compile-Fehler. `ArgsSchemaProbe` löst das mit autoref-basierter
/// Spezialisierung (stabiles Rust, dasselbe Verfahren wie in `anyhow`):
///
/// - [`DerivedArgsSchema`] ist für **`&ArgsSchemaProbe<T>`** implementiert und
///   verlangt `T: OpArgsSchema`.
/// - [`NoArgsSchema`] ist für **`ArgsSchemaProbe<T>`** implementiert, ohne
///   jede Schranke.
///
/// Beim Aufruf über eine Referenz (`(&probe).harw_args_schema()`) prüft die
/// Methodenauflösung zuerst `&ArgsSchemaProbe<T>`. Trägt `T` den Trait, gewinnt
/// [`DerivedArgsSchema`] und liefert `Some`; andernfalls ist der Kandidat nicht
/// anwendbar, die Auflösung dereferenziert eine Stufe weiter und landet bei
/// [`NoArgsSchema`] mit `None`. Beide Fälle übersetzen — es gibt keinen Pfad,
/// auf dem eine Operation ohne `OpArgs`-Derive nicht mehr kompiliert.
///
/// # Nebenläufigkeit
/// Zustandsloser Nulltyp (`PhantomData`); rein und aus beliebig vielen Threads
/// verwendbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{OpArgsSchema, object_schema, string_schema};
/// use harw_operations::operation::{ArgsSchemaProbe, DerivedArgsSchema as _, NoArgsSchema as _};
///
/// struct WithSchema;
/// impl OpArgsSchema for WithSchema {
///     fn json_schema() -> harw_tools::JsonSchema {
///         object_schema(vec![("path", string_schema("Pfad."))], &["path"])
///     }
/// }
///
/// struct WithoutSchema;
///
/// let with = ArgsSchemaProbe::<WithSchema>::new();
/// let with_ref: &ArgsSchemaProbe<WithSchema> = &with;
/// assert!(with_ref.harw_args_schema().is_some());
///
/// let without = ArgsSchemaProbe::<WithoutSchema>::new();
/// let without_ref: &ArgsSchemaProbe<WithoutSchema> = &without;
/// assert!(without_ref.harw_args_schema().is_none());
/// ```
pub struct ArgsSchemaProbe<T>(PhantomData<fn() -> T>);

impl<T> ArgsSchemaProbe<T> {
    /// Erzeugt eine Sonde für den Argument-Typ `T`.
    ///
    /// # Rückgabe
    /// Eine zustandslose `ArgsSchemaProbe<T>` (Nulltyp, keine Allokation).
    ///
    /// # Nebenläufigkeit
    /// Rein; aus beliebig vielen Threads aufrufbar.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::operation::ArgsSchemaProbe;
    ///
    /// let _probe = ArgsSchemaProbe::<()>::new();
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Default for ArgsSchemaProbe<T> {
    /// Wie [`ArgsSchemaProbe::new`] — die Sonde trägt keinen Zustand.
    fn default() -> Self {
        Self::new()
    }
}

/// Spezialisierter Sonden-Zweig: liefert das Schema eines Argument-Typs, der
/// [`OpArgsSchema`] implementiert.
///
/// # Beschreibung
/// Implementiert für `&ArgsSchemaProbe<T>` (eine Autoref-Stufe *vor*
/// [`NoArgsSchema`]) und damit nur anwendbar, wenn `T: OpArgsSchema` gilt.
/// Siehe [`ArgsSchemaProbe`] für das vollständige Auflösungsverfahren.
///
/// # Nebenläufigkeit
/// Rein; keine Sperren, kein gemeinsamer Zustand.
pub trait DerivedArgsSchema {
    /// Liefert `Some(<T as OpArgsSchema>::json_schema)`.
    ///
    /// # Rückgabe
    /// Immer `Some` — dieser Zweig wird nur gewählt, wenn der Argument-Typ den
    /// Trait trägt.
    fn harw_args_schema(&self) -> Option<ArgsSchemaFn>;
}

// Der spezialisierte Zweig liegt auf dem **Wert**-Typ, nicht auf der Referenz.
// Das ist die entscheidende Seite der Autoref-Spezialisierung: die
// Methodenauflösung probiert Kandidaten nach der Zahl nötiger Autoref-Schritte,
// und der Kandidat mit **weniger** Schritten gewinnt. Läge dieser Zweig auf
// `&ArgsSchemaProbe<T>` und der Rückfall auf `ArgsSchemaProbe<T>`, bekäme der
// Rückfall den kürzeren Weg — und die Sonde lieferte immer `None`, ohne dass
// der Compiler etwas meldet.
impl<T: OpArgsSchema> DerivedArgsSchema for ArgsSchemaProbe<T> {
    fn harw_args_schema(&self) -> Option<ArgsSchemaFn> {
        Some(<T as OpArgsSchema>::json_schema)
    }
}

/// Rückfall-Sonden-Zweig: greift für jeden Argument-Typ **ohne**
/// [`OpArgsSchema`]-Implementierung.
///
/// # Beschreibung
/// Implementiert für `ArgsSchemaProbe<T>` ohne Trait-Schranke. Die
/// Methodenauflösung erreicht diesen Zweig erst, nachdem
/// [`DerivedArgsSchema`] mangels `T: OpArgsSchema` verworfen wurde.
///
/// # Nebenläufigkeit
/// Rein; keine Sperren, kein gemeinsamer Zustand.
pub trait NoArgsSchema {
    /// Liefert `None` — für diesen Argument-Typ existiert kein Schema.
    ///
    /// # Rückgabe
    /// Immer `None`.
    fn harw_args_schema(&self) -> Option<ArgsSchemaFn>;
}

// Der Rückfall liegt auf der **Referenz** und braucht damit einen
// Autoref-Schritt mehr als [`DerivedArgsSchema`]. Er wird deshalb nur gewählt,
// wenn der spezialisierte Zweig mangels `T: OpArgsSchema` gar nicht anwendbar
// ist — genau das gewünschte Verhalten.
impl<T> NoArgsSchema for &ArgsSchemaProbe<T> {
    fn harw_args_schema(&self) -> Option<ArgsSchemaFn> {
        None
    }
}

// ── Metadaten ────────────────────────────────────────────────────────────────

/// Statische Metadaten, die eine Operation beschreiben.
///
/// # Beschreibung
/// `OperationMeta` wird von [`Operation::meta`] zurückgegeben und ist die
/// einzige Datenquelle für Registrierungen, Hilfe-Texte, Berechtigungsprüfungen
/// und Adapter-Konfiguration. Alle Felder sind statisch (`&'static str`) oder
/// cheaply klonbar.
///
/// # Argument-Schema
/// [`OperationMeta::args_schema`] ist optional und standardmäßig `None`.
/// Operationen, deren Argument-Typ `#[derive(OpArgs)]` trägt, bekommen den
/// Zeiger vom `#[operation]`-Makro gesetzt; alle anderen bleiben bei `None`.
/// Für neue Konstruktionsstellen steht [`OperationMeta::default`] als
/// Auffüllwert bereit (`..OperationMeta::default()`).
///
/// # Ausgabe-Schema
/// [`OperationMeta::output_schema`] ist das spiegelbildliche Gegenstück zu
/// [`OperationMeta::args_schema`] für die strukturierte Nutzlast in
/// [`OpOutput::data`] (F-222, `x-findings-register-w1-w3.md`). Das `#[operation]`-Makro
/// setzt es derzeit unbedingt auf `None` — es gibt (Stand W3/C-OPS) noch keine
/// Attribut-Syntax, mit der eine Operation ihren Ausgabetyp deklariert; das
/// Feld liegt bereit, damit eine spätere Welle es füllen kann, ohne den
/// Vertrag erneut zu brechen.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::{
///     OperationMeta, OperationDomain, OperationCategory, PermissionTier, Surface, CommandVisibility,
/// };
///
/// let meta = OperationMeta {
///     name: "session.list",
///     summary: "Listet alle aktiven Sessions auf.",
///     domain: OperationDomain::Session,
///     permission: PermissionTier::Observer,
///     surfaces: vec![
///         Surface::Command { path: "/session/list", visibility: CommandVisibility::TuiOnly },
///     ],
///     aliases: &[],
///     category: OperationCategory::Misc,
///     args_schema: None,
///     output_schema: None,
///     busy: Default::default(),
/// };
/// assert_eq!(meta.name, "session.list");
/// ```
#[derive(Clone, Debug)]
pub struct OperationMeta {
    /// Eindeutiger, maschinenlesbarer Name (z. B. `"session.list"`).
    pub name: &'static str,
    /// Kurze menschenlesbare Zusammenfassung (ein Satz, Deutsch).
    pub summary: &'static str,
    /// Thematische Zuordnung für Hilfe/Discovery.
    pub domain: OperationDomain,
    /// Mindest-Berechtigungsstufe, die ein Aufrufer benötigt.
    pub permission: PermissionTier,
    /// Alle deklarierten Expositionsflächen dieser Operation.
    pub surfaces: Vec<Surface>,
    /// Alternative command names — e.g. `["r", "reasoning"]` for `/effort`.
    pub aliases: &'static [&'static str],
    /// User-facing category for /help grouping.
    pub category: OperationCategory,
    /// Bauroutine des Argument-Schemas für die Modell-Tool-Fläche.
    ///
    /// `Some(build)` — der Argument-Typ der Operation implementiert
    /// [`OpArgsSchema`] (üblicherweise über `#[derive(OpArgs)]`); `build()`
    /// liefert ein **geschlossenes** Objekt-Schema mit allen Feldnamen.
    /// [`crate::adapter::model_tool::ModelToolProvider`] reicht es unverändert
    /// an das Modell weiter.
    ///
    /// `None` — kein Schema am Argument-Typ. Der Modell-Tool-Adapter fällt dann
    /// auf sein bekanntes Namens-Schema bzw. zuletzt auf ein offenes
    /// Objekt-Schema zurück (siehe
    /// [`crate::adapter::model_tool::model_tool_schema_for`]).
    pub args_schema: Option<ArgsSchemaFn>,
    /// Bauroutine des Ausgabe-Schemas für [`OpOutput::data`] (F-222).
    ///
    /// Derselbe Zeigertyp wie [`OperationMeta::args_schema`]
    /// (`fn() -> JsonSchema`), aber für die Struktur der optionalen
    /// strukturierten Nutzlast statt für die Eingabeargumente. `None` — der
    /// Normalfall, solange keine Operation ihr Ausgabeschema deklariert —
    /// bedeutet, dass eine strukturierte Web-Ansicht `data` ungetypt behandeln
    /// muss (Rohtext bleibt in [`OpOutput::text`] immer verfügbar).
    pub output_schema: Option<ArgsSchemaFn>,
    /// Verfügbarkeit dieser Operation während eines laufenden ("busy") Turns.
    ///
    /// `BusyAvailability::DeferredUntilTurnEnd` (Standard) — der Befehl wird
    /// eingereiht und erst nach Turn-Ende ausgeführt. `Immediate` — der
    /// Befehl darf während busy sofort ausgeführt werden; nur für reine
    /// Lese-/Steuerbefehle ohne Session-Mutation über die Turn-Grenze
    /// gedacht. `Staged` — läuft sofort, merkt die Änderung aber nur vor
    /// (gilt ab dem nächsten Turn). Überschreibungen je Unterbefehl liefert
    /// [`Operation::busy_subcommands`].
    pub busy: BusyAvailability,
}

impl Default for OperationMeta {
    /// Auffüllwert für die `..OperationMeta::default()`-Schreibweise.
    ///
    /// # Beschreibung
    /// Existiert ausschließlich, damit Konstruktionsstellen neue optionale
    /// Felder nicht einzeln aufzählen müssen. `name` und `summary` sind leer —
    /// ein so gebautes `OperationMeta` beschreibt **keine** gültige Operation
    /// und darf nie unverändert registriert werden. `permission` ist
    /// [`PermissionTier::Observer`], die niedrigste Stufe: ein vergessenes Feld
    /// weitet keine Rechte aus.
    ///
    /// # Rückgabe
    /// Ein `OperationMeta` ohne Name, ohne Flächen, ohne Aliase und ohne
    /// Argument- oder Ausgabe-Schema.
    ///
    /// # Nebenläufigkeit
    /// Rein; aus beliebig vielen Threads aufrufbar.
    fn default() -> Self {
        Self {
            name: "",
            summary: "",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: Vec::new(),
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::default(),
        }
    }
}

// ── Ein-/Ausgabe ─────────────────────────────────────────────────────────────

/// Explizite Herkunft eines Operationsaufrufs.
///
/// # Beschreibung
/// Statt die Aufruffläche indirekt aus zwei parallelen Feldern zu erraten
/// (`raw_args` vs. `json_args`), macht `OpInvocation` sie zu einer echten
/// Summe. Jede Operation weiß dadurch eindeutig:
/// - **wer** sie aufgerufen hat (Command-Zeile, Modell-Tool, Agent-Tool),
/// - **welches Argumentformat** gilt (rohe Tokens vs. JSON),
/// - **welche flächenspezifischen Aktionen** erlaubt sind (z. B. darf ein
///   read-only Modell-Tool destruktive Subcommands nicht erreichen — die Op
///   kann per `matches!(inv, OpInvocation::ModelTool { .. })` gaten).
///
/// Das Makro `#[operation]` dispatcht auf diese Variante: `Command` parst über
/// [`FromRawArgs`], die Tool-Flächen über `serde_json`.
#[derive(Clone, Debug)]
pub enum OpInvocation {
    /// Aufruf aus einer `/command`-Zeile.
    Command {
        /// Registrierter Command-Pfad, z. B. `"/model"`.
        path: String,
        /// Rohe Argumenttoken nach dem Command, z. B. `["list"]` oder `["--stat"]`.
        args: Vec<String>,
    },
    /// Aufruf als Modell-Tool. `args` ist das JSON-Argumentobjekt.
    ModelTool {
        /// Strukturierte JSON-Argumente aus dem Tool-Call (`Null` = keine).
        args: serde_json::Value,
    },
    /// Aufruf als Agent-Tool (Child-Spawn). `args` ist der JSON-Kontext.
    AgentTool {
        /// Ziel-Child-Rolle/-Name.
        child_name: String,
        /// Strukturierte JSON-Argumente/Kontext (`Null` = keine).
        args: serde_json::Value,
    },
}

impl OpInvocation {
    /// `true`, wenn der Aufruf von einer `/command`-Zeile stammt.
    #[must_use]
    pub fn is_command(&self) -> bool {
        matches!(self, Self::Command { .. })
    }

    /// `true`, wenn der Aufruf ein Modell-Tool-Call ist.
    #[must_use]
    pub fn is_model_tool(&self) -> bool {
        matches!(self, Self::ModelTool { .. })
    }

    /// `true`, wenn der Aufruf ein Agent-Tool-Spawn ist.
    #[must_use]
    pub fn is_agent_tool(&self) -> bool {
        matches!(self, Self::AgentTool { .. })
    }

    /// Rohe Command-Tokens; leer für Tool-Flächen.
    #[must_use]
    pub fn raw_args(&self) -> &[String] {
        match self {
            Self::Command { args, .. } => args,
            Self::ModelTool { .. } | Self::AgentTool { .. } => &[],
        }
    }

    /// JSON-Argumente der Tool-Flächen; `Null` für die Command-Fläche.
    #[must_use]
    pub fn json_args(&self) -> &serde_json::Value {
        const NULL: serde_json::Value = serde_json::Value::Null;
        match self {
            Self::ModelTool { args, .. } | Self::AgentTool { args, .. } => args,
            Self::Command { .. } => &NULL,
        }
    }
}

/// Fläche-neutrale Eingabe an eine Operation — dünne Hülle um [`OpInvocation`].
///
/// # Beschreibung
/// `OpInput` trägt die explizite Aufruf-Herkunft. Adapter konstruieren es über
/// die Konstruktoren [`OpInput::command`], [`OpInput::model_tool`] bzw.
/// [`OpInput::agent_tool`]; das `#[operation]`-Makro liest [`OpInput::invocation`].
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::OpInput;
///
/// let input = OpInput::command("/model", vec!["list".to_owned()]);
/// assert!(input.invocation.is_command());
/// assert_eq!(input.invocation.raw_args(), &["list".to_owned()]);
/// ```
#[derive(Clone, Debug)]
pub struct OpInput {
    /// Explizite Herkunft des Aufrufs.
    pub invocation: OpInvocation,
}

impl OpInput {
    /// Konstruiert eine Command-Eingabe aus Pfad und rohen Tokens.
    #[must_use]
    pub fn command(path: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            invocation: OpInvocation::Command {
                path: path.into(),
                args,
            },
        }
    }

    /// Konstruiert eine Modell-Tool-Eingabe aus JSON-Argumenten.
    #[must_use]
    pub fn model_tool(args: serde_json::Value) -> Self {
        Self {
            invocation: OpInvocation::ModelTool { args },
        }
    }

    /// Konstruiert eine Agent-Tool-Eingabe aus Child-Name und JSON-Kontext.
    #[must_use]
    pub fn agent_tool(child_name: impl Into<String>, args: serde_json::Value) -> Self {
        Self {
            invocation: OpInvocation::AgentTool {
                child_name: child_name.into(),
                args,
            },
        }
    }
}

impl Default for OpInput {
    /// Default ist eine leere Modell-Tool-Eingabe (`Null`), damit `raw_args()`
    /// leer und `json_args()` `Null` bleibt — verhaltensgleich zum früheren
    /// Zwei-Felder-Default.
    fn default() -> Self {
        Self::model_tool(serde_json::Value::Null)
    }
}

/// Parst rohe `/command`-Tokens in den typisierten Argument-Struct einer Operation.
///
/// # Beschreibung
/// Jeder Argument-Typ, der über das `#[operation]`-Makro auf der Command-Fläche
/// exponiert wird, implementiert diesen Trait. Das Makro ruft ihn für die
/// [`OpInvocation::Command`]-Variante auf; die Tool-Flächen nutzen weiter
/// `serde_json`. So besitzt jede Operation ein flächenspezifisches, explizites
/// Argument-Parsing statt einer fragilen `json_args.is_null()`-Heuristik.
pub trait FromRawArgs: Sized {
    /// Parst die rohen Tokens (ohne führenden Command-Namen).
    ///
    /// # Errors
    /// [`crate::error::OpError::InvalidArguments`], wenn die Tokens syntaktisch
    /// oder semantisch ungültig für diesen Argument-Typ sind.
    fn from_raw_args(tokens: &[String]) -> Result<Self, crate::error::OpError>;
}

/// Fläche-neutrales Ergebnis einer Operation.
///
/// # Beschreibung
/// `OpOutput` enthält einen menschenlesbaren Text, den Adapter flächenspezifisch
/// rendern (z. B. als TUI-Zeile, als Markdown in einem Channel, als `content`
/// in einem Tool-Call-Ergebnis), sowie eine optionale strukturierte Nutzlast
/// (F-222, `x-findings-register-w1-w3.md`): vor dieser Welle musste jede
/// strukturierte Web-Ansicht raten, ob `text` JSON enthält — `data` macht die
/// strukturierte Nutzlast explizit, ohne `text` als menschenlesbaren Bericht
/// zu ersetzen. [`OperationMeta::output_schema`] beschreibt optional die
/// Form von `data`.
///
/// # Migration
/// Bestehende handgeschriebene Operationen konstruieren `OpOutput` bislang als
/// `OpOutput { text }` (ohne `data`) — dieses Literal kompiliert mit dem neuen
/// Feld nicht mehr. Der `From<String>`-Impl unten liefert den mechanischen
/// Ersatz: `OpOutput::from(text)` bzw. `text.into()` liefert exakt dasselbe
/// Verhalten wie zuvor (`data: None`), ohne dass jede Konstruktionsstelle
/// einzeln um `data: None` ergänzt werden muss.
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::OpOutput;
///
/// let out = OpOutput { text: "3 Sessions aktiv.".to_owned(), data: None };
/// assert_eq!(out.text, "3 Sessions aktiv.");
///
/// // Migrationspfad für bestehende `OpOutput { text }`-Konstruktionsstellen:
/// let out2: OpOutput = "3 Sessions aktiv.".to_owned().into();
/// assert_eq!(out, out2);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpOutput {
    /// Menschenlesbarer Ausgabetext (ggf. mehrzeilig via `\n`).
    pub text: String,
    /// Optionale strukturierte Nutzlast (F-222). `None`, solange die
    /// Operation keine strukturierte Ansicht unterstützt oder das Ergebnis
    /// rein im menschenlesbaren `text` liegt.
    pub data: Option<serde_json::Value>,
}

impl From<String> for OpOutput {
    /// Baut ein `OpOutput` aus reinem Text, ohne strukturierte Nutzlast.
    ///
    /// # Beschreibung
    /// Migrationshilfe (F-222): ersetzt mechanisch jedes bisherige
    /// `OpOutput { text }`-Literal durch `OpOutput::from(text)` bzw.
    /// `text.into()`, ohne dass jede Konstruktionsstelle das neue `data`-Feld
    /// einzeln nennen muss.
    ///
    /// # Argumente
    /// - `text` (`String`): der menschenlesbare Ausgabetext.
    ///
    /// # Rückgabe
    /// `OpOutput { text, data: None }`.
    ///
    /// # Nebenläufigkeit
    /// Rein; aus beliebig vielen Threads aufrufbar.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_operations::operation::OpOutput;
    ///
    /// let out: OpOutput = "ok".to_owned().into();
    /// assert_eq!(out.text, "ok");
    /// assert!(out.data.is_none());
    /// ```
    fn from(text: String) -> Self {
        Self { text, data: None }
    }
}

// ── Future-Typ & Trait ────────────────────────────────────────────────────────

/// Gebox-tes, `Send`-fähiges Future einer Operationsausführung.
///
/// # Beschreibung
/// `OpFuture<'a>` ist der Rückgabetyp von [`Operation::run`]. Die Lifetime `'a`
/// bindet das Future an `&'a self`, sodass Implementierungen auf `self`-Feldern
/// ohne `Arc`-Wrapping leihen können.
pub type OpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<OpOutput, crate::error::OpError>> + Send + 'a>>;

/// Kern-Trait: eine ausführbare Operation, die über Adapter als Command oder
/// Modell-Tool exponiert werden kann.
///
/// # Beschreibung
/// Jede konkrete Operation implementiert diesen Trait und wird über ihre
/// [`OperationMeta`] in einer Registry registriert. Adapter (Command-Adapter,
/// ModelTool-Adapter) konsumieren ausschließlich diesen Trait — sie kennen
/// keine konkreten Typen.
///
/// # Nebenläufigkeit
/// `Operation: Send + Sync`, damit Implementierungen hinter `Arc<dyn Operation>`
/// über Thread-Grenzen geteilt werden können.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_operations::operation::{
///     Operation, OperationMeta, OperationDomain, OperationCategory, PermissionTier, OpInput, OpOutput, OpFuture,
/// };
/// use harw_operations::context::OpContext;
///
/// struct Noop;
///
/// impl Operation for Noop {
///     fn meta(&self) -> &OperationMeta {
///         static META: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
///         META.get_or_init(|| OperationMeta {
///             name: "noop",
///             summary: "Tut nichts.",
///             domain: OperationDomain::Misc,
///             permission: PermissionTier::Observer,
///             surfaces: vec![],
///             aliases: &[],
///             category: OperationCategory::Misc,
///             args_schema: None,
///             output_schema: None,
///             busy: Default::default(),
///         })
///     }
///
///     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
///         Box::pin(async { Ok(OpOutput { text: String::new(), data: None }) })
///     }
/// }
/// ```
pub trait Operation: Send + Sync {
    /// Gibt die statischen Metadaten dieser Operation zurück.
    ///
    /// # Rückgabe
    /// Eine Referenz auf [`OperationMeta`] mit Name, Zusammenfassung, Domäne,
    /// Berechtigung und allen deklarierten Flächen. Die Referenz muss für die
    /// Lebensdauer von `&self` gültig sein.
    fn meta(&self) -> &OperationMeta;

    /// Führt die Kernlogik der Operation aus.
    ///
    /// # Argumente
    /// - `ctx` (`&'a OpContext`): Unveränderlicher Ausführungs-Kontext (Session, Turn, Sandbox, Services).
    ///   Der Kontext wird vom Executor gesetzt und darf von der Operation nicht umgangen werden.
    /// - `input` (`OpInput`): Fläche-neutrale Eingabe (Rohtokens und/oder JSON-Argumente).
    ///
    /// # Rückgabe
    /// - `Ok(OpOutput)`: Erfolgreiche Ausführung mit menschenlesbarem Ergebnistext.
    /// - `Err(OpError)`: Fehler gemäß [`crate::error::OpError`].
    ///
    /// # Fehler
    /// - [`crate::error::OpError::InvalidArguments`]: Eingabe ist ungültig.
    /// - [`crate::error::OpError::Execution`]: Laufzeitfehler während der Ausführung.
    /// - [`crate::error::OpError::NotAvailable`]: Operation im aktuellen Kontext nicht verfügbar.
    ///
    /// # Nebenläufigkeit
    /// Das zurückgegebene Future ist `Send` und kann auf einem Tokio-Threadpool
    /// ausgeführt werden. Implementierungen dürfen `&self` und `&ctx` für die Dauer des
    /// Futures borgen; sie müssen aber sicherstellen, dass der Borrow `Send` ist.
    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a>;

    /// Unterbefehls-Überschreibungen der Busy-Klasse (siehe [`BusySubcommand`]).
    ///
    /// # Rückgabe
    /// Standard: leer — jeder Aufruf erbt [`OperationMeta::busy`]. Das
    /// `#[operation]`-Makro überschreibt die Methode, wenn
    /// `busy_subcommands = "..."` gesetzt ist.
    fn busy_subcommands(&self) -> &'static [BusySubcommand] {
        &[]
    }

    /// Busy-Klasse eines konkreten Aufrufs.
    ///
    /// # Argumente
    /// - `args`: Argument-Tokens der Befehlszeile (ohne Befehlsnamen).
    ///
    /// # Rückgabe
    /// [`BusySubcommand::resolve`] über [`OperationMeta::busy`] und
    /// [`Self::busy_subcommands`] mit dem ersten Token.
    fn busy_for(&self, args: &[String]) -> BusyAvailability {
        BusySubcommand::resolve(
            self.meta().busy,
            self.busy_subcommands(),
            args.first().map(String::as_str),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicy, ArgsSchemaProbe, BusyAvailability, CommandVisibility,
        DerivedArgsSchema as _, NoArgsSchema as _, OpInput, OpOutput, Operation, OperationCategory,
        OperationDomain, OperationMeta, PermissionTier, Surface, WebMethod,
    };
    use crate::context::{OpContext, ServiceMap};
    use crate::error::OpError;
    use crate::op_schema::{OpArgsSchema, object_schema, string_schema};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    // ── Shared test fixtures ──────────────────────────────────────────────────

    /// Minimal no-op operation that always succeeds.
    struct NoopOp;

    impl Operation for NoopOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "noop",
                summary: "Tut nichts.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> super::OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "ok".to_owned(),
                    data: None,
                })
            })
        }
    }

    /// Operation that always returns `OpError::InvalidArguments`.
    struct InvalidArgsOp;

    impl Operation for InvalidArgsOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "invalid-args",
                summary: "Schlägt immer mit InvalidArguments fehl.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> super::OpFuture<'a> {
            Box::pin(async {
                Err(OpError::InvalidArguments(
                    "kein Argument erlaubt".to_owned(),
                ))
            })
        }
    }

    /// Operation that always returns `OpError::Execution`.
    struct ExecutionErrOp;

    impl Operation for ExecutionErrOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "exec-err",
                summary: "Schlägt immer mit Execution fehl.",
                domain: OperationDomain::Execution,
                permission: PermissionTier::Operator,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> super::OpFuture<'a> {
            Box::pin(async { Err(OpError::Execution("interner Fehler".to_owned())) })
        }
    }

    /// Operation that always returns `OpError::NotAvailable`.
    struct NotAvailableOp;

    impl Operation for NotAvailableOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "not-available",
                summary: "Immer nicht verfügbar.",
                domain: OperationDomain::Agents,
                permission: PermissionTier::Maintainer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> super::OpFuture<'a> {
            Box::pin(async { Err(OpError::NotAvailable("Feature deaktiviert".to_owned())) })
        }
    }

    /// Operation with multiple surfaces (Command + ModelTool).
    struct MultiSurfaceOp;

    impl Operation for MultiSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "multi-surface",
                summary: "Mehrere Flächen.",
                domain: OperationDomain::Session,
                permission: PermissionTier::Owner,
                surfaces: vec![
                    Surface::Command {
                        path: "/session/list",
                        visibility: CommandVisibility::ChannelParity,
                    },
                    Surface::ModelTool {
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Session,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> super::OpFuture<'a> {
            Box::pin(async move {
                Ok(OpOutput {
                    text: format!("args={}", input.invocation.raw_args().len()),
                    data: None,
                })
            })
        }
    }

    fn make_empty_input() -> OpInput {
        OpInput::default()
    }

    fn make_raw_input(args: &[&str]) -> OpInput {
        OpInput::command("/x", args.iter().map(|s| (*s).to_owned()).collect())
    }

    fn make_json_input(val: serde_json::Value) -> OpInput {
        OpInput::model_tool(val)
    }

    /// Erstellt einen minimalen `OpContext` für Tests.
    /// Legt ein temporäres Verzeichnis an, baut eine WorkspaceRegistry und
    /// konstruiert einen SandboxSpec mit ReadWorkspace-Berechtigung.
    fn make_test_ctx() -> TestResult<(OpContext, PathBuf)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("harw-ops-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(tmp.join("ws"))
            .map_err(ctx("Test-Workspace-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
        Ok((ctx, tmp))
    }

    // ── OperationDomain ───────────────────────────────────────────────────────

    #[test]
    fn test_operation_domain_equality_same_variant() {
        assert_eq!(OperationDomain::Session, OperationDomain::Session);
        assert_eq!(OperationDomain::Agents, OperationDomain::Agents);
        assert_eq!(OperationDomain::Execution, OperationDomain::Execution);
        assert_eq!(
            OperationDomain::CatalogConfig,
            OperationDomain::CatalogConfig
        );
        assert_eq!(OperationDomain::Knowledge, OperationDomain::Knowledge);
        assert_eq!(OperationDomain::Misc, OperationDomain::Misc);
    }

    #[test]
    fn test_operation_domain_inequality_different_variants() {
        assert_ne!(OperationDomain::Session, OperationDomain::Misc);
        assert_ne!(OperationDomain::Agents, OperationDomain::Execution);
    }

    #[test]
    fn test_operation_domain_is_copy() {
        let d = OperationDomain::Knowledge;
        let d2 = d; // Copy semantics — no move
        assert_eq!(d, d2);
    }

    #[test]
    fn test_operation_domain_debug_is_non_empty() {
        assert!(!format!("{:?}", OperationDomain::Misc).is_empty());
    }

    #[test]
    fn test_operation_domain_serde_roundtrip_all_variants() -> TestResult {
        for domain in OperationDomain::ALL {
            let json = serde_json::to_string(&domain)
                .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?;
            assert_eq!(json, format!("\"{}\"", domain.as_str()));
            let back: OperationDomain = serde_json::from_str(&json)
                .map_err(ctx("Deserialisierung darf nicht fehlschlagen"))?;
            assert_eq!(back, domain);
        }
        Ok(())
    }

    #[test]
    fn test_operation_domain_infra_variants_serde_names() -> TestResult {
        for (domain, slug) in [
            (OperationDomain::Identity, "\"identity\""),
            (OperationDomain::Network, "\"network\""),
            (OperationDomain::Security, "\"security\""),
            (OperationDomain::Crypto, "\"crypto\""),
            (OperationDomain::CatalogConfig, "\"catalog_config\""),
        ] {
            let json = serde_json::to_string(&domain)
                .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?;
            assert_eq!(json, slug);
        }
        Ok(())
    }

    #[test]
    fn test_operation_domain_rejects_unknown_serde_name() {
        assert!(serde_json::from_str::<OperationDomain>("\"infra\"").is_err());
        assert!(serde_json::from_str::<OperationDomain>("\"Crypto\"").is_err());
    }

    #[test]
    fn test_operation_domain_display_and_from_slug_agree() {
        for domain in OperationDomain::ALL {
            assert_eq!(domain.to_string(), domain.as_str());
            assert_eq!(OperationDomain::from_slug(domain.as_str()), Some(domain));
        }
        assert_eq!(OperationDomain::from_slug("unknown"), None);
    }

    #[test]
    fn test_operation_domain_all_slugs_unique() {
        let mut slugs: Vec<&str> = OperationDomain::ALL
            .into_iter()
            .map(OperationDomain::as_str)
            .collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), OperationDomain::ALL.len());
    }

    #[test]
    fn test_operation_domain_default_category_mapping() {
        assert_eq!(
            OperationDomain::Session.default_category(),
            OperationCategory::Session
        );
        assert_eq!(
            OperationDomain::Agents.default_category(),
            OperationCategory::Agent
        );
        assert_eq!(
            OperationDomain::CatalogConfig.default_category(),
            OperationCategory::Model
        );
        assert_eq!(
            OperationDomain::Knowledge.default_category(),
            OperationCategory::Knowledge
        );
        assert_eq!(
            OperationDomain::Misc.default_category(),
            OperationCategory::Misc
        );
        for infra in [
            OperationDomain::Execution,
            OperationDomain::Identity,
            OperationDomain::Network,
            OperationDomain::Security,
            OperationDomain::Crypto,
        ] {
            assert_eq!(infra.default_category(), OperationCategory::System);
        }
    }

    // ── PermissionTier ordering ───────────────────────────────────────────────

    #[test]
    fn test_permission_tier_total_order_ascending() {
        assert!(PermissionTier::Observer < PermissionTier::Operator);
        assert!(PermissionTier::Operator < PermissionTier::Maintainer);
        assert!(PermissionTier::Maintainer < PermissionTier::Owner);
    }

    #[test]
    fn test_permission_tier_equality() {
        assert_eq!(PermissionTier::Observer, PermissionTier::Observer);
        assert_eq!(PermissionTier::Owner, PermissionTier::Owner);
    }

    #[test]
    fn test_permission_tier_observer_is_lowest() {
        for tier in [
            PermissionTier::Operator,
            PermissionTier::Maintainer,
            PermissionTier::Owner,
        ] {
            assert!(PermissionTier::Observer < tier);
        }
    }

    #[test]
    fn test_permission_tier_owner_is_highest() {
        for tier in [
            PermissionTier::Observer,
            PermissionTier::Operator,
            PermissionTier::Maintainer,
        ] {
            assert!(PermissionTier::Owner > tier);
        }
    }

    #[test]
    fn test_permission_tier_is_copy() {
        let t = PermissionTier::Operator;
        let t2 = t;
        assert_eq!(t, t2);
    }

    // ── CommandVisibility ─────────────────────────────────────────────────────

    #[test]
    fn test_command_visibility_equality() {
        assert_eq!(CommandVisibility::TuiOnly, CommandVisibility::TuiOnly);
        assert_eq!(
            CommandVisibility::ChannelParity,
            CommandVisibility::ChannelParity
        );
        assert_eq!(
            CommandVisibility::ChannelReduced,
            CommandVisibility::ChannelReduced
        );
    }

    #[test]
    fn test_command_visibility_inequality() {
        assert_ne!(CommandVisibility::TuiOnly, CommandVisibility::ChannelParity);
        assert_ne!(
            CommandVisibility::ChannelParity,
            CommandVisibility::ChannelReduced
        );
    }

    #[test]
    fn test_command_visibility_is_copy() {
        let v = CommandVisibility::TuiOnly;
        let v2 = v;
        assert_eq!(v, v2);
    }

    // ── ApprovalPolicy ────────────────────────────────────────────────────────

    #[test]
    fn test_approval_policy_equality() {
        assert_eq!(ApprovalPolicy::None, ApprovalPolicy::None);
        assert_eq!(ApprovalPolicy::Always, ApprovalPolicy::Always);
    }

    #[test]
    fn test_approval_policy_inequality() {
        assert_ne!(ApprovalPolicy::None, ApprovalPolicy::Always);
    }

    #[test]
    fn test_approval_policy_is_copy() {
        let p = ApprovalPolicy::Always;
        let p2 = p;
        assert_eq!(p, p2);
    }

    // ── BusyAvailability ──────────────────────────────────────────────────────

    #[test]
    fn test_busy_availability_default_is_deferred_until_turn_end() {
        assert_eq!(
            BusyAvailability::default(),
            BusyAvailability::DeferredUntilTurnEnd
        );
    }

    #[test]
    fn test_busy_availability_equality() {
        assert_eq!(BusyAvailability::Immediate, BusyAvailability::Immediate);
        assert_eq!(
            BusyAvailability::DeferredUntilTurnEnd,
            BusyAvailability::DeferredUntilTurnEnd
        );
    }

    #[test]
    fn test_busy_availability_inequality() {
        assert_ne!(
            BusyAvailability::Immediate,
            BusyAvailability::DeferredUntilTurnEnd
        );
    }

    #[test]
    fn test_busy_availability_is_copy() {
        let b = BusyAvailability::Immediate;
        let b2 = b;
        assert_eq!(b, b2);
    }

    #[test]
    fn test_busy_availability_runs_during_turn() {
        assert!(BusyAvailability::Immediate.runs_during_turn());
        assert!(BusyAvailability::Staged.runs_during_turn());
        assert!(!BusyAvailability::DeferredUntilTurnEnd.runs_during_turn());
    }

    #[test]
    fn test_busy_subcommand_resolve_prefers_table_and_bare_entry() {
        use super::BusySubcommand;
        const TABLE: &[BusySubcommand] = &[
            BusySubcommand::new(Some("show"), BusyAvailability::Immediate),
            BusySubcommand::new(Some("switch"), BusyAvailability::Staged),
            BusySubcommand::new(None, BusyAvailability::Immediate),
        ];
        let deferred = BusyAvailability::DeferredUntilTurnEnd;
        assert_eq!(
            BusySubcommand::resolve(deferred, TABLE, Some("show")),
            BusyAvailability::Immediate
        );
        assert_eq!(
            BusySubcommand::resolve(deferred, TABLE, Some("switch")),
            BusyAvailability::Staged
        );
        assert_eq!(
            BusySubcommand::resolve(deferred, TABLE, None),
            BusyAvailability::Immediate
        );
        assert_eq!(
            BusySubcommand::resolve(deferred, TABLE, Some("test")),
            deferred
        );
        assert_eq!(BusySubcommand::resolve(deferred, &[], None), deferred);
    }

    // ── Surface ───────────────────────────────────────────────────────────────

    #[test]
    fn test_surface_command_equality() {
        let a = Surface::Command {
            path: "/session/list",
            visibility: CommandVisibility::TuiOnly,
        };
        let b = Surface::Command {
            path: "/session/list",
            visibility: CommandVisibility::TuiOnly,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_surface_model_tool_equality() {
        let a = Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_surface_command_vs_model_tool_not_equal() {
        let cmd = Surface::Command {
            path: "/x",
            visibility: CommandVisibility::TuiOnly,
        };
        let tool = Surface::ModelTool {
            readonly: false,
            approval: ApprovalPolicy::Always,
        };
        assert_ne!(cmd, tool);
    }

    #[test]
    fn test_surface_command_different_paths_not_equal() {
        let a = Surface::Command {
            path: "/a",
            visibility: CommandVisibility::TuiOnly,
        };
        let b = Surface::Command {
            path: "/b",
            visibility: CommandVisibility::TuiOnly,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_command_different_visibility_not_equal() {
        let a = Surface::Command {
            path: "/x",
            visibility: CommandVisibility::TuiOnly,
        };
        let b = Surface::Command {
            path: "/x",
            visibility: CommandVisibility::ChannelParity,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_model_tool_readonly_flag_differs() {
        let a = Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::ModelTool {
            readonly: false,
            approval: ApprovalPolicy::None,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_clone_produces_equal_value() {
        let s = Surface::Command {
            path: "/clone-test",
            visibility: CommandVisibility::ChannelReduced,
        };
        assert_eq!(s.clone(), s);
    }

    // ── Surface::Web ──────────────────────────────────────────────────────────

    #[test]
    fn test_surface_web_equality() {
        let a = Surface::Web {
            path: "/api/session/list",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::Web {
            path: "/api/session/list",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_surface_web_different_paths_not_equal() {
        let a = Surface::Web {
            path: "/api/a",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::Web {
            path: "/api/b",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_web_method_differs() {
        let a = Surface::Web {
            path: "/api/x",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::Web {
            path: "/api/x",
            method: WebMethod::Post,
            approval: ApprovalPolicy::None,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_web_approval_differs() {
        let a = Surface::Web {
            path: "/api/x",
            method: WebMethod::Post,
            approval: ApprovalPolicy::None,
        };
        let b = Surface::Web {
            path: "/api/x",
            method: WebMethod::Post,
            approval: ApprovalPolicy::Always,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_surface_web_vs_other_surfaces_not_equal() {
        let web = Surface::Web {
            path: "/api/x",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        };
        let cmd = Surface::Command {
            path: "/api/x",
            visibility: CommandVisibility::TuiOnly,
        };
        let tool = Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None,
        };
        let agent = Surface::AgentTool {
            child_name: "child",
            authority_reducer: "reduce",
            budget_hint: "8k",
        };
        assert_ne!(web, cmd);
        assert_ne!(web, tool);
        assert_ne!(web, agent);
    }

    #[test]
    fn test_surface_web_clone_produces_equal_value() {
        let s = Surface::Web {
            path: "/api/clone-test",
            method: WebMethod::Post,
            approval: ApprovalPolicy::RequireForEffect,
        };
        assert_eq!(s.clone(), s);
    }

    // ── WebMethod ─────────────────────────────────────────────────────────────

    #[test]
    fn test_web_method_equality() {
        assert_eq!(WebMethod::Get, WebMethod::Get);
        assert_eq!(WebMethod::Post, WebMethod::Post);
    }

    #[test]
    fn test_web_method_inequality() {
        assert_ne!(WebMethod::Get, WebMethod::Post);
    }

    #[test]
    fn test_web_method_is_copy() {
        let m = WebMethod::Get;
        let m2 = m;
        assert_eq!(m, m2);
    }

    #[test]
    fn test_web_method_serde_roundtrip_get() -> TestResult {
        let json = serde_json::to_string(&WebMethod::Get)
            .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?;
        let back: WebMethod =
            serde_json::from_str(&json).map_err(ctx("Deserialisierung darf nicht fehlschlagen"))?;
        assert_eq!(back, WebMethod::Get);
        Ok(())
    }

    #[test]
    fn test_web_method_serde_roundtrip_post() -> TestResult {
        let json = serde_json::to_string(&WebMethod::Post)
            .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?;
        let back: WebMethod =
            serde_json::from_str(&json).map_err(ctx("Deserialisierung darf nicht fehlschlagen"))?;
        assert_eq!(back, WebMethod::Post);
        Ok(())
    }

    #[test]
    fn test_web_method_serde_uses_screaming_snake_case() -> TestResult {
        assert_eq!(
            serde_json::to_string(&WebMethod::Get)
                .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?,
            "\"GET\""
        );
        assert_eq!(
            serde_json::to_string(&WebMethod::Post)
                .map_err(ctx("Serialisierung darf nicht fehlschlagen"))?,
            "\"POST\""
        );
        Ok(())
    }

    // ── OperationMeta ─────────────────────────────────────────────────────────

    #[test]
    fn test_operation_meta_fields_accessible() {
        let meta = OperationMeta {
            name: "test.op",
            summary: "Eine Testoperation.",
            domain: OperationDomain::CatalogConfig,
            permission: PermissionTier::Maintainer,
            surfaces: vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        };
        assert_eq!(meta.name, "test.op");
        assert_eq!(meta.summary, "Eine Testoperation.");
        assert_eq!(meta.domain, OperationDomain::CatalogConfig);
        assert_eq!(meta.permission, PermissionTier::Maintainer);
        assert_eq!(meta.surfaces.len(), 1);
    }

    #[test]
    fn test_operation_meta_empty_surfaces() {
        let meta = OperationMeta {
            name: "bare",
            summary: "Keine Flächen.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: vec![],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        };
        assert!(meta.surfaces.is_empty());
    }

    #[test]
    fn test_operation_meta_clone() {
        let meta = OperationMeta {
            name: "clone.me",
            summary: "Klon-Test.",
            domain: OperationDomain::Knowledge,
            permission: PermissionTier::Operator,
            surfaces: vec![],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        };
        let cloned = meta.clone();
        assert_eq!(cloned.name, meta.name);
        assert_eq!(cloned.domain, meta.domain);
        assert_eq!(cloned.permission, meta.permission);
    }

    // ── OpInput ───────────────────────────────────────────────────────────────

    #[test]
    fn test_op_input_default_is_empty() {
        let input = OpInput::default();
        assert!(input.invocation.raw_args().is_empty());
        assert_eq!(input.invocation.json_args(), &serde_json::Value::Null);
        assert!(input.invocation.is_model_tool());
    }

    #[test]
    fn test_op_input_with_raw_args() {
        let input = make_raw_input(&["--limit", "5"]);
        assert_eq!(input.invocation.raw_args().len(), 2);
        assert_eq!(input.invocation.raw_args()[0], "--limit");
        assert_eq!(input.invocation.raw_args()[1], "5");
        assert_eq!(input.invocation.json_args(), &serde_json::Value::Null);
    }

    #[test]
    fn test_op_input_with_json_args() {
        let json = serde_json::json!({ "limit": 10 });
        let input = make_json_input(json.clone());
        assert!(input.invocation.raw_args().is_empty());
        assert_eq!(input.invocation.json_args(), &json);
    }

    #[test]
    fn test_op_input_clone() {
        let input = make_raw_input(&["a", "b"]);
        let cloned = input.clone();
        assert_eq!(cloned.invocation.raw_args(), input.invocation.raw_args());
    }

    #[test]
    fn test_op_input_empty_raw_args_slice() {
        let input = make_raw_input(&[]);
        assert!(input.invocation.raw_args().is_empty());
    }

    // ── OpInvocation — new API coverage ──────────────────────────────────────

    #[test]
    fn test_op_input_command_sets_is_command_and_raw_args() {
        let input = OpInput::command("/model", vec!["list".to_owned()]);
        assert!(input.invocation.is_command());
        assert!(!input.invocation.is_model_tool());
        assert!(!input.invocation.is_agent_tool());
        assert_eq!(input.invocation.raw_args(), &["list".to_owned()]);
        assert_eq!(input.invocation.json_args(), &serde_json::Value::Null);
    }

    #[test]
    fn test_op_input_model_tool_sets_is_model_tool_and_json_args() {
        let json = serde_json::json!({ "cmd": "list" });
        let input = OpInput::model_tool(json.clone());
        assert!(input.invocation.is_model_tool());
        assert!(!input.invocation.is_command());
        assert!(!input.invocation.is_agent_tool());
        assert!(input.invocation.raw_args().is_empty());
        assert_eq!(input.invocation.json_args(), &json);
    }

    #[test]
    fn test_op_input_agent_tool_sets_is_agent_tool_and_json_args() {
        let json = serde_json::json!({ "role": "reviewer" });
        let input = OpInput::agent_tool("child-a", json.clone());
        assert!(input.invocation.is_agent_tool());
        assert!(!input.invocation.is_command());
        assert!(!input.invocation.is_model_tool());
        assert!(input.invocation.raw_args().is_empty());
        assert_eq!(input.invocation.json_args(), &json);
    }

    #[test]
    fn test_op_invocation_command_path_and_args_accessible() -> TestResult {
        let input = OpInput::command("/session/list", vec!["--limit".to_owned(), "5".to_owned()]);
        match input.invocation {
            crate::operation::OpInvocation::Command { path, args } => {
                assert_eq!(path, "/session/list");
                assert_eq!(args, vec!["--limit".to_owned(), "5".to_owned()]);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet Command, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── OpOutput ──────────────────────────────────────────────────────────────

    #[test]
    fn test_op_output_text_field() {
        let out = OpOutput {
            text: "3 Sessions aktiv.".to_owned(),
            data: None,
        };
        assert_eq!(out.text, "3 Sessions aktiv.");
    }

    #[test]
    fn test_op_output_empty_text() {
        let out = OpOutput {
            text: String::new(),
            data: None,
        };
        assert!(out.text.is_empty());
    }

    #[test]
    fn test_op_output_equality() {
        let a = OpOutput {
            text: "gleich".to_owned(),
            data: None,
        };
        let b = OpOutput {
            text: "gleich".to_owned(),
            data: None,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn test_op_output_inequality() {
        let a = OpOutput {
            text: "alpha".to_owned(),
            data: None,
        };
        let b = OpOutput {
            text: "beta".to_owned(),
            data: None,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_op_output_clone() {
        let out = OpOutput {
            text: "clone me".to_owned(),
            data: None,
        };
        assert_eq!(out.clone(), out);
    }

    #[test]
    fn test_op_output_data_field_accessible() {
        let payload = serde_json::json!({ "count": 3 });
        let out = OpOutput {
            text: "3 Sessions aktiv.".to_owned(),
            data: Some(payload.clone()),
        };
        assert_eq!(out.data, Some(payload));
    }

    #[test]
    fn test_op_output_equality_considers_data_field() {
        let a = OpOutput {
            text: "gleich".to_owned(),
            data: None,
        };
        let b = OpOutput {
            text: "gleich".to_owned(),
            data: Some(serde_json::json!({ "x": 1 })),
        };
        assert_ne!(
            a, b,
            "zwei OpOutput mit gleichem text aber unterschiedlichem data dürfen nicht gleich sein"
        );
    }

    #[test]
    fn test_op_output_from_string_sets_text_and_no_data() {
        let out: OpOutput = "ok".to_owned().into();
        assert_eq!(out.text, "ok");
        assert!(out.data.is_none());
    }

    #[test]
    fn test_op_output_from_string_matches_manual_construction() {
        let via_from = OpOutput::from("3 Sessions aktiv.".to_owned());
        let manual = OpOutput {
            text: "3 Sessions aktiv.".to_owned(),
            data: None,
        };
        assert_eq!(via_from, manual);
    }

    // ── Operation::meta ───────────────────────────────────────────────────────

    #[test]
    fn test_operation_meta_name_via_trait() {
        let op = NoopOp;
        assert_eq!(op.meta().name, "noop");
    }

    #[test]
    fn test_operation_meta_domain_via_trait() {
        let op = NoopOp;
        assert_eq!(op.meta().domain, OperationDomain::Misc);
    }

    #[test]
    fn test_operation_meta_permission_via_trait() {
        let op = NoopOp;
        assert_eq!(op.meta().permission, PermissionTier::Observer);
    }

    #[test]
    fn test_operation_meta_surfaces_empty_via_trait() {
        let op = NoopOp;
        assert!(op.meta().surfaces.is_empty());
    }

    #[test]
    fn test_operation_meta_surfaces_multi_surface_op() {
        let op = MultiSurfaceOp;
        assert_eq!(op.meta().surfaces.len(), 2);
    }

    #[test]
    fn test_operation_meta_stable_reference() {
        let op = NoopOp;
        // Both calls must return the same pointer (OnceLock guarantees this).
        let m1 = op.meta() as *const OperationMeta;
        let m2 = op.meta() as *const OperationMeta;
        assert_eq!(
            m1, m2,
            "meta() should return the same static reference each time"
        );
    }

    // ── Operation::run — happy path ───────────────────────────────────────────

    #[tokio::test]
    async fn test_noop_op_run_returns_ok() -> TestResult {
        let op = NoopOp;
        let (ctx, tmp) = make_test_ctx()?;
        let result = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        assert!(result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn test_noop_op_run_output_text_non_empty() -> TestResult {
        let op = NoopOp;
        let (ctx, tmp) = make_test_ctx()?;
        let out = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        match out {
            Ok(o) => assert!(!o.text.is_empty()),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_multi_surface_op_run_reflects_arg_count() -> TestResult {
        let op = MultiSurfaceOp;
        let (ctx, tmp) = make_test_ctx()?;
        let input = make_raw_input(&["a", "b", "c"]);
        let out = op.run(&ctx, input).await;
        std::fs::remove_dir_all(tmp).ok();
        match out {
            Ok(o) => assert_eq!(o.text, "args=3"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_multi_surface_op_run_with_empty_args() -> TestResult {
        let op = MultiSurfaceOp;
        let (ctx, tmp) = make_test_ctx()?;
        let out = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        match out {
            Ok(o) => assert_eq!(o.text, "args=0"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    // ── Operation::run — error paths ──────────────────────────────────────────

    #[tokio::test]
    async fn test_invalid_args_op_run_returns_invalid_arguments_err() -> TestResult {
        let op = InvalidArgsOp;
        let (ctx, tmp) = make_test_ctx()?;
        let result = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet InvalidArguments, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_execution_err_op_run_returns_execution_err() -> TestResult {
        let op = ExecutionErrOp;
        let (ctx, tmp) = make_test_ctx()?;
        let result = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::Execution(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet Execution, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_not_available_op_run_returns_not_available_err() -> TestResult {
        let op = NotAvailableOp;
        let (ctx, tmp) = make_test_ctx()?;
        let result = op.run(&ctx, make_empty_input()).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::NotAvailable(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet NotAvailable, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Send + Sync (compile-time checks via trait bounds) ────────────────────

    #[test]
    fn test_operation_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopOp>();
        assert_send_sync::<MultiSurfaceOp>();
    }

    // ── Arc<dyn Operation> across threads ─────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_arc_dyn_operation_shared_across_tasks() -> TestResult {
        use std::sync::Arc;
        let (op_ctx, tmp) = make_test_ctx()?;
        let op_ctx = Arc::new(op_ctx);
        let op: Arc<dyn Operation> = Arc::new(NoopOp);
        let op2 = Arc::clone(&op);
        let ctx1 = Arc::clone(&op_ctx);
        let ctx2 = Arc::clone(&op_ctx);
        let h1 = tokio::spawn(async move { op.run(&ctx1, make_empty_input()).await });
        let h2 = tokio::spawn(async move { op2.run(&ctx2, make_empty_input()).await });
        let r1 = h1.await;
        let r2 = h2.await;
        std::fs::remove_dir_all(tmp).ok();
        let Ok(inner1) = r1 else {
            return Err(TestError::Unexpected("task 1 panicked".into()));
        };
        let Ok(inner2) = r2 else {
            return Err(TestError::Unexpected("task 2 panicked".into()));
        };
        assert!(inner1.is_ok(), "operation 1 returned Err");
        assert!(inner2.is_ok(), "operation 2 returned Err");
        Ok(())
    }

    // ── OperationCategory ─────────────────────────────────────────────────────

    #[test]
    fn test_operation_category_as_str_round_trip() {
        for c in [
            OperationCategory::Model,
            OperationCategory::Agent,
            OperationCategory::Session,
            OperationCategory::System,
            OperationCategory::Knowledge,
            OperationCategory::Misc,
        ] {
            assert!(!c.as_str().is_empty());
        }
    }

    #[test]
    fn test_operation_category_default_is_misc() {
        assert_eq!(OperationCategory::default(), OperationCategory::Misc);
    }

    // ── args_schema & ArgsSchemaProbe ─────────────────────────────────────────

    /// Argument-Typ **mit** `OpArgsSchema` — steht für `#[derive(OpArgs)]`.
    struct WithSchemaArgs;

    impl OpArgsSchema for WithSchemaArgs {
        fn json_schema() -> harw_tools::JsonSchema {
            object_schema(
                vec![("job_id", string_schema("Kennung des Jobs."))],
                &["job_id"],
            )
        }
    }

    /// Argument-Typ **ohne** `OpArgsSchema` — steht für eine Operation, die das
    /// Derive nicht trägt.
    struct WithoutSchemaArgs;

    /// Spiegelt exakt die Auflösung, die `#[operation]` erzeugt: eine
    /// Autoref-Stufe auf die Sonde, beide Zweig-Traits im Geltungsbereich.
    macro_rules! probe_args_schema {
        ($ty:ty) => {{
            let probe = ArgsSchemaProbe::<$ty>::new();
            let probe_ref: &ArgsSchemaProbe<$ty> = &probe;
            probe_ref.harw_args_schema()
        }};
    }

    #[test]
    fn test_probe_returns_schema_for_type_implementing_op_args_schema() {
        assert!(
            probe_args_schema!(WithSchemaArgs).is_some(),
            "ein Argument-Typ mit OpArgsSchema muss seinen Schema-Zeiger liefern"
        );
    }

    #[test]
    fn test_probe_returns_none_for_type_without_op_args_schema() {
        assert!(
            probe_args_schema!(WithoutSchemaArgs).is_none(),
            "ohne OpArgsSchema darf die Sonde None liefern statt zu brechen"
        );
    }

    #[test]
    fn test_probe_schema_pointer_builds_closed_schema() -> TestResult {
        let Some(build) = probe_args_schema!(WithSchemaArgs) else {
            return Err(TestError::Missing("Some für WithSchemaArgs"));
        };
        let schema = build();

        assert_eq!(
            schema.additional_properties,
            Some(Box::new(harw_tools::AdditionalProperties::Bool(false))),
            "das gelieferte Schema muss geschlossen sein"
        );
        assert_eq!(schema.required, Some(vec!["job_id".to_owned()]));
        Ok(())
    }

    #[test]
    fn test_probe_schema_pointer_equals_trait_function() -> TestResult {
        let Some(build) = probe_args_schema!(WithSchemaArgs) else {
            return Err(TestError::Missing("Some für WithSchemaArgs"));
        };

        assert_eq!(
            build(),
            <WithSchemaArgs as OpArgsSchema>::json_schema(),
            "die Sonde muss genau die Trait-Funktion binden"
        );
        Ok(())
    }

    #[test]
    fn test_operation_meta_args_schema_defaults_to_none_in_fixtures() {
        let op = NoopOp;
        assert!(
            op.meta().args_schema.is_none(),
            "handgeschriebene Metadaten ohne Schema bleiben None"
        );
    }

    #[test]
    fn test_operation_meta_default_has_no_args_schema() {
        let meta = OperationMeta::default();

        assert!(meta.args_schema.is_none());
        assert!(meta.output_schema.is_none());
        assert_eq!(meta.permission, PermissionTier::Observer);
        assert_eq!(meta.domain, OperationDomain::Misc);
        assert_eq!(meta.category, OperationCategory::Misc);
        assert!(meta.surfaces.is_empty());
        assert!(meta.aliases.is_empty());
        assert_eq!(meta.busy, BusyAvailability::DeferredUntilTurnEnd);
    }

    #[test]
    fn test_operation_meta_default_fills_only_unnamed_fields() {
        let meta = OperationMeta {
            name: "session.list",
            summary: "Listet Sessions auf.",
            ..OperationMeta::default()
        };

        assert_eq!(meta.name, "session.list");
        assert!(meta.args_schema.is_none());
        assert!(meta.output_schema.is_none());
    }

    #[test]
    fn test_operation_meta_with_args_schema_is_cloneable() -> TestResult {
        let meta = OperationMeta {
            name: "with-schema",
            summary: "Trägt ein Argument-Schema.",
            args_schema: Some(<WithSchemaArgs as OpArgsSchema>::json_schema),
            ..OperationMeta::default()
        };
        let cloned = meta.clone();

        let Some(build) = cloned.args_schema else {
            return Err(TestError::Missing("der Zeiger muss den Clone überleben"));
        };
        assert_eq!(build().required, Some(vec!["job_id".to_owned()]));
        Ok(())
    }

    #[test]
    fn test_operation_meta_with_output_schema_is_cloneable() -> TestResult {
        let meta = OperationMeta {
            name: "with-output-schema",
            summary: "Trägt ein Ausgabe-Schema.",
            output_schema: Some(<WithSchemaArgs as OpArgsSchema>::json_schema),
            ..OperationMeta::default()
        };
        let cloned = meta.clone();

        let Some(build) = cloned.output_schema else {
            return Err(TestError::Missing("der Zeiger muss den Clone überleben"));
        };
        assert_eq!(build().required, Some(vec!["job_id".to_owned()]));
        Ok(())
    }
}
