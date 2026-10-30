//! Fehlertyp für `harw-dod-workspace`.
//!
//! # Verantwortungsbereich
//! [`WorkspaceError`] ist der eine Fehlertyp dieser Crate. `Display`, `Debug`,
//! `std::error::Error` und die `From`-Konvertierung für `#[from]`-Varianten
//! entstehen über `#[derive(harw_macros::HarwError)]` (Muster:
//! `harw-dod-cap/src/error.rs`). Kein `anyhow`, kein `thiserror`.
//!
//! # Der K79-Vorfall: ein Werkzeugfehler, gemeldet als Baumbefund
//! Seit AW0-00 tragen alle 95 Member-Manifeste dieses Workspace
//! `version.workspace = true` statt einer wörtlichen Versions-Zeichenkette.
//! `harw_code_graph::RawPackageSection::version` war zu diesem Zeitpunkt als
//! `Option<String>` deklariert; `serde` konnte die Tabellenform
//! `{ workspace = true }` nicht in eine Zeichenkette einlesen und scheiterte
//! deshalb an **jedem** der 95 Manifeste. `Inventory::read` bildete diesen
//! Fehler — wie jeden anderen Ladefehler auch — über
//! `.map_err(|_| WorkspaceError::MalformedSource)` ab. Für einen Empfänger
//! dieser Meldung sah das aus wie: „der überwachte Baum ist fehlerhaft".
//! Tatsächlich war der Baum in Ordnung; **unser eigener Parser** verstand ihn
//! nicht mehr, seit sich eine einzige, überall gleich vorgenommene
//! Konvention geändert hatte (Korrektur K79 hat die Schema-Lücke in
//! `harw_code_graph` seither geschlossen). Ein Sicherheitssensor, der jeden
//! Ladefehler auf dieselbe Variante abbildet, kann strukturell nicht
//! zwischen „der Baum ist kompromittiert" und „unser Werkzeug versteht ihn
//! nicht" unterscheiden — und nur eines davon ist ein Sicherheitsereignis.
//! Diese Datei fächert die Abbildung deshalb variantenweise auf; siehe
//! [`crate::inventory::Inventory::read`] für die konkrete Zuordnung jeder
//! `harw_code_graph::CodeGraphError`-Variante.
//!
//! # Inhaltsfrei nach demselben Vorbild wie `harw_dod_cap::SensorError`
//! Sowohl [`WorkspaceError::MalformedSource`] als auch
//! [`WorkspaceError::ToolFault`] tragen **absichtlich keinen** inneren
//! Fehlerwert. `harw_code_graph::CodeGraphError::Toml` — und darunter
//! `toml::de::Error` selbst — betten einen Ausschnitt der fehlerhaften Zeile
//! in ihre `Display`-Ausgabe ein. Würde eine dieser Varianten `#[from]` auf
//! `CodeGraphError` tragen und ihre `#[msg]` `{0}` referenzieren (nötig, um
//! die von `harw-macros` erzeugte `unused_variables`-Falle zu vermeiden),
//! liefe genau dieser Dateiinhalt in die Meldung durch. Die einzige sichere
//! Option ist deshalb je eine feldlose Variante, die den Ursprungsfehler an
//! der Aufrufstelle verwirft.
//!
//! # Die Fanout-Entscheidung: Baum oder Werkzeug?
//! [`crate::inventory::Inventory::read`] ordnet jede
//! `harw_code_graph::CodeGraphError`-Variante einzeln einer der beiden
//! Kategorien zu:
//!
//! - **[`WorkspaceError::MalformedSource`]** (Baumbefund): `Toml`
//!   (unlesbares TOML — dieselbe Variante, mit der auch echt kaputtes TOML
//!   im überwachten Baum gemeldet wird, siehe Test
//!   `test_read_malformed_cargo_toml_returns_malformed_source_without_file_content`),
//!   `ManifestMissing` (ein Manifest fehlt physisch an einem erwarteten
//!   Pfad — eine reine Existenzprüfung, unabhängig davon, wie vollständig
//!   unser Schema ist), `NoWorkspaceSection` (das Wurzel-Manifest hat
//!   schlicht keinen `[workspace]`-Abschnitt — ebenfalls eine reine
//!   Vorhandensein-Prüfung), `MemberMissing` (eine bereits als
//!   workspace-intern klassifizierte Abhängigkeitskante zeigt auf keinen
//!   real existierenden Knoten — eine Inkonsistenz der tatsächlichen
//!   Graphstruktur) und `CycleDetected` (ein echter, unaufgelöster
//!   Abhängigkeitszyklus zwischen Membern — Cargo selbst verbietet das, ein
//!   Fund ist also ein struktureller Fakt über den Baum).
//! - **[`WorkspaceError::ToolFault`]** (Werkzeugfehler): `InvalidPath` (auf
//!   dem von `Inventory::read` genutzten Pfad entsteht diese Variante
//!   ausschließlich, wenn `[workspace].members` ein syntaktisch gültiges,
//!   aber von unserem Auflöser nicht unterstütztes Glob-Muster enthält —
//!   laut `harw_code_graph::resolve_member_dirs`-Doku wird nur `<prefix>/*`
//!   unterstützt; jedes andere Muster scheitert, obwohl Cargo es akzeptiert
//!   — strukturell derselbe Fehler wie K79: eine Fähigkeitslücke unseres
//!   Werkzeugs, keine Eigenschaft des Baums), `CargoHomeUnavailable` (fehlt
//!   in der Umgebung dieses Prozesses, nicht im Baum) und
//!   `RegistrySourceNotFound` (betrifft den lokalen Registry-Cache dieses
//!   Hosts, nicht den überwachten Baum). `Io`-Fehler behalten die
//!   bestehende [`Self::Io`]-Variante bei.
//!
//! # Verhältnis zu `harw_dod_cap::SensorError`
//! [`WorkspaceError`] ist der reichhaltigere, crate-eigene Fehlertyp von
//! [`crate::inventory::Inventory::read`]. `harw_dod_signals::Sensor::poll`
//! schreibt jedoch wörtlich `harw_dod_cap::SensorError` als Fehlertyp vor
//! (Vertrag Abschnitt G) — deshalb bildet [`From<WorkspaceError> for
//! harw_dod_cap::SensorError`] auf die fünf vorhandenen `SensorError`-
//! Varianten ab, sodass [`crate::sensor::WorkspaceDriftSensor::poll`] `?`
//! verwenden kann.
//!
//! **Lücke geschlossen:** `harw_dod_cap::SensorError` kennt inzwischen
//! [`harw_dod_cap::SensorError::ToolFault`] mit `Permanence::Permanent`
//! (`harw-dod-cap/src/error.rs`, `fn permanence`) — genau für den Fall
//! „unser Werkzeug kann die Quelle dauerhaft nicht verarbeiten", ohne dass
//! die Quelle selbst betroffen ist. [`Self::ToolFault`] bildet deshalb nicht
//! mehr — mangels Alternative — auf [`Self::MalformedSource`] ab, das dort
//! als `Permanence::Transient` eingestuft ist. Vorher degradierte der
//! Sensor erst, nachdem der Sentinel seine Retry-Policy für
//! `Transient`-Fehler ausgeschöpft hatte; jetzt meldet er sich beim
//! nächsten Poll sofort als dauerhaft degradiert, weil ein unterstütztes
//! Glob-Muster nicht spontan wiederkehrt und `CARGO_HOME` nicht zufällig
//! für einen Zyklus verschwindet.
//!
//! # Exportierte Typen
//! [`WorkspaceError`], [`WorkspaceResult`] (von `harw-macros` erzeugter
//! Typalias).
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_workspace::error::WorkspaceError;
//!
//! let err = WorkspaceError::MalformedSource;
//! assert_eq!(
//!     err.to_string(),
//!     "workspace manifest or lockfile could not be parsed"
//! );
//!
//! let tool_err = WorkspaceError::ToolFault;
//! assert_eq!(
//!     tool_err.to_string(),
//!     "workspace tooling could not process the source"
//! );
//! ```

use harw_macros::HarwError;

/// Fehler beim Lesen oder Vergleichen des Workspace-Inventars.
///
/// # Description
/// Deckt alle Fehlerfälle von [`crate::inventory::Inventory::read`] ab:
/// Bereichsverletzungen, I/O-Fehler beim Kanonisieren der Workspace-Wurzel,
/// unlesbare bzw. strukturell unstimmige Manifeste/Lockfiles im überwachten
/// Baum ([`Self::MalformedSource`]) und Fähigkeitslücken unseres eigenen
/// Ladewerkzeugs ([`Self::ToolFault`]). Siehe Moduldoku für die vollständige
/// Zuordnungstabelle und die Begründung, warum beide Varianten feldlos
/// bleiben.
#[derive(Debug, HarwError)]
pub enum WorkspaceError {
    /// I/O-Fehler beim Kanonisieren der Workspace-Wurzel
    /// (`Path::canonicalize`) oder beim Lesen eines Manifests bzw. Lockfiles
    /// durch `harw_code_graph`.
    ///
    /// # Aussage
    /// Sagt zunächst nichts über den Inhalt des Baums aus — nur, dass ein
    /// Betriebssystem-Lesevorgang gescheitert ist (z. B. Berechtigungen,
    /// verschwundene Datei zwischen Auflisten und Lesen). Der Empfänger darf
    /// daraus **keinen** Strukturbefund über den Baum ableiten, sondern nur:
    /// dieser Lesevorgang ist gescheitert, ein erneuter Versuch kann sinnvoll
    /// sein.
    ///
    /// # Arguments
    /// - `source` (`std::io::Error`): der zugrunde liegende
    ///   Betriebssystem-Fehler. Über `std::error::Error::source()` verlinkt.
    #[from]
    Io(std::io::Error),

    /// Die Workspace-Wurzel oder ein gelesenes Member-Manifest liegt
    /// außerhalb des konfigurierten [`harw_dod_cap::ReadScope`].
    ///
    /// # Aussage
    /// Ein Befund über den Baum: ein `[workspace].members`-Eintrag verweist
    /// (nach Kanonisierung) auf einen Pfad außerhalb des erlaubten
    /// Lesebereichs. Nennt **nie** den beanstandeten Pfad — dieselbe
    /// Zusicherung wie [`harw_dod_cap::error::SensorError::OutsideScope`].
    #[msg("path resolves outside the sensor read scope")]
    OutsideScope,

    /// Ein Manifest oder Lockfile **im überwachten Baum** ist syntaktisch
    /// oder strukturell unstimmig: unlesbares TOML, ein fehlendes Manifest,
    /// ein fehlender `[workspace]`-Abschnitt, eine Abhängigkeitskante auf
    /// einen nicht existierenden Knoten, oder ein echter
    /// Abhängigkeitszyklus.
    ///
    /// # Aussage
    /// Ein Befund über den Baum: jede dieser Ursachen ist ein struktureller
    /// oder Existenz-Fakt über die aktuell im Baum liegenden Dateien, der
    /// unabhängig von der Vollständigkeit unseres eigenen Parser-Schemas
    /// gilt (siehe Moduldoku für die vollständige Zuordnung). Der Empfänger
    /// darf daraus ableiten: der überwachte Baum weicht von einem gültigen
    /// Cargo-Workspace ab.
    #[msg("workspace manifest or lockfile could not be parsed")]
    MalformedSource,

    /// Unser eigenes Ladewerkzeug (`harw_code_graph::WorkspaceGraph::load`)
    /// konnte den Baum nicht verarbeiten, obwohl der Baum selbst kein
    /// unmittelbar erkennbarer Sicherheitsbefund ist: ein syntaktisch
    /// gültiges, aber von unserem Auflöser nicht unterstütztes
    /// `[workspace].members`-Glob-Muster, eine fehlende
    /// `CARGO_HOME`/`HOME`-Umgebungsvariable dieses Prozesses, oder ein
    /// fehlendes lokales Registry-Quellverzeichnis auf diesem Host.
    ///
    /// # Aussage
    /// Ein Befund über **unser Werkzeug**, nicht über den Baum — strukturell
    /// derselbe Fehler wie der K79-Vorfall (siehe Moduldoku): eine
    /// Fähigkeitslücke unseres Parsers bzw. unserer Prozessumgebung, die
    /// ein syntaktisch gültiges Cargo-Konstrukt fälschlich wie einen
    /// Baumbefund aussehen ließe. Der Empfänger darf daraus **keinen**
    /// Strukturbefund über den überwachten Baum ableiten — nur: dieses
    /// Werkzeug konnte diesen Poll-Zyklus nicht auswerten.
    #[msg("workspace tooling could not process the source")]
    ToolFault,
}

impl From<WorkspaceError> for harw_dod_cap::SensorError {
    /// Bildet [`WorkspaceError`] auf `harw_dod_cap::SensorError` ab, damit
    /// [`crate::sensor::WorkspaceDriftSensor::poll`] den von
    /// `harw_dod_signals::Sensor::poll` vorgeschriebenen Fehlertyp
    /// zurückgeben und dabei `?` verwenden kann.
    ///
    /// # Description
    /// `Io`, `OutsideScope` und `MalformedSource` sind in beiden Enums
    /// bereits feldlos bzw. gleich benannt und wandern unverändert durch.
    /// [`WorkspaceError::ToolFault`] bildet seit Schließung der in der
    /// Moduldoku beschriebenen Lücke ebenfalls unverändert auf
    /// [`harw_dod_cap::SensorError::ToolFault`] ab, statt — wie zuvor,
    /// mangels Alternative — auf `MalformedSource` auszuweichen. Der
    /// Unterschied ist kein kosmetischer: `MalformedSource` ist in
    /// `harw_dod_cap` als `Permanence::Transient` eingestuft, `ToolFault`
    /// als `Permanence::Permanent`. Ein Werkzeugfehler des Drift-Sensors
    /// wiederholt sich beim nächsten Poll identisch — dieselbe Lücke im
    /// eigenen Parser verschwindet nicht von selbst, anders als ein
    /// flüchtiger Lesefehler. Mit `MalformedSource` hätte der Sentinel
    /// zunächst seine Retry-Politik für `Transient`-Fehler ausschöpfen
    /// müssen, bevor der Sensor als degradiert gemeldet wurde; mit
    /// `ToolFault` meldet sich der Sensor sofort als dauerhaft degradiert.
    /// Das ist genau die Unterscheidung, die im K79-Vorfall fehlte: nach der
    /// Vereinheitlichung auf `version.workspace = true` meldete der
    /// Drift-Sensor monatelang „Quelle fehlerhaft", obwohl der überwachte
    /// Baum in Ordnung war und **unser** Parser das Problem hatte — ein
    /// echtes defektes Manifest im Baum ergibt weiterhin `MalformedSource`
    /// (siehe [`crate::inventory::Inventory::read`] und den Test
    /// `test_read_malformed_cargo_toml_returns_malformed_source_without_file_content`).
    ///
    /// # Arguments
    /// - `err` (`WorkspaceError`): der zu übersetzende Fehler.
    ///
    /// # Returns
    /// Den entsprechenden `harw_dod_cap::SensorError`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::SensorError;
    /// use harw_dod_workspace::error::WorkspaceError;
    ///
    /// let mapped: SensorError = WorkspaceError::OutsideScope.into();
    /// assert!(matches!(mapped, SensorError::OutsideScope));
    ///
    /// let tool_mapped: SensorError = WorkspaceError::ToolFault.into();
    /// assert!(matches!(tool_mapped, SensorError::ToolFault));
    /// ```
    fn from(err: WorkspaceError) -> Self {
        match err {
            WorkspaceError::Io(io) => Self::Io(io),
            WorkspaceError::OutsideScope => Self::OutsideScope,
            WorkspaceError::MalformedSource => Self::MalformedSource,
            WorkspaceError::ToolFault => Self::ToolFault,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceError;
    use harw_dod_cap::{Permanence, SensorError};

    #[test]
    fn test_malformed_source_display_is_exact_and_content_free() {
        assert_eq!(
            WorkspaceError::MalformedSource.to_string(),
            "workspace manifest or lockfile could not be parsed"
        );
    }

    #[test]
    fn test_tool_fault_display_is_exact_and_content_free() {
        assert_eq!(
            WorkspaceError::ToolFault.to_string(),
            "workspace tooling could not process the source"
        );
    }

    #[test]
    fn test_tool_fault_display_differs_from_malformed_source() {
        assert_ne!(
            WorkspaceError::ToolFault.to_string(),
            WorkspaceError::MalformedSource.to_string()
        );
    }

    #[test]
    fn test_outside_scope_display_is_exact() {
        assert_eq!(
            WorkspaceError::OutsideScope.to_string(),
            "path resolves outside the sensor read scope"
        );
    }

    #[test]
    fn test_io_source_links_to_underlying_error() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = WorkspaceError::Io(source);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_from_workspace_error_maps_outside_scope_to_sensor_error() {
        let mapped: SensorError = WorkspaceError::OutsideScope.into();
        assert!(matches!(mapped, SensorError::OutsideScope));
    }

    #[test]
    fn test_from_workspace_error_maps_malformed_source_to_sensor_error() {
        let mapped: SensorError = WorkspaceError::MalformedSource.into();
        assert!(matches!(mapped, SensorError::MalformedSource));
    }

    #[test]
    fn test_from_workspace_error_maps_tool_fault_to_sensor_error() {
        let mapped: SensorError = WorkspaceError::ToolFault.into();
        assert!(matches!(mapped, SensorError::ToolFault));
    }

    #[test]
    fn test_from_workspace_error_tool_fault_is_permanent() {
        let mapped: SensorError = WorkspaceError::ToolFault.into();
        assert_eq!(mapped.permanence(), Permanence::Permanent);
    }

    #[test]
    fn test_from_workspace_error_maps_io_to_sensor_error() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "boom");
        let mapped: SensorError = WorkspaceError::Io(source).into();
        assert!(matches!(mapped, SensorError::Io(_)));
    }
}
