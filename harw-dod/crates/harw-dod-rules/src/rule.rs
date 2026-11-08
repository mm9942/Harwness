//! Der Regel-Trait und sein Eingabekontext (Knoten AW4-03, Contract-Master §G.1).
//!
//! # Verantwortungsbereich
//! [`Rule`] ist die eine Schnittstelle, über die aus Beobachtungen Befunde
//! werden. [`RuleContext`] bündelt alles, was eine Regel zur Auswertung
//! braucht — nichts, was eine Regel selbst beschaffen müsste: Zeit, Samples,
//! Ereignisse, Baselines, den Netzbereich und (PL-100 WP1.6) den
//! persistierten Listener-Zustand
//! ([`RuleContext::listener_history`], gelesen von `listener-new`) sowie die
//! [`RuleConfig`] ([`RuleContext::rule_config`]: Workspace-Wurzeln,
//! zusätzliche Home-Verzeichnisse, flüchtiger Portbereich).
//!
//! # Reine Funktionen — die wichtigste Auflage dieser Crate
//! Eine [`Rule::evaluate`]-Implementierung darf kein I/O ausführen, kein Netz
//! ansprechen, keinen Subprozess starten und **nie die Systemuhr lesen**.
//! `now` ist ein Feld von [`RuleContext`], keine Methode, die intern
//! `jiff::Timestamp::now()` aufrufen könnte — eine Regel bekommt die Zeit nur
//! injiziert. Diese Auflage ist keine Stilfrage: eine Regel, die ihre
//! Umgebung selbst liest, ist gegen keinen goldenen Fall mehr prüfbar, und
//! goldene Fälle (feste Eingabe, festes erwartetes Ergebnis) sind die einzige
//! Art, eine Sicherheitsregel verlässlich zu prüfen. Jede Regel dieser Crate
//! trägt deshalb einen Test, der sie zweimal mit identischem `RuleContext`
//! auswertet und byteidentische Ergebnisse erwartet (siehe die Tests in
//! [`crate::rules`]).
//!
//! # Nebenläufigkeit
//! `Rule: Send + Sync`, damit ein Aufrufer (z. B. [`crate::engine::run_rules`])
//! mehrere Regeln parallel über denselben, unveränderlichen [`RuleContext`]
//! laufen lassen kann, ohne Sperren. Jede konkrete Regel in dieser Crate ist
//! ein zustandsloser Unit-Struct und damit trivial `Send + Sync`.
//!
//! # Fehler
//! [`Rule::evaluate`] ist total: es liefert `Vec<Finding<Raw>>`, kein
//! `Result` — eine Regel, die nichts auslöst, liefert einen leeren Vektor,
//! keinen Fehler. Fehler gibt es nur beim **Konfigurieren**: die Setter der
//! [`RuleConfig`] lehnen unbrauchbare Werte mit [`RuleConfigError`] ab
//! (inhaltsfrei), bevor eine Regel sie je sieht.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::EgressFlowRule;
//!
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &[],
//!     baselines: &[],
//!     network_scope: &scope,
//!     listener_history: None,
//!     rule_config: None,
//! };
//! assert!(EgressFlowRule.evaluate(&ctx).is_empty());
//! ```
//
use std::fmt;
use std::ops::RangeInclusive;
use std::path::Path;

use harw_authority::NetworkScope;
use harw_dod_signals::{HostSample, SecurityEvent};
use jiff::Timestamp;

use crate::baseline::Baseline;
use crate::finding::{Finding, Raw};
use crate::listener_history::ListenerHistory;

/// Eine Sicherheitsregel: reine Funktion von Beobachtungen zu Befunden.
///
/// # Description
/// Jede Implementierung prüft [`RuleContext`] und liefert die dabei
/// ausgelösten Befunde als `Finding<Raw>` — noch nicht zertifiziert (siehe
/// [`crate::finding`]-Moduldoku). Siehe Moduldoku für die Reinheitsauflage.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Concurrency
/// `Send + Sync`, siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_authority::NetworkScope;
/// use harw_dod_rules::rule::{Rule, RuleContext};
/// use harw_dod_rules::rules::EgressFlowRule;
///
/// let scope = NetworkScope::empty();
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &[],
///     baselines: &[],
///     network_scope: &scope,
///     listener_history: None,
///     rule_config: None,
/// };
/// assert_eq!(EgressFlowRule.id(), "egress-flow");
/// ```
pub trait Rule: Send + Sync {
    /// Die stabile Kennung dieser Regel.
    ///
    /// # Returns
    /// Ein statischer, nicht-leerer Bezeichner (z. B. `"egress-flow"`). Wird
    /// unverändert in jeden von dieser Regel erzeugten [`Finding::rule_id`]
    /// übernommen.
    fn id(&self) -> &'static str;

    /// Prüft den Kontext und liefert die ausgelösten Befunde.
    ///
    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): die Beobachtungen und der Bezugsrahmen,
    ///   gegen die diese Regel prüft.
    ///
    /// # Returns
    /// Die ausgelösten Befunde als `Vec<Finding<Raw>>`. Leer, wenn nichts
    /// ausgelöst hat.
    ///
    /// # Errors
    /// Keine — siehe Moduldoku.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>>;
}

/// Der Eingabekontext einer Regelauswertung.
///
/// # Description
/// Bündelt alle Beobachtungen und den Bezugsrahmen, den eine Regel dieser
/// Crate braucht. `network_scope` ist ein Zusatz gegenüber der ursprünglichen
/// Skizze (die dort vorgesehene Lücke „was die konkreten Regeln unten sonst
/// brauchen“) — [`crate::rules::EgressFlowRule`] braucht ihn, um
/// `harw_authority::NetworkScope::allows`/`allows_addr` zu benutzen, statt die
/// Bereichsprüfung selbst zweitzuschreiben.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_authority::NetworkScope;
/// use harw_dod_rules::rule::RuleContext;
///
/// let scope = NetworkScope::empty();
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &[],
///     baselines: &[],
///     network_scope: &scope,
///     listener_history: None,
///     rule_config: None,
/// };
/// assert!(ctx.samples.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct RuleContext<'a> {
    /// **Injizierte** Zeit. Eine Regel liest nie die Systemuhr. Befunde über
    /// Ereignisse oder Samples tragen **nicht** diese Zeit, sondern das
    /// `observed_at` der auslösenden Beobachtung (PL-100 WP0.2); `now` ist
    /// die Auswertungszeit für Befunde ohne einzelne auslösende Beobachtung.
    pub now: Timestamp,
    /// Die zu prüfenden Messwerte.
    pub samples: &'a [HostSample],
    /// Die zu prüfenden Sicherheitsereignisse.
    pub events: &'a [SecurityEvent],
    /// Die bekannten Baselines, gegen die eine Regel abweichende
    /// Beobachtungen bewerten kann (siehe [`crate::baseline`]-Moduldoku).
    /// Je Metrik höchstens eine aktive Baseline: der Aufrufer prüft den Satz
    /// mit [`crate::baseline::check_baselines`]. Eine doppelte Metrik oder
    /// einen ungültigen Bereich bewertet
    /// [`crate::rules::BaselineDeviationRule`] nicht, meldet ihn aber als
    /// eigenen Befund (fail-closed, PL-100 WP1.7).
    pub baselines: &'a [Baseline],
    /// Der erlaubte Netzbereich, gegen den
    /// [`crate::rules::EgressFlowRule`] beobachtete Ziele prüft.
    pub network_scope: &'a NetworkScope,
    /// Der persistierte Listener-Zustand für
    /// [`crate::rules::ListenerNewRule`] (PL-100 WP1.6). `None` oder eine
    /// noch nicht vorbereitete Historie: `listener-new` meldet nichts. Der
    /// Aufrufer (Sentinel) lädt und speichert sie und lernt nach jedem Zyklus
    /// über [`ListenerHistory::observe`] — eine Regel liest sie nur (siehe
    /// [`crate::listener_history`]-Moduldoku).
    pub listener_history: Option<&'a ListenerHistory>,
    /// Konfiguration der Regeln (PL-100 WP1.6), z. B. die Workspace-Wurzeln
    /// für [`crate::rules::ExecWritablePathRule`] und der flüchtige
    /// Portbereich für [`crate::rules::ListenerNewRule`]. `None` heißt
    /// Vorgaben ([`RuleConfig::default`]). Wer hier eine Konfiguration
    /// übergibt, lernt die Listener-Historie mit derselben
    /// ([`ListenerHistory::observe_with_config`]).
    pub rule_config: Option<&'a RuleConfig>,
}

/// Konfiguration der Regeln, die nicht aus Beobachtungen stammt
/// (PL-100 WP1.6).
///
/// # Description
/// Trägt
/// - die **Workspace-Wurzeln**: Verzeichnisse, in denen Agenten und Jobs
///   Code schreiben. Ein Programmstart unterhalb einer solchen Wurzel meldet
///   [`crate::rules::ExecWritablePathRule`] wie einen Start aus einem
///   Home-Verzeichnis. Jede Wurzel wird beim Hinzufügen lexikalisch
///   normalisiert (siehe [`RuleConfig::with_workspace_root`]); Duplikate
///   werden zusammengefasst.
/// - zusätzliche **Home-Verzeichnisse** (siehe [`RuleConfig::with_home_dir`]):
///   neben den festen Orten `/home/<name>`, `/root`, `/var/home/<name>` und
///   `/var/roothome` gelten sie für [`crate::rules::ExecWritablePathRule`]
///   und [`crate::rules::PersistenceWriteRule`] als Home-Verzeichnis
///   (verschachtelte oder AD-Layouts wie `/home/<domäne>/<name>`,
///   Dienstkonten).
/// - den **flüchtigen Portbereich** (`net.ipv4.ip_local_port_range`,
///   Vorgabe [`RuleConfig::DEFAULT_EPHEMERAL_PORTS`]): Ports, die der Kernel
///   für `bind(…, 0)` und nicht verbundene UDP-Sockets zufällig vergibt.
///   [`crate::rules::ListenerNewRule`] meldet einen neuen Listener in diesem
///   Bereich ohne bekanntes TCP nur als `Low`/`Anomaly` und fasst als UDP
///   bekannte zu einem groben Schlüssel zusammen (siehe
///   [`crate::listener_history::ListenerPort`]).
///
/// # Errors
/// Nur beim Setzen, siehe [`RuleConfigError`].
///
/// # Concurrency
/// Reiner Wert, `Send + Sync`.
///
/// # Examples
/// ```rust
/// use std::path::Path;
///
/// use harw_dod_rules::rule::RuleConfig;
///
/// # fn main() -> Result<(), harw_dod_rules::rule::RuleConfigError> {
/// let config = RuleConfig::new().with_workspace_root(Path::new("/srv/agents/./ws/"))?;
/// assert_eq!(config.workspace_roots().to_vec(), vec!["/srv/agents/ws".to_owned()]);
/// assert!(RuleConfig::new().with_workspace_root(Path::new("/")).is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleConfig {
    // Normalisierte, absolute Workspace-Wurzeln (UTF-8, nie `/`), ohne
    // Duplikate, in Einfügereihenfolge.
    workspace_roots: Vec<String>,
    // Normalisierte, absolute zusätzliche Home-Verzeichnisse (UTF-8, nie `/`,
    // nie unter einem Systemverzeichnis), ohne Duplikate, in
    // Einfügereihenfolge.
    home_dirs: Vec<String>,
    // Erster Port des flüchtigen Bereichs; `>= MIN_EPHEMERAL_PORT`.
    ephemeral_first: u16,
    // Letzter Port des flüchtigen Bereichs; `>= ephemeral_first`.
    ephemeral_last: u16,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            workspace_roots: Vec::new(),
            home_dirs: Vec::new(),
            ephemeral_first: *Self::DEFAULT_EPHEMERAL_PORTS.start(),
            ephemeral_last: *Self::DEFAULT_EPHEMERAL_PORTS.end(),
        }
    }
}

impl RuleConfig {
    /// Vorgabe des flüchtigen Portbereichs: Linux' Standardwert von
    /// `net.ipv4.ip_local_port_range`.
    pub const DEFAULT_EPHEMERAL_PORTS: RangeInclusive<u16> = 32_768..=60_999;

    /// Kleinster zulässiger Anfang des flüchtigen Bereichs: bekannte Ports
    /// (`< 1024`) werden nie zusammengefasst.
    pub const MIN_EPHEMERAL_PORT: u16 = 1024;

    /// Oberste Verzeichnisse, unter denen [`Self::with_home_dir`] kein
    /// Home-Verzeichnis annimmt: Systemkonten tragen dort oft ihr „Home"
    /// (`bin` → `/bin`, `daemon` → `/usr/sbin`, `sys` → `/dev`), und jeder
    /// Programmstart aus `/bin` würde sonst zum Start aus einem
    /// Home-Verzeichnis.
    pub const SYSTEM_TOP_DIRS: &'static [&'static str] = &[
        "bin", "boot", "dev", "etc", "lib", "lib32", "lib64", "libx32", "proc", "sbin", "sys",
        "usr",
    ];

    /// Eine Konfiguration ohne Workspace-Wurzeln mit dem vorgegebenen
    /// flüchtigen Portbereich.
    ///
    /// # Returns
    /// Dasselbe wie `RuleConfig::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt den flüchtigen Portbereich.
    ///
    /// # Description
    /// Der Aufrufer übernimmt ihn aus `net.ipv4.ip_local_port_range` des
    /// Hosts, falls dieser vom Standard abweicht.
    ///
    /// # Arguments
    /// - `first` (`u16`): erster Port (inklusiv), mindestens
    ///   [`Self::MIN_EPHEMERAL_PORT`].
    /// - `last` (`u16`): letzter Port (inklusiv), mindestens `first`.
    ///
    /// # Returns
    /// Die geänderte Konfiguration.
    ///
    /// # Errors
    /// [`RuleConfigError::EphemeralPortRangeInvalid`], wenn `first > last`
    /// oder `first < MIN_EPHEMERAL_PORT`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_rules::rule::RuleConfig;
    ///
    /// # fn main() -> Result<(), harw_dod_rules::rule::RuleConfigError> {
    /// let config = RuleConfig::new().with_ephemeral_ports(40_000, 50_000)?;
    /// assert!(config.is_ephemeral_port(45_000));
    /// assert!(!config.is_ephemeral_port(8080));
    /// assert!(RuleConfig::new().with_ephemeral_ports(80, 50_000).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_ephemeral_ports(mut self, first: u16, last: u16) -> Result<Self, RuleConfigError> {
        if first > last || first < Self::MIN_EPHEMERAL_PORT {
            return Err(RuleConfigError::EphemeralPortRangeInvalid);
        }
        self.ephemeral_first = first;
        self.ephemeral_last = last;
        Ok(self)
    }

    /// Der flüchtige Portbereich.
    ///
    /// # Returns
    /// Erster bis letzter Port, inklusiv.
    #[must_use]
    pub fn ephemeral_ports(&self) -> RangeInclusive<u16> {
        self.ephemeral_first..=self.ephemeral_last
    }

    /// Ob `port` im flüchtigen Bereich liegt.
    ///
    /// # Arguments
    /// - `port` (`u16`): ein lokaler Port.
    ///
    /// # Returns
    /// `true` für `ephemeral_ports().contains(&port)`.
    #[must_use]
    pub fn is_ephemeral_port(&self, port: u16) -> bool {
        (self.ephemeral_first..=self.ephemeral_last).contains(&port)
    }

    /// Fügt eine Workspace-Wurzel hinzu.
    ///
    /// # Description
    /// Die Wurzel wird lexikalisch normalisiert (`.`, `..` und doppelte
    /// Trenner aufgelöst, kein Symlink-Auflösen — Regeln führen kein I/O
    /// aus). Eine bereits vorhandene Wurzel wird nicht doppelt geführt.
    ///
    /// # Arguments
    /// - `root` (`&Path`): ein absoluter Verzeichnispfad. Der Aufrufer
    ///   kanonisiert ihn vorher, falls er Symlinks enthält.
    ///
    /// # Returns
    /// Die erweiterte Konfiguration.
    ///
    /// # Errors
    /// - [`RuleConfigError::WorkspaceRootNotUtf8`]: kein UTF-8 (Ereignispfade
    ///   sind UTF-8, ein solcher Pfad träfe nie).
    /// - [`RuleConfigError::WorkspaceRootNotAbsolute`]: relativer Pfad.
    /// - [`RuleConfigError::WorkspaceRootIsFilesystemRoot`]: `/` erfasste
    ///   jeden Programmstart.
    ///
    /// # Examples
    /// Siehe Typdoku.
    pub fn with_workspace_root(mut self, root: &Path) -> Result<Self, RuleConfigError> {
        let normalized = normalized_dir(root).map_err(|issue| match issue {
            DirIssue::NotUtf8 => RuleConfigError::WorkspaceRootNotUtf8,
            DirIssue::NotAbsolute => RuleConfigError::WorkspaceRootNotAbsolute,
            DirIssue::FilesystemRoot => RuleConfigError::WorkspaceRootIsFilesystemRoot,
        })?;
        if !self.workspace_roots.contains(&normalized) {
            self.workspace_roots.push(normalized);
        }
        Ok(self)
    }

    /// Die normalisierten Workspace-Wurzeln.
    ///
    /// # Returns
    /// Absolute Pfade ohne abschließenden Trenner, in Einfügereihenfolge.
    #[must_use]
    pub fn workspace_roots(&self) -> &[String] {
        &self.workspace_roots
    }

    /// Fügt ein zusätzliches Home-Verzeichnis hinzu.
    ///
    /// # Description
    /// Neben den festen Orten (`/home/<name>`, `/root`, `/var/home/<name>`,
    /// `/var/roothome`) gilt `dir` für
    /// [`crate::rules::ExecWritablePathRule`] und
    /// [`crate::rules::PersistenceWriteRule`] als Home-Verzeichnis. Gedacht
    /// für die verschiedenen Home-Verzeichnisse aus `/etc/passwd`
    /// (verschachtelte oder AD-Layouts wie `/home/<domäne>/<name>`,
    /// Dienstkonten wie `/var/lib/git`). Ein Verzeichnis unter einem
    /// Systemverzeichnis ([`Self::SYSTEM_TOP_DIRS`]) wird abgelehnt; der
    /// Aufrufer überspringt solche Einträge. Normalisierung wie bei
    /// [`Self::with_workspace_root`]; ein bereits vorhandenes Verzeichnis wird
    /// nicht doppelt geführt.
    ///
    /// # Arguments
    /// - `dir` (`&Path`): ein absoluter Verzeichnispfad. Der Aufrufer
    ///   kanonisiert ihn vorher, falls er Symlinks enthält (der Sensor meldet
    ///   aufgelöste Pfade).
    ///
    /// # Returns
    /// Die erweiterte Konfiguration.
    ///
    /// # Errors
    /// - [`RuleConfigError::HomeDirNotUtf8`]: kein UTF-8.
    /// - [`RuleConfigError::HomeDirNotAbsolute`]: relativer Pfad.
    /// - [`RuleConfigError::HomeDirIsFilesystemRoot`]: `/`.
    /// - [`RuleConfigError::HomeDirIsSystemDir`]: unter einem
    ///   Systemverzeichnis wie `/bin` oder `/usr`.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::Path;
    ///
    /// use harw_dod_rules::rule::{RuleConfig, RuleConfigError};
    ///
    /// # fn main() -> Result<(), RuleConfigError> {
    /// let config = RuleConfig::new().with_home_dir(Path::new("/home/corp/alice/"))?;
    /// assert_eq!(config.home_dirs().to_vec(), vec!["/home/corp/alice".to_owned()]);
    /// assert_eq!(
    ///     RuleConfig::new().with_home_dir(Path::new("/usr/sbin")),
    ///     Err(RuleConfigError::HomeDirIsSystemDir)
    /// );
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_home_dir(mut self, dir: &Path) -> Result<Self, RuleConfigError> {
        let normalized = normalized_dir(dir).map_err(|issue| match issue {
            DirIssue::NotUtf8 => RuleConfigError::HomeDirNotUtf8,
            DirIssue::NotAbsolute => RuleConfigError::HomeDirNotAbsolute,
            DirIssue::FilesystemRoot => RuleConfigError::HomeDirIsFilesystemRoot,
        })?;
        let top = normalized.split('/').find(|part| !part.is_empty());
        if top.is_some_and(|top| Self::SYSTEM_TOP_DIRS.contains(&top)) {
            return Err(RuleConfigError::HomeDirIsSystemDir);
        }
        if !self.home_dirs.contains(&normalized) {
            self.home_dirs.push(normalized);
        }
        Ok(self)
    }

    /// Die normalisierten zusätzlichen Home-Verzeichnisse.
    ///
    /// # Returns
    /// Absolute Pfade ohne abschließenden Trenner, in Einfügereihenfolge.
    #[must_use]
    pub fn home_dirs(&self) -> &[String] {
        &self.home_dirs
    }
}

// Warum ein Pfad kein brauchbares Verzeichnis für die `RuleConfig` ist.
enum DirIssue {
    // Kein UTF-8.
    NotUtf8,
    // Nicht absolut (oder leer).
    NotAbsolute,
    // Die Dateisystemwurzel `/`.
    FilesystemRoot,
}

// Normalisiert ein absolutes Verzeichnis lexikalisch zu `/a/b` (siehe
// `crate::rules::path_class::normalized_components`).
fn normalized_dir(dir: &Path) -> Result<String, DirIssue> {
    let text = dir.to_str().ok_or(DirIssue::NotUtf8)?;
    let components =
        crate::rules::path_class::normalized_components(text).ok_or(DirIssue::NotAbsolute)?;
    if components.is_empty() {
        return Err(DirIssue::FilesystemRoot);
    }
    Ok(format!("/{}", components.join("/")))
}

/// Ein Wert ist für [`RuleConfig`] unbrauchbar.
///
/// # Description
/// Inhaltsfrei: keine Meldung interpoliert den Pfad oder die Ports.
///
/// # Errors
/// Von [`RuleConfig::with_workspace_root`], [`RuleConfig::with_home_dir`]
/// und [`RuleConfig::with_ephemeral_ports`] erzeugt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleConfigError {
    /// Der Pfad ist kein gültiges UTF-8.
    WorkspaceRootNotUtf8,
    /// Der Pfad ist nicht absolut.
    WorkspaceRootNotAbsolute,
    /// Der Pfad ist die Dateisystemwurzel `/`.
    WorkspaceRootIsFilesystemRoot,
    /// Das Home-Verzeichnis ist kein gültiges UTF-8.
    HomeDirNotUtf8,
    /// Das Home-Verzeichnis ist nicht absolut.
    HomeDirNotAbsolute,
    /// Das Home-Verzeichnis ist die Dateisystemwurzel `/`.
    HomeDirIsFilesystemRoot,
    /// Das Home-Verzeichnis liegt unter einem Systemverzeichnis
    /// ([`RuleConfig::SYSTEM_TOP_DIRS`]).
    HomeDirIsSystemDir,
    /// Der flüchtige Portbereich ist leer oder beginnt unter
    /// [`RuleConfig::MIN_EPHEMERAL_PORT`].
    EphemeralPortRangeInvalid,
}

impl fmt::Display for RuleConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspaceRootNotUtf8 => f.write_str("workspace root is not valid utf-8"),
            Self::WorkspaceRootNotAbsolute => f.write_str("workspace root is not an absolute path"),
            Self::WorkspaceRootIsFilesystemRoot => {
                f.write_str("workspace root must not be the filesystem root")
            }
            Self::HomeDirNotUtf8 => f.write_str("home directory is not valid utf-8"),
            Self::HomeDirNotAbsolute => f.write_str("home directory is not an absolute path"),
            Self::HomeDirIsFilesystemRoot => {
                f.write_str("home directory must not be the filesystem root")
            }
            Self::HomeDirIsSystemDir => {
                f.write_str("home directory must not lie below a system directory")
            }
            Self::EphemeralPortRangeInvalid => f.write_str(
                "ephemeral port range must be non-empty and start at or above port 1024",
            ),
        }
    }
}

impl std::error::Error for RuleConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_rule_context_exposes_given_slices() {
        let scope = NetworkScope::empty();
        let ctx = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &[],
            baselines: &[],
            network_scope: &scope,
            listener_history: None,
            rule_config: None,
        };
        assert!(ctx.samples.is_empty());
        assert!(ctx.events.is_empty());
        assert!(ctx.baselines.is_empty());
        assert_eq!(ctx.now, Timestamp::UNIX_EPOCH);
        assert!(ctx.listener_history.is_none());
        assert!(ctx.rule_config.is_none());
    }

    #[test]
    fn test_rule_config_normalizes_and_deduplicates_workspace_roots() -> TestResult {
        let config = RuleConfig::new()
            .with_workspace_root(Path::new("/srv/agents/ws/"))
            .map_err(ctx("absolute Wurzel"))?
            .with_workspace_root(Path::new("/srv//agents/./x/../ws"))
            .map_err(ctx("gleiche Wurzel, anders geschrieben"))?
            .with_workspace_root(Path::new("/home/dev/project"))
            .map_err(ctx("zweite Wurzel"))?;
        assert_eq!(
            config.workspace_roots().to_vec(),
            vec!["/srv/agents/ws".to_owned(), "/home/dev/project".to_owned()]
        );
        assert_eq!(RuleConfig::new(), RuleConfig::default());
        Ok(())
    }

    #[test]
    fn test_rule_config_rejects_relative_and_filesystem_root() {
        assert_eq!(
            RuleConfig::new().with_workspace_root(Path::new(".")),
            Err(RuleConfigError::WorkspaceRootNotAbsolute)
        );
        assert_eq!(
            RuleConfig::new().with_workspace_root(Path::new("workspace")),
            Err(RuleConfigError::WorkspaceRootNotAbsolute)
        );
        assert_eq!(
            RuleConfig::new().with_workspace_root(Path::new("/")),
            Err(RuleConfigError::WorkspaceRootIsFilesystemRoot)
        );
        assert_eq!(
            RuleConfig::new().with_workspace_root(Path::new("/tmp/..")),
            Err(RuleConfigError::WorkspaceRootIsFilesystemRoot)
        );
    }

    #[test]
    fn test_rule_config_ephemeral_ports_default_and_override() -> TestResult {
        let default = RuleConfig::new();
        assert_eq!(default.ephemeral_ports(), 32_768..=60_999);
        assert!(default.is_ephemeral_port(32_768));
        assert!(default.is_ephemeral_port(60_999));
        assert!(!default.is_ephemeral_port(32_767));
        assert!(!default.is_ephemeral_port(61_000));
        assert!(!default.is_ephemeral_port(8080));

        let custom = RuleConfig::new()
            .with_ephemeral_ports(49_152, 65_535)
            .map_err(ctx("IANA-Bereich"))?;
        assert!(custom.is_ephemeral_port(65_535));
        assert!(!custom.is_ephemeral_port(40_000));
        Ok(())
    }

    #[test]
    fn test_rule_config_rejects_invalid_ephemeral_ranges() {
        assert_eq!(
            RuleConfig::new().with_ephemeral_ports(50_000, 40_000),
            Err(RuleConfigError::EphemeralPortRangeInvalid)
        );
        assert_eq!(
            RuleConfig::new().with_ephemeral_ports(1023, 60_999),
            Err(RuleConfigError::EphemeralPortRangeInvalid)
        );
        assert!(RuleConfig::new().with_ephemeral_ports(1024, 1024).is_ok());
    }

    #[test]
    fn test_rule_config_normalizes_and_deduplicates_home_dirs() -> TestResult {
        let config = RuleConfig::new()
            .with_home_dir(Path::new("/home/corp/alice/"))
            .map_err(ctx("AD-Home"))?
            .with_home_dir(Path::new("/home//corp/./x/../alice"))
            .map_err(ctx("gleiches Home, anders geschrieben"))?
            .with_home_dir(Path::new("/var/lib/git"))
            .map_err(ctx("Dienstkonto"))?;
        assert_eq!(
            config.home_dirs().to_vec(),
            vec!["/home/corp/alice".to_owned(), "/var/lib/git".to_owned()]
        );
        assert!(config.workspace_roots().is_empty());
        Ok(())
    }

    #[test]
    fn test_rule_config_rejects_unusable_home_dirs() {
        for (dir, expected) in [
            ("relative/home", RuleConfigError::HomeDirNotAbsolute),
            ("", RuleConfigError::HomeDirNotAbsolute),
            ("/", RuleConfigError::HomeDirIsFilesystemRoot),
            ("/home/..", RuleConfigError::HomeDirIsFilesystemRoot),
            ("/bin", RuleConfigError::HomeDirIsSystemDir),
            ("/usr/sbin", RuleConfigError::HomeDirIsSystemDir),
            ("/dev", RuleConfigError::HomeDirIsSystemDir),
            ("/home/../usr/games", RuleConfigError::HomeDirIsSystemDir),
        ] {
            assert_eq!(
                RuleConfig::new().with_home_dir(Path::new(dir)),
                Err(expected),
                "{dir}"
            );
        }
        // Ein Name, der nur mit einem Systemverzeichnis beginnt, ist keins.
        assert!(
            RuleConfig::new()
                .with_home_dir(Path::new("/usrhome/a"))
                .is_ok()
        );
    }

    #[test]
    fn test_rule_config_error_display_is_content_free() {
        for error in [
            RuleConfigError::WorkspaceRootNotUtf8,
            RuleConfigError::HomeDirNotAbsolute,
            RuleConfigError::HomeDirIsSystemDir,
            RuleConfigError::EphemeralPortRangeInvalid,
        ] {
            assert!(!error.to_string().contains('/'), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_rule_config_rejects_non_utf8_root() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let root = Path::new(OsStr::from_bytes(b"/srv/\xff"));
        assert_eq!(
            RuleConfig::new().with_workspace_root(root),
            Err(RuleConfigError::WorkspaceRootNotUtf8)
        );
        assert_eq!(
            RuleConfig::new().with_home_dir(root),
            Err(RuleConfigError::HomeDirNotUtf8)
        );
    }
}
