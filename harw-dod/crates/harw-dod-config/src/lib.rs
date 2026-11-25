//! Der gemeinsame, restriktive Konfigurationsvertrag fuer die DoD-
//! Beobachtungskette.
//!
//! Diese Crate parst und validiert ausschliesslich die deklarierte
//! Systemkonfiguration. Sie loest absichtlich keine lebenden cgroup-v2-
//! Identitaeten auf: das ist Host-I/O und bleibt beim Loader. Der Loader muss
//! fehlende oder nach Loeschung wiederangelegte cgroups deshalb als
//! Degradierung behandeln, nie als Grund einen Pfad erneut und unbemerkt zu
//! akzeptieren.
//!
//! [`load_system_config`] ist die Produktionsquelle. Sie akzeptiert nur
//! [`SYSTEM_CONFIG_PATH`] und oeffnet jedes Pfadglied ohne Symlink-Folge,
//! bevor sie Eigentum und Schreibrechte auf den bereits offenen
//! Deskriptoren prueft. [`load_config`] verwendet fuer einen explizit
//! uebergebenen administrativen `--config`-Pfad denselben sicheren
//! Descriptor-Walk.
//!
//! # Optionale Tabelle `[sentinel]` (PL-100)
//! Neben den Profilen traegt die Datei optional Einstellungen von
//! `harw-sentinel` ([`SentinelSettings`], [`Config::sentinel`]):
//!
//! ```toml
//! [sentinel]
//! probe_peer_users = ["harw-probe-bpf", "harw-probe-fs"]
//! finding_cooldown_secs = 600
//! ```
//!
//! Jeder Schluessel ist optional; ein fehlender Schluessel (oder die ganze
//! Tabelle) heisst „Vorgabe des Sentinels" (dieselbe wie ohne das
//! zugehoerige Kommandozeilenflag), und ein angegebenes Flag gewinnt immer.
//! Die Tabelle ist additiv: `schema_version` bleibt `1`. Weil
//! `deny_unknown_fields` gilt, lehnt ein Leser **vor** dieser Fassung eine
//! Datei mit `[sentinel]` ab — die ausgelieferten Dateien fuehren die
//! Tabelle deshalb nur auskommentiert (Reader-first, PL-100 I11).
//!
//! `container_sensor` (`"auto"`, `"on"`, `"off"`; [`ContainerSensorMode`])
//! schaltet den Container-Sensor des Sentinels (PL-100 WP1.8).
//!
//! # Optionale Tabelle `[probe_fs]` (PL-100 WP1.6)
//! Der Beobachtungsbereich von `harw-probe-fs` ([`ProbeFsSettings`],
//! [`Config::probe_fs`]): `scope_roots` (absolute Verzeichnisse, jede
//! Schreibaenderung darunter zaehlt), `home_roots` (absolute Verzeichnisse
//! mit Home-Verzeichnissen; darunter zaehlen nur Pfade, die mit einem
//! Eintrag von `home_entries` beginnen) und `home_entries` (relativ zu einem
//! Home-Verzeichnis, z. B. `.ssh`). Dieselben Regeln wie `[sentinel]`:
//! optional je Schluessel, das Kommandozeilenflag der Sonde gewinnt,
//! additiv bei Schema `1`, ausgeliefert auskommentiert.
//!
//! # Examples
//! ```rust
//! use harw_dod_config::Config;
//!
//! # fn main() -> Result<(), harw_dod_config::ConfigError> {
//! let config = Config::from_toml(
//!     "schema_version = 1\nmode = \"observe\"\nactive_profile = \"host\"\n\n\
//!      [sentinel]\nfinding_cooldown_secs = 300\n\n\
//!      [profiles.host]\nscope = \"host\"\nsensors = [\"exec\"]\n",
//! )?;
//! assert_eq!(config.sentinel().finding_cooldown_secs(), Some(300));
//! assert_eq!(config.sentinel().probe_peer_users(), None);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use ipnet::IpNet;
use rustix::fs::{CWD, FileType, Mode, OFlags};
use rustix::io::retry_on_intr;
use serde::Deserialize;

/// Der einzige implizite Produktionspfad fuer die DoD-Systemkonfiguration.
pub const SYSTEM_CONFIG_PATH: &str = "/etc/harw-dod/config.toml";

/// Obergrenze fuer eine Konfiguration, bevor TOML geparst wird.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Hoechstzahl der Eintraege von `[sentinel] probe_peer_users`.
pub const MAX_PROBE_PEER_USERS: usize = 32;

/// Hoechstlaenge eines Eintrags von `[sentinel] probe_peer_users` in Bytes
/// (dieselbe Grenze wie `--probe-peer-user` von `harw-sentinel`).
pub const MAX_PROBE_PEER_USER_BYTES: usize = 256;

/// Groesster zulaessiger Wert von `[sentinel] finding_cooldown_secs` (7 Tage;
/// derselbe wie `--finding-cooldown-secs` von `harw-sentinel`).
pub const MAX_FINDING_COOLDOWN_SECS: u64 = 7 * 24 * 60 * 60;

/// Hoechstzahl der Eintraege je Liste von `[probe_fs]` (`scope_roots`,
/// `home_roots`, `home_entries`).
pub const MAX_PROBE_FS_PATHS: usize = 64;

/// Hoechstlaenge eines Pfads von `[probe_fs]` in Bytes (`PATH_MAX`).
pub const MAX_PROBE_FS_PATH_BYTES: usize = 4096;

/// Liest die feste, vertrauenswuerdige Produktionskonfiguration.
///
/// Der Pfad ist absichtlich nicht konfigurierbar: es gibt keine Suche im
/// Repository, im Home-Verzeichnis oder unter `HARW_HOME`. Alle
/// Verzeichnisse von `/` bis `/etc/harw-dod` sowie die Datei selbst muessen
/// root-gehoeren und duerfen fuer Gruppe oder Andere nicht schreibbar sein.
/// Die Pruefung erfolgt auf Deskriptoren, die mit `O_NOFOLLOW` geoeffnet
/// wurden; ein Umbenennen zwischen einer Pfadpruefung und dem spaeteren Lesen
/// kann die Vertrauensentscheidung daher nicht austauschen.
pub fn load_system_config() -> Result<Config, ConfigError> {
    let bytes = read_limited(open_trusted_system_config()?)?;
    Config::from_bytes(&bytes)
}

/// Liest einen explizit vom Administrator oder einem Test uebergebenen Pfad.
///
/// Der explizite Pfad ist ebenfalls eine Vertrauensquelle: jedes Pfadglied
/// wird als Descriptor ohne Symlink-Folge geoeffnet und muss root-gehoeren
/// sowie fuer Gruppe und Andere nicht schreibbar sein. Dadurch ist die API
/// fuer `--config` und den festen Produktionspfad gleichermassen geeignet.
/// Relative Pfade werden abgewiesen, damit die Vertrauensquelle nie vom
/// aktuellen Arbeitsverzeichnis abhaengt.
pub fn load_config(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    let bytes = read_limited(open_explicit_config(path.as_ref())?)?;
    Config::from_bytes(&bytes)
}

/// Vollstaendig geparste, aber noch nicht auf ein aktives Profil reduzierte
/// Systemkonfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    active_profile: Option<ProfileId>,
    profiles: BTreeMap<ProfileId, ValidProfile>,
    // Die geprueften Schluessel von `[sentinel]` (leer ohne die Tabelle).
    sentinel: SentinelSettings,
    // Die geprueften Schluessel von `[probe_fs]` (leer ohne die Tabelle).
    probe_fs: ProbeFsSettings,
    digest: ConfigDigest,
}

impl Config {
    /// Parst und validiert eine TOML-Konfiguration.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        Self::from_bytes(input.as_bytes())
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConfigError> {
        let input = std::str::from_utf8(bytes).map_err(|_| ConfigError::InvalidUtf8)?;
        let raw: RawConfig = toml::from_str(input).map_err(ConfigError::Parse)?;
        Self::validate(raw, ConfigDigest::from_bytes(bytes))
    }

    fn validate(raw: RawConfig, digest: ConfigDigest) -> Result<Self, ConfigError> {
        let RawConfig {
            schema_version,
            mode: ObservationMode::Observe,
            active_profile,
            sentinel: raw_sentinel,
            probe_fs: raw_probe_fs,
            profiles: raw_profiles,
        } = raw;
        if schema_version != 1 {
            return Err(ConfigError::UnsupportedSchemaVersion(schema_version));
        }
        if raw_profiles.is_empty() {
            return Err(ConfigError::EmptyProfiles);
        }

        let mut profiles = BTreeMap::new();
        for (name, profile) in raw_profiles {
            let id = ProfileId::parse(name)?;
            let profile = ValidProfile::validate(&id, profile)?;
            profiles.insert(id, profile);
        }

        let sentinel = raw_sentinel
            .map(SentinelSettings::validate)
            .transpose()?
            .unwrap_or_default();
        let probe_fs = raw_probe_fs
            .map(ProbeFsSettings::validate)
            .transpose()?
            .unwrap_or_default();
        let active_profile = active_profile.map(ProfileId::parse).transpose()?;
        Ok(Self {
            active_profile,
            profiles,
            sentinel,
            probe_fs,
            digest,
        })
    }

    /// Digest der exakt gelesenen Konfigurationsbytes.
    #[must_use]
    pub const fn config_digest(&self) -> ConfigDigest {
        self.digest
    }

    /// Die optionalen Einstellungen von `harw-sentinel` aus `[sentinel]`.
    ///
    /// # Returns
    /// Die geprueften Werte; ohne die Tabelle ist jeder Schluessel `None`
    /// (Vorgabe des Sentinels). Unabhaengig vom aktiven Profil.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    #[must_use]
    pub const fn sentinel(&self) -> &SentinelSettings {
        &self.sentinel
    }

    /// Der optionale Beobachtungsbereich von `harw-probe-fs` aus
    /// `[probe_fs]`.
    ///
    /// # Returns
    /// Die geprueften Werte; ohne die Tabelle ist jeder Schluessel `None`
    /// (Vorgabe der Sonde). Unabhaengig vom aktiven Profil.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_config::Config;
    /// use std::path::PathBuf;
    ///
    /// # fn main() -> Result<(), harw_dod_config::ConfigError> {
    /// let config = Config::from_toml(
    ///     "schema_version = 1\nmode = \"observe\"\n\n\
    ///      [probe_fs]\nhome_entries = [\".ssh\"]\n\n\
    ///      [profiles.host]\nscope = \"host\"\nsensors = [\"exec\"]\n",
    /// )?;
    /// assert_eq!(
    ///     config.probe_fs().home_entries(),
    ///     Some([PathBuf::from(".ssh")].as_slice())
    /// );
    /// assert_eq!(config.probe_fs().scope_roots(), None);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn probe_fs(&self) -> &ProbeFsSettings {
        &self.probe_fs
    }

    /// Loest genau das explizit gewaehlte Profil in eine unveraenderliche
    /// Laufzeitentscheidung auf.
    ///
    /// Fehlt die Auswahl, wird nie auf ein Hostprofil oder irgendein anderes
    /// Profil zurueckgefallen.
    pub fn resolve_active(&self) -> Result<ResolvedObservationProfile, ConfigError> {
        let active = self
            .active_profile
            .as_ref()
            .ok_or(ConfigError::MissingActiveProfile)?;
        let profile =
            self.profiles
                .get(active)
                .ok_or_else(|| ConfigError::UnknownActiveProfile {
                    profile: active.clone(),
                })?;

        Ok(ResolvedObservationProfile {
            profile_id: active.clone(),
            scope: profile.scope.clone(),
            sensors: profile.sensors.clone(),
            egress_allow_cidrs: profile.egress_allow_cidrs.clone(),
            config_digest: self.digest,
        })
    }
}

/// Ein abgeschlossenes, ausgewaehltes Beobachtungsprofil.
///
/// Es enthaelt keine mutable Konfiguration und keine aktuell geoeffneten
/// cgroup-Deskriptoren. Sentinel und Probe muessen denselben
/// [`Self::config_digest`] protokollieren; eine abweichende Aufloesung darf
/// keine vollstaendige Bereitschaft melden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedObservationProfile {
    profile_id: ProfileId,
    scope: ObservationScope,
    sensors: Vec<Sensor>,
    egress_allow_cidrs: Vec<IpNet>,
    config_digest: ConfigDigest,
}

impl ResolvedObservationProfile {
    /// Kennung des explizit gewaehlten Profils.
    #[must_use]
    pub fn profile_id(&self) -> &ProfileId {
        &self.profile_id
    }

    /// Der begrenzte Beobachtungsbereich.
    #[must_use]
    pub fn scope(&self) -> &ObservationScope {
        &self.scope
    }

    /// Sensoren, die fuer dieses Profil aktiviert werden duerfen.
    #[must_use]
    pub fn sensors(&self) -> &[Sensor] {
        &self.sensors
    }

    /// CIDRs, die als erlaubte ausgehende TCP-Ziele bewertet werden.
    ///
    /// Eine leere Liste bedeutet: jedes vom Profil erfasste Ziel liegt
    /// ausserhalb der erlaubten Menge. Diese Information installiert niemals
    /// Firewallregeln.
    #[must_use]
    pub fn egress_allow_cidrs(&self) -> &[IpNet] {
        &self.egress_allow_cidrs
    }

    /// Inhaltsdigest der gelesenen Konfigurationsbytes.
    #[must_use]
    pub const fn config_digest(&self) -> ConfigDigest {
        self.config_digest
    }
}

/// Optionale Einstellungen von `harw-sentinel` (Tabelle `[sentinel]`,
/// PL-100).
///
/// # Description
/// Jeder Wert ist `None`, wenn der Schluessel fehlt; dann gilt die Vorgabe
/// des Sentinels. Ein Kommandozeilenflag des Sentinels gewinnt immer ueber
/// den Wert hier (die Aufloesung macht der Sentinel, nicht diese Crate).
/// Geprueft beim Parsen:
/// - `probe_peer_users`: hoechstens [`MAX_PROBE_PEER_USERS`] Eintraege, je
///   nicht leer, hoechstens [`MAX_PROBE_PEER_USER_BYTES`] Bytes, ohne `:`,
///   Leer- oder Steuerzeichen, ohne Duplikat. Eine leere Liste ist gueltig
///   (dann darf nur uid `0` Ereignisse einreichen).
/// - `finding_cooldown_secs`: hoechstens [`MAX_FINDING_COOLDOWN_SECS`];
///   `0` meldet jede Wiederholung.
/// - `container_sensor`: einer der drei Werte von [`ContainerSensorMode`]
///   (ein anderer Wert ist ein Parsefehler).
///
/// # Concurrency
/// Unveraenderlicher Werttyp, `Send + Sync`.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentinelSettings {
    // `[sentinel] probe_peer_users`, wie konfiguriert (Reihenfolge erhalten).
    probe_peer_users: Option<Vec<String>>,
    // `[sentinel] finding_cooldown_secs`.
    finding_cooldown_secs: Option<u64>,
    // `[sentinel] container_sensor`.
    container_sensor: Option<ContainerSensorMode>,
}

/// Schalter des Container-Sensors von `harw-sentinel` (`[sentinel]
/// container_sensor`, PL-100 WP1.8).
///
/// # Description
/// - `Auto` (Vorgabe des Sentinels): der Sensor laeuft fuer jede bekannte
///   Anordnung, auch ohne beim Start erkannte Laufzeit (daemonloses Podman,
///   Start vor dem ersten Container). Auf einer cgroup-v2-Wurzel ist ein
///   fehlendes cgroup-Verzeichnis eine leere Messung, keine
///   Beeintraechtigung; ohne cgroup-v2-Wurzel meldet er sich als
///   beeintraechtigt (`SensorDegraded`).
/// - `On`: wie `Auto`, aber Container gelten als erwartet: fehlt das
///   cgroup-Verzeichnis einer Anordnung, deren Laufzeit beim Start nicht
///   erkannt wurde (kein Laufzeit-Socket, kein laufender Scope), meldet er
///   sich als beeintraechtigt.
/// - `Off`: kein Container-Sensor.
///
/// # Concurrency
/// `Copy + Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_dod_config::{Config, ContainerSensorMode};
///
/// # fn main() -> Result<(), harw_dod_config::ConfigError> {
/// let config = Config::from_toml(
///     "schema_version = 1\nmode = \"observe\"\n\n\
///      [sentinel]\ncontainer_sensor = \"off\"\n\n\
///      [profiles.host]\nscope = \"host\"\nsensors = [\"exec\"]\n",
/// )?;
/// assert_eq!(config.sentinel().container_sensor(), Some(ContainerSensorMode::Off));
/// assert_eq!(ContainerSensorMode::Off.label(), "off");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerSensorMode {
    /// Laeuft fuer jede bekannte Anordnung; ein fehlendes cgroup-Verzeichnis
    /// ist auf einer cgroup-v2-Wurzel eine leere Messung.
    Auto,
    /// Laeuft immer; ohne cgroup-Verzeichnis einer nicht erkannten Laufzeit
    /// beeintraechtigt.
    On,
    /// Laeuft nie.
    Off,
}

impl ContainerSensorMode {
    /// Der Konfigurationswert (`"auto"`, `"on"`, `"off"`), auch fuer das
    /// Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

impl SentinelSettings {
    /// Einstellungen ohne einen einzigen Schluessel (ueberall die Vorgabe des
    /// Sentinels) — dasselbe wie `SentinelSettings::default()`, aber in
    /// `const`-Kontexten nutzbar.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            probe_peer_users: None,
            finding_cooldown_secs: None,
            container_sensor: None,
        }
    }

    /// Der Schalter des Container-Sensors.
    ///
    /// # Returns
    /// `None` ohne den Schluessel (dann gilt die Vorgabe des Sentinels,
    /// [`ContainerSensorMode::Auto`]).
    #[must_use]
    pub const fn container_sensor(&self) -> Option<ContainerSensorMode> {
        self.container_sensor
    }

    /// Die Sonden-Konten der `SO_PEERCRED`-Zulassung des Sentinels
    /// (Kontoname oder numerische uid je Eintrag).
    ///
    /// # Returns
    /// `None` ohne den Schluessel; sonst die Eintraege in
    /// Konfigurationsreihenfolge (auch leer).
    #[must_use]
    pub fn probe_peer_users(&self) -> Option<&[String]> {
        self.probe_peer_users.as_deref()
    }

    /// Die Abklingzeit des Befund-Throttles des Sentinels in Sekunden.
    ///
    /// # Returns
    /// `None` ohne den Schluessel.
    #[must_use]
    pub const fn finding_cooldown_secs(&self) -> Option<u64> {
        self.finding_cooldown_secs
    }

    // Prueft die rohe Tabelle (siehe Typdoku).
    fn validate(raw: RawSentinel) -> Result<Self, ConfigError> {
        let RawSentinel {
            probe_peer_users,
            finding_cooldown_secs,
            container_sensor,
        } = raw;
        if let Some(secs) = finding_cooldown_secs
            && secs > MAX_FINDING_COOLDOWN_SECS
        {
            return Err(ConfigError::FindingCooldownTooLarge { secs });
        }
        if let Some(users) = probe_peer_users.as_deref() {
            if users.len() > MAX_PROBE_PEER_USERS {
                return Err(ConfigError::TooManyProbePeerUsers { count: users.len() });
            }
            for (index, user) in users.iter().enumerate() {
                if !is_valid_probe_peer_user(user) {
                    return Err(ConfigError::InvalidProbePeerUser { user: user.clone() });
                }
                if users
                    .get(..index)
                    .is_some_and(|before| before.contains(user))
                {
                    return Err(ConfigError::DuplicateProbePeerUser { user: user.clone() });
                }
            }
        }
        Ok(Self {
            probe_peer_users,
            finding_cooldown_secs,
            container_sensor,
        })
    }
}

// Ein Eintrag von `probe_peer_users`: nicht leer, begrenzt, ohne den
// `passwd(5)`-Feldtrenner `:`, ohne Leer- und Steuerzeichen (dieselbe
// Grammatik wie `--probe-peer-user`).
fn is_valid_probe_peer_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= MAX_PROBE_PEER_USER_BYTES
        && !user
            .chars()
            .any(|c| c == ':' || c.is_whitespace() || c.is_control())
}

/// Optionaler Beobachtungsbereich von `harw-probe-fs` (Tabelle `[probe_fs]`,
/// PL-100 WP1.6).
///
/// # Description
/// Jeder Wert ist `None`, wenn der Schluessel fehlt; dann gilt die Vorgabe
/// der Sonde. Ein Kommandozeilenflag der Sonde gewinnt (die Aufloesung macht
/// die Sonde, nicht diese Crate). Geprueft beim Parsen, je Liste hoechstens
/// [`MAX_PROBE_FS_PATHS`] Eintraege ohne Duplikat, je Eintrag hoechstens
/// [`MAX_PROBE_FS_PATH_BYTES`] Bytes ohne Steuerzeichen:
/// - `scope_roots`, `home_roots`: absolut, ohne leere, `.`- oder
///   `..`-Komponente, ohne abschliessendes `/`, nie `/` selbst (die
///   Landlock-Leseregel der Sonde umfasste sonst das ganze Dateisystem).
/// - `home_entries`: relativ (ohne fuehrendes `/`), sonst dieselben Regeln;
///   ein Eintrag wie `.ssh` deckt `.ssh` und alles darunter, `.bashrc` nur
///   genau diese Datei (komponentenweise, nie `.bashrc.bak`).
///
/// Eine leere Liste ist gueltig (z. B. keine `home_roots`); ob am Ende
/// ueberhaupt eine Wurzel bleibt, prueft die Sonde beim Start.
///
/// # Concurrency
/// Unveraenderlicher Werttyp, `Send + Sync`.
///
/// # Examples
/// Siehe [`Config::probe_fs`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeFsSettings {
    // `[probe_fs] scope_roots`, Reihenfolge erhalten.
    scope_roots: Option<Vec<PathBuf>>,
    // `[probe_fs] home_roots`, Reihenfolge erhalten.
    home_roots: Option<Vec<PathBuf>>,
    // `[probe_fs] home_entries`, Reihenfolge erhalten.
    home_entries: Option<Vec<PathBuf>>,
}

impl ProbeFsSettings {
    /// Einstellungen ohne einen einzigen Schluessel (ueberall die Vorgabe der
    /// Sonde) — dasselbe wie `ProbeFsSettings::default()`, aber in
    /// `const`-Kontexten nutzbar.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scope_roots: None,
            home_roots: None,
            home_entries: None,
        }
    }

    /// Absolute Verzeichnisse, unter denen jede Schreibaenderung zaehlt.
    ///
    /// # Returns
    /// `None` ohne den Schluessel; sonst die Eintraege in
    /// Konfigurationsreihenfolge (auch leer).
    #[must_use]
    pub fn scope_roots(&self) -> Option<&[PathBuf]> {
        self.scope_roots.as_deref()
    }

    /// Absolute Verzeichnisse mit Home-Verzeichnissen (z. B. `/home`) oder
    /// selbst ein Home-Verzeichnis (z. B. `/root`).
    ///
    /// # Returns
    /// `None` ohne den Schluessel; sonst die Eintraege in
    /// Konfigurationsreihenfolge (auch leer).
    #[must_use]
    pub fn home_roots(&self) -> Option<&[PathBuf]> {
        self.home_roots.as_deref()
    }

    /// Pfade relativ zu einem Home-Verzeichnis, die unter `home_roots`
    /// zaehlen (z. B. `.ssh`, `.config/systemd`, `.bashrc`).
    ///
    /// # Returns
    /// `None` ohne den Schluessel; sonst die Eintraege in
    /// Konfigurationsreihenfolge (auch leer).
    #[must_use]
    pub fn home_entries(&self) -> Option<&[PathBuf]> {
        self.home_entries.as_deref()
    }

    // Prueft die rohe Tabelle (siehe Typdoku).
    fn validate(raw: RawProbeFs) -> Result<Self, ConfigError> {
        let RawProbeFs {
            scope_roots,
            home_roots,
            home_entries,
        } = raw;
        Ok(Self {
            scope_roots: scope_roots
                .map(|paths| validate_probe_fs_paths("scope_roots", paths, PathShape::Absolute))
                .transpose()?,
            home_roots: home_roots
                .map(|paths| validate_probe_fs_paths("home_roots", paths, PathShape::Absolute))
                .transpose()?,
            home_entries: home_entries
                .map(|paths| validate_probe_fs_paths("home_entries", paths, PathShape::Relative))
                .transpose()?,
        })
    }
}

// Ob ein Eintrag von `[probe_fs]` absolut (Wurzel) oder relativ (Eintrag
// unter einem Home-Verzeichnis) sein muss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathShape {
    Absolute,
    Relative,
}

// Prueft eine Liste von `[probe_fs]` (Anzahl, Form je Eintrag, Duplikate).
fn validate_probe_fs_paths(
    key: &'static str,
    raw: Vec<String>,
    shape: PathShape,
) -> Result<Vec<PathBuf>, ConfigError> {
    if raw.len() > MAX_PROBE_FS_PATHS {
        return Err(ConfigError::TooManyProbeFsPaths {
            key,
            count: raw.len(),
        });
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(raw.len());
    for entry in raw {
        if !is_valid_probe_fs_path(&entry, shape) {
            return Err(ConfigError::InvalidProbeFsPath { key, path: entry });
        }
        let path = PathBuf::from(&entry);
        if paths.contains(&path) {
            return Err(ConfigError::DuplicateProbeFsPath { key, path: entry });
        }
        paths.push(path);
    }
    Ok(paths)
}

// Ein Eintrag von `[probe_fs]`: nicht leer, begrenzt, ohne Steuerzeichen,
// nur normale Komponenten (keine leere, `.`, `..`), kein abschliessendes `/`;
// absolut mit mindestens einer Komponente (also nie `/`) bzw. relativ.
fn is_valid_probe_fs_path(raw: &str, shape: PathShape) -> bool {
    if raw.is_empty() || raw.len() > MAX_PROBE_FS_PATH_BYTES || raw.chars().any(char::is_control) {
        return false;
    }
    let rest = match (shape, raw.strip_prefix('/')) {
        (PathShape::Absolute, Some(rest)) => rest,
        (PathShape::Relative, None) => raw,
        (PathShape::Absolute, None) | (PathShape::Relative, Some(_)) => return false,
    };
    !rest.is_empty()
        && rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Ob `raw` als Wurzel von `[probe_fs]` (`scope_roots`, `home_roots`) gilt.
///
/// # Description
/// Dieselbe Grammatik wie beim Parsen (siehe [`ProbeFsSettings`]): absolut,
/// nicht `/`, nur normale Komponenten, kein abschliessendes `/`, ohne
/// Steuerzeichen, hoechstens [`MAX_PROBE_FS_PATH_BYTES`] Bytes. Fuer die
/// Kommandozeile von `harw-probe-fs`, damit Flag und Datei dieselbe Form
/// verlangen.
///
/// # Examples
/// ```rust
/// use harw_dod_config::is_valid_probe_fs_root;
///
/// assert!(is_valid_probe_fs_root("/etc"));
/// assert!(!is_valid_probe_fs_root("/"));
/// assert!(!is_valid_probe_fs_root("etc"));
/// ```
#[must_use]
pub fn is_valid_probe_fs_root(raw: &str) -> bool {
    is_valid_probe_fs_path(raw, PathShape::Absolute)
}

/// Ob `raw` als Eintrag von `[probe_fs] home_entries` gilt.
///
/// # Description
/// Wie [`is_valid_probe_fs_root`], aber relativ (ohne fuehrendes `/`).
///
/// # Examples
/// ```rust
/// use harw_dod_config::is_valid_probe_fs_home_entry;
///
/// assert!(is_valid_probe_fs_home_entry(".config/systemd"));
/// assert!(!is_valid_probe_fs_home_entry("/root/.ssh"));
/// assert!(!is_valid_probe_fs_home_entry("../.ssh"));
/// ```
#[must_use]
pub fn is_valid_probe_fs_home_entry(raw: &str) -> bool {
    is_valid_probe_fs_path(raw, PathShape::Relative)
}

/// Das Scope-Modell eines aufgeloesten Profils.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationScope {
    /// Vollstaendige Hostbeobachtung, nur nach expliziter Auswahl dieses
    /// Profils.
    Host,
    /// Eine Menge benannter cgroup-v2-Unterbaeume.
    Cgroups(CgroupScope),
}

/// Ein cgroup-v2-Bereich mit komponentenweiser Nachfahrensemantik.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupScope {
    paths: Vec<CgroupPath>,
    include_descendants: bool,
}

impl CgroupScope {
    /// Die konfigurierten cgroup-v2-Pfade, relativ zur Mountwurzel.
    #[must_use]
    pub fn paths(&self) -> &[CgroupPath] {
        &self.paths
    }

    /// Ob Nachfahren der konfigurierten cgroups in den Bereich gehoeren.
    #[must_use]
    pub const fn include_descendants(&self) -> bool {
        self.include_descendants
    }
}

/// Ein bereits validierter, kanonischer cgroup-v2-Pfad.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CgroupPath(PathBuf);

impl CgroupPath {
    fn parse(profile: &ProfileId, raw: String) -> Result<Self, ConfigError> {
        if !raw.starts_with('/') || (raw != "/" && raw.ends_with('/')) {
            return Err(ConfigError::InvalidCgroupPath {
                profile: profile.clone(),
                path: raw,
            });
        }
        if raw != "/"
            && raw.split('/').skip(1).any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.bytes().any(|byte| byte == 0 || byte < 0x20)
            })
        {
            return Err(ConfigError::InvalidCgroupPath {
                profile: profile.clone(),
                path: raw,
            });
        }
        Ok(Self(PathBuf::from(raw)))
    }

    fn is_mount_root(&self) -> bool {
        self.0 == Path::new("/")
    }

    /// Der cgroup-Pfad relativ zur cgroup-v2-Mountwurzel.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Prueft komponentenweise, ob `candidate` diese cgroup oder einen ihrer
    /// Nachfahren bezeichnet. Damit umfasst `/a` niemals `/ab`.
    #[must_use]
    pub fn is_same_or_ancestor_of(&self, candidate: &CgroupPath) -> bool {
        candidate.0.starts_with(&self.0)
    }
}

/// Ein geschlossener Satz der v1-eBPF-Sensoren.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sensor {
    /// Erfolgreicher Prozessstart.
    Exec,
    /// Ausgehender TCP-Verbindungsversuch.
    TcpConnect,
}

impl Sensor {
    const fn sort_key(&self) -> u8 {
        match self {
            Self::Exec => 0,
            Self::TcpConnect => 1,
        }
    }
}

/// Stabiler, eingeschraenkter Profilname.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileId(String);

impl ProfileId {
    fn parse(value: String) -> Result<Self, ConfigError> {
        let valid = !value.is_empty()
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric()
                    || byte == b'_'
                    || byte == b'-'
                    || (index > 0 && byte == b'.')
            });
        if !valid {
            return Err(ConfigError::InvalidProfileName { profile: value });
        }
        Ok(Self(value))
    }

    /// Die TOML-Profilkennung.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Ein BLAKE3-Digest der exakt geparsten Konfigurationsbytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigDigest([u8; blake3::OUT_LEN]);

impl ConfigDigest {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Die rohen 32 Digest-Bytes fuer Wire-Protokolle.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; blake3::OUT_LEN] {
        &self.0
    }
}

impl fmt::Display for ConfigDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Fehler beim Lesen, Parsen, Validieren oder Aufloesen der Konfiguration.
#[derive(Debug)]
pub enum ConfigError {
    /// Der feste Systempfad konnte nicht sicher gelesen werden.
    Io(std::io::Error),
    /// Die Datei ist nicht UTF-8-kodiert.
    InvalidUtf8,
    /// Die TOML-Struktur weicht vom geschlossenen Schema ab.
    Parse(toml::de::Error),
    /// Die Datei ist groesser als die zulaessige Obergrenze.
    ConfigTooLarge,
    /// Das Schema ist nicht diese implementierte Fassung.
    UnsupportedSchemaVersion(u32),
    /// Es sind keine Profile deklariert.
    EmptyProfiles,
    /// Die Auswahl fehlt; insbesondere tritt kein Host-Fallback ein.
    MissingActiveProfile,
    /// Die aktive Profilkennung existiert nicht.
    UnknownActiveProfile { profile: ProfileId },
    /// Eine Profilkennung ist nicht Teil der eingeschraenkten Grammatik.
    InvalidProfileName { profile: String },
    /// Ein cgroup-Pfad ist nicht absolut, nicht kanonisch oder enthaelt
    /// Traversierung.
    InvalidCgroupPath { profile: ProfileId, path: String },
    /// Ein cgroup-Profil besitzt keine cgroups.
    EmptyCgroupPaths { profile: ProfileId },
    /// Die cgroup-v2-Mountwurzel umfasst den ganzen Host und darf nicht den
    /// ausdruecklichen Host-Scope umgehen.
    CgroupMountRootRequiresHostScope { profile: ProfileId },
    /// Derselbe cgroup-Pfad ist mehrfach konfiguriert.
    DuplicateCgroupPath {
        profile: ProfileId,
        path: CgroupPath,
    },
    /// Ein cgroup-Profil hat die explizite Nachfahrensemantik ausgelassen.
    MissingIncludeDescendants { profile: ProfileId },
    /// Ein Hostprofil versucht gleichzeitig cgroups zu setzen.
    HostProfileHasCgroups { profile: ProfileId },
    /// Ein Hostprofil versucht gleichzeitig Nachfahrensemantik zu setzen.
    HostProfileHasIncludeDescendants { profile: ProfileId },
    /// Ein Profil aktiviert keinen Sensor.
    EmptySensors { profile: ProfileId },
    /// Ein Sensor wurde innerhalb eines Profils mehrfach genannt.
    DuplicateSensor { profile: ProfileId, sensor: Sensor },
    /// Ein CIDR wurde innerhalb eines Profils mehrfach genannt.
    DuplicateEgressAllowCidr { profile: ProfileId, cidr: IpNet },
    /// Ein geoeffnetes Pfadglied ist nicht root-eigen oder fuer Gruppe/Andere
    /// schreibbar.
    UntrustedSystemPath { path: &'static str },
    /// Ein expliziter `--config`-Pfad oder ein Pfadglied darin ist nicht
    /// root-eigen oder fuer Gruppe/Andere schreibbar.
    UntrustedExplicitPath { path: PathBuf },
    /// Ein geoeffnetes Pfadglied hat nicht den erwarteten Dateityp.
    UnexpectedSystemPathType { path: &'static str },
    /// Ein expliziter `--config`-Pfad oder ein Pfadglied darin hat nicht den
    /// erwarteten Dateityp.
    UnexpectedExplicitPathType { path: PathBuf },
    /// Der explizite Pfad enthält keine ausschließlich sicheren Komponenten.
    InvalidExplicitPath { path: PathBuf },
    /// `[sentinel] probe_peer_users` hat mehr als [`MAX_PROBE_PEER_USERS`]
    /// Eintraege.
    TooManyProbePeerUsers { count: usize },
    /// Ein Eintrag von `[sentinel] probe_peer_users` ist leer, zu lang oder
    /// enthaelt `:`, Leer- oder Steuerzeichen.
    InvalidProbePeerUser { user: String },
    /// Ein Eintrag von `[sentinel] probe_peer_users` kommt mehrfach vor.
    DuplicateProbePeerUser { user: String },
    /// `[sentinel] finding_cooldown_secs` liegt ueber
    /// [`MAX_FINDING_COOLDOWN_SECS`].
    FindingCooldownTooLarge { secs: u64 },
    /// Eine Liste von `[probe_fs]` (`key`) hat mehr als
    /// [`MAX_PROBE_FS_PATHS`] Eintraege.
    TooManyProbeFsPaths { key: &'static str, count: usize },
    /// Ein Eintrag von `[probe_fs]` (`key`) verletzt die Pfadform (siehe
    /// [`ProbeFsSettings`]).
    InvalidProbeFsPath { key: &'static str, path: String },
    /// Ein Eintrag von `[probe_fs]` (`key`) kommt mehrfach vor.
    DuplicateProbeFsPath { key: &'static str, path: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "failed to read DoD configuration: {error}"),
            Self::InvalidUtf8 => formatter.write_str("DoD configuration is not valid UTF-8"),
            Self::Parse(error) => write!(formatter, "invalid DoD configuration TOML: {error}"),
            Self::ConfigTooLarge => formatter.write_str("DoD configuration exceeds the size limit"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported DoD configuration schema version {version}"
                )
            }
            Self::EmptyProfiles => formatter.write_str("DoD configuration defines no profiles"),
            Self::MissingActiveProfile => formatter.write_str(
                "DoD configuration has no active_profile; observation must not be activated",
            ),
            Self::UnknownActiveProfile { profile } => {
                write!(
                    formatter,
                    "active DoD profile `{}` does not exist",
                    profile.as_str()
                )
            }
            Self::InvalidProfileName { profile } => {
                write!(formatter, "invalid DoD profile name `{profile}`")
            }
            Self::InvalidCgroupPath { profile, path } => write!(
                formatter,
                "invalid cgroup path `{path}` in DoD profile `{}`",
                profile.as_str()
            ),
            Self::EmptyCgroupPaths { profile } => write!(
                formatter,
                "cgroup DoD profile `{}` has no cgroup_paths",
                profile.as_str()
            ),
            Self::CgroupMountRootRequiresHostScope { profile } => write!(
                formatter,
                "cgroup DoD profile `{}` selects the cgroup-v2 mount root; use scope = `host` explicitly",
                profile.as_str()
            ),
            Self::DuplicateCgroupPath { profile, path } => write!(
                formatter,
                "cgroup path `{}` is duplicated in DoD profile `{}`",
                path.as_path().display(),
                profile.as_str()
            ),
            Self::MissingIncludeDescendants { profile } => write!(
                formatter,
                "cgroup DoD profile `{}` must set include_descendants explicitly",
                profile.as_str()
            ),
            Self::HostProfileHasCgroups { profile } => write!(
                formatter,
                "host DoD profile `{}` must not define cgroup_paths",
                profile.as_str()
            ),
            Self::HostProfileHasIncludeDescendants { profile } => write!(
                formatter,
                "host DoD profile `{}` must not define include_descendants",
                profile.as_str()
            ),
            Self::EmptySensors { profile } => write!(
                formatter,
                "DoD profile `{}` enables no sensors",
                profile.as_str()
            ),
            Self::DuplicateSensor { profile, sensor } => write!(
                formatter,
                "sensor `{sensor:?}` is duplicated in DoD profile `{}`",
                profile.as_str()
            ),
            Self::DuplicateEgressAllowCidr { profile, cidr } => write!(
                formatter,
                "egress CIDR `{cidr}` is duplicated in DoD profile `{}`",
                profile.as_str()
            ),
            Self::UntrustedSystemPath { path } => {
                write!(
                    formatter,
                    "DoD system configuration path `{path}` is not trusted"
                )
            }
            Self::UntrustedExplicitPath { path } => write!(
                formatter,
                "explicit DoD configuration path `{}` is not trusted (root-owned and not group/world-writable required)",
                path.display()
            ),
            Self::UnexpectedSystemPathType { path } => {
                write!(
                    formatter,
                    "DoD system configuration path `{path}` has an unexpected type"
                )
            }
            Self::UnexpectedExplicitPathType { path } => write!(
                formatter,
                "explicit DoD configuration path `{}` has an unexpected type",
                path.display()
            ),
            Self::InvalidExplicitPath { path } => write!(
                formatter,
                "explicit DoD configuration path `{}` contains an unsafe component",
                path.display()
            ),
            Self::TooManyProbePeerUsers { count } => write!(
                formatter,
                "[sentinel] probe_peer_users has {count} entries, at most {MAX_PROBE_PEER_USERS} are allowed"
            ),
            // `{:?}` escapt Steuerzeichen eines abgelehnten Eintrags.
            Self::InvalidProbePeerUser { user } => write!(
                formatter,
                "[sentinel] probe_peer_users entry {user:?} is empty, longer than \
                 {MAX_PROBE_PEER_USER_BYTES} bytes or contains ':', whitespace or control characters"
            ),
            Self::DuplicateProbePeerUser { user } => write!(
                formatter,
                "[sentinel] probe_peer_users entry {user:?} is duplicated"
            ),
            Self::FindingCooldownTooLarge { secs } => write!(
                formatter,
                "[sentinel] finding_cooldown_secs = {secs} exceeds the maximum of \
                 {MAX_FINDING_COOLDOWN_SECS} seconds"
            ),
            Self::TooManyProbeFsPaths { key, count } => write!(
                formatter,
                "[probe_fs] {key} has {count} entries, at most {MAX_PROBE_FS_PATHS} are allowed"
            ),
            // `{:?}` escapt Steuerzeichen eines abgelehnten Eintrags.
            Self::InvalidProbeFsPath { key, path } => write!(
                formatter,
                "[probe_fs] {key} entry {path:?} is empty, longer than \
                 {MAX_PROBE_FS_PATH_BYTES} bytes, contains control characters or an \
                 empty, '.' or '..' component, ends with '/', is the filesystem root \
                 or has the wrong form (roots are absolute, home entries relative)"
            ),
            Self::DuplicateProbeFsPath { key, path } => {
                write!(formatter, "[probe_fs] {key} entry {path:?} is duplicated")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema_version: u32,
    mode: ObservationMode,
    active_profile: Option<String>,
    // Optionale Tabelle `[sentinel]` (PL-100, additiv, Schema bleibt 1).
    #[serde(default)]
    sentinel: Option<RawSentinel>,
    // Optionale Tabelle `[probe_fs]` (PL-100 WP1.6, additiv, Schema bleibt 1).
    #[serde(default)]
    probe_fs: Option<RawProbeFs>,
    profiles: BTreeMap<String, RawProfile>,
}

// Ungepruefte Tabelle `[sentinel]`; wird ueber `SentinelSettings::validate`
// zu `SentinelSettings`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSentinel {
    // Siehe `SentinelSettings::probe_peer_users`.
    #[serde(default)]
    probe_peer_users: Option<Vec<String>>,
    // Siehe `SentinelSettings::finding_cooldown_secs`.
    #[serde(default)]
    finding_cooldown_secs: Option<u64>,
    // Siehe `SentinelSettings::container_sensor`.
    #[serde(default)]
    container_sensor: Option<ContainerSensorMode>,
}

// Ungepruefte Tabelle `[probe_fs]`; wird ueber `ProbeFsSettings::validate`
// zu `ProbeFsSettings`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProbeFs {
    // Siehe `ProbeFsSettings::scope_roots`.
    #[serde(default)]
    scope_roots: Option<Vec<String>>,
    // Siehe `ProbeFsSettings::home_roots`.
    #[serde(default)]
    home_roots: Option<Vec<String>>,
    // Siehe `ProbeFsSettings::home_entries`.
    #[serde(default)]
    home_entries: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ObservationMode {
    Observe,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    scope: RawScope,
    cgroup_paths: Option<Vec<String>>,
    include_descendants: Option<bool>,
    sensors: Vec<Sensor>,
    #[serde(default)]
    egress_allow_cidrs: Vec<IpNet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawScope {
    Host,
    Cgroups,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidProfile {
    scope: ObservationScope,
    sensors: Vec<Sensor>,
    egress_allow_cidrs: Vec<IpNet>,
}

impl ValidProfile {
    fn validate(profile: &ProfileId, raw: RawProfile) -> Result<Self, ConfigError> {
        let scope = match raw.scope {
            RawScope::Host => {
                if raw.cgroup_paths.is_some() {
                    return Err(ConfigError::HostProfileHasCgroups {
                        profile: profile.clone(),
                    });
                }
                if raw.include_descendants.is_some() {
                    return Err(ConfigError::HostProfileHasIncludeDescendants {
                        profile: profile.clone(),
                    });
                }
                ObservationScope::Host
            }
            RawScope::Cgroups => {
                let cgroup_paths =
                    raw.cgroup_paths
                        .ok_or_else(|| ConfigError::EmptyCgroupPaths {
                            profile: profile.clone(),
                        })?;
                if cgroup_paths.is_empty() {
                    return Err(ConfigError::EmptyCgroupPaths {
                        profile: profile.clone(),
                    });
                }
                let include_descendants = raw.include_descendants.ok_or_else(|| {
                    ConfigError::MissingIncludeDescendants {
                        profile: profile.clone(),
                    }
                })?;
                let mut paths = Vec::with_capacity(cgroup_paths.len());
                for path in cgroup_paths {
                    let path = CgroupPath::parse(profile, path)?;
                    if path.is_mount_root() {
                        return Err(ConfigError::CgroupMountRootRequiresHostScope {
                            profile: profile.clone(),
                        });
                    }
                    if paths.contains(&path) {
                        return Err(ConfigError::DuplicateCgroupPath {
                            profile: profile.clone(),
                            path,
                        });
                    }
                    paths.push(path);
                }
                paths.sort_unstable();
                ObservationScope::Cgroups(CgroupScope {
                    paths,
                    include_descendants,
                })
            }
        };

        if raw.sensors.is_empty() {
            return Err(ConfigError::EmptySensors {
                profile: profile.clone(),
            });
        }
        let mut sensors = Vec::with_capacity(raw.sensors.len());
        for sensor in raw.sensors {
            if sensors.contains(&sensor) {
                return Err(ConfigError::DuplicateSensor {
                    profile: profile.clone(),
                    sensor,
                });
            }
            sensors.push(sensor);
        }
        sensors.sort_unstable_by_key(Sensor::sort_key);

        let mut egress_allow_cidrs = Vec::with_capacity(raw.egress_allow_cidrs.len());
        for cidr in raw.egress_allow_cidrs {
            // `IpNet` accepts host bits in CIDR notation. The observation
            // scope is a set of networks, so normalize first; otherwise
            // `192.0.2.0/24` and `192.0.2.1/24` could bypass duplicate
            // detection while denoting the same allowlist range.
            let cidr = cidr.trunc();
            if egress_allow_cidrs.contains(&cidr) {
                return Err(ConfigError::DuplicateEgressAllowCidr {
                    profile: profile.clone(),
                    cidr,
                });
            }
            egress_allow_cidrs.push(cidr);
        }
        egress_allow_cidrs.sort_unstable();

        Ok(Self {
            scope,
            sensors,
            egress_allow_cidrs,
        })
    }
}

fn open_explicit_config(path: &Path) -> Result<File, ConfigError> {
    let path = path.to_owned();
    if !path.is_absolute() {
        return Err(ConfigError::InvalidExplicitPath { path });
    }
    let trust = PathTrust::Explicit(path.clone());
    let mut components = path.components().peekable();
    let mut parent = open_trusted_directory(CWD, Path::new("/"), trust.clone())?;

    while let Some(component) = components.next() {
        match component {
            Component::RootDir if path.is_absolute() => continue,
            Component::Normal(name) if components.peek().is_some() => {
                parent = open_trusted_directory(parent.as_fd(), Path::new(name), trust.clone())?;
            }
            Component::Normal(name) => {
                return open_trusted_regular_file(parent.as_fd(), Path::new(name), trust);
            }
            // Reject `.` and `..` even where the kernel would resolve them:
            // this keeps the checked descriptor walk identical to the named
            // path and rules out escaping an administrator-selected root.
            Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_)
            | Component::RootDir => {
                return Err(ConfigError::InvalidExplicitPath { path });
            }
        }
    }
    Err(ConfigError::InvalidExplicitPath { path })
}

fn open_trusted_system_config() -> Result<File, ConfigError> {
    let root = open_trusted_directory(CWD, Path::new("/"), PathTrust::System("/"))?;
    let etc = open_trusted_directory(root.as_fd(), Path::new("etc"), PathTrust::System("/etc"))?;
    let config_dir = open_trusted_directory(
        etc.as_fd(),
        Path::new("harw-dod"),
        PathTrust::System("/etc/harw-dod"),
    )?;
    open_trusted_regular_file(
        config_dir.as_fd(),
        Path::new("config.toml"),
        PathTrust::System(SYSTEM_CONFIG_PATH),
    )
}

#[derive(Clone)]
enum PathTrust {
    System(&'static str),
    Explicit(PathBuf),
}

fn open_trusted_directory(
    parent: impl rustix::fd::AsFd,
    name: &Path,
    trust: PathTrust,
) -> Result<OwnedFd, ConfigError> {
    let parent = parent.as_fd();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = retry_on_intr(|| rustix::fs::openat(parent, name, flags, Mode::empty()))
        .map_err(|error| ConfigError::Io(error.into()))?;
    let stat = rustix::fs::fstat(&fd).map_err(|error| ConfigError::Io(error.into()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(unexpected_type(&trust));
    }
    ensure_trusted_mode(stat.st_uid, stat.st_mode, &trust)?;
    Ok(fd)
}

fn open_trusted_regular_file(
    parent: impl rustix::fd::AsFd,
    name: &Path,
    trust: PathTrust,
) -> Result<File, ConfigError> {
    let parent = parent.as_fd();
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let fd = retry_on_intr(|| rustix::fs::openat(parent, name, flags, Mode::empty()))
        .map_err(|error| ConfigError::Io(error.into()))?;
    finish_regular_file(fd, trust)
}

fn finish_regular_file(fd: OwnedFd, trust: PathTrust) -> Result<File, ConfigError> {
    let stat = rustix::fs::fstat(&fd).map_err(|error| ConfigError::Io(error.into()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(unexpected_type(&trust));
    }
    ensure_trusted_mode(stat.st_uid, stat.st_mode, &trust)?;
    let flags = rustix::fs::fcntl_getfl(&fd).map_err(|error| ConfigError::Io(error.into()))?;
    if flags.contains(OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(&fd, flags.difference(OFlags::NONBLOCK))
            .map_err(|error| ConfigError::Io(error.into()))?;
    }
    Ok(File::from(fd))
}

fn ensure_trusted_mode(
    uid: u32,
    mode: rustix::fs::RawMode,
    trust: &PathTrust,
) -> Result<(), ConfigError> {
    if uid != 0 || mode & 0o022 != 0 {
        return Err(match trust {
            PathTrust::System(path) => ConfigError::UntrustedSystemPath { path },
            PathTrust::Explicit(path) => ConfigError::UntrustedExplicitPath { path: path.clone() },
        });
    }
    Ok(())
}

fn unexpected_type(trust: &PathTrust) -> ConfigError {
    match trust {
        PathTrust::System(path) => ConfigError::UnexpectedSystemPathType { path },
        PathTrust::Explicit(path) => ConfigError::UnexpectedExplicitPathType { path: path.clone() },
    }
}

fn read_limited(file: File) -> Result<Vec<u8>, ConfigError> {
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(ConfigError::Io)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::ConfigTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const HOST_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
"#;

    /// Die mitgelieferten Konfigurationsdateien müssen den eigenen, strengen
    /// Parser bestehen (`deny_unknown_fields`) — sonst startet dod mit genau
    /// der Datei nicht, die wir ausliefern (Tippfehler `egotiation_allow_cidrs`).
    /// Die Beispieldatei lässt `active_profile` absichtlich auskommentiert
    /// (keine implizite Profilwahl); geprüft wird sie mit einkommentierter
    /// Auswahl, damit trotzdem jedes Feld gegen den Parser läuft.
    #[test]
    fn shipped_config_files_parse() -> TestResult {
        let example = include_str!("../../../packaging/config.example.toml");
        let unselected = Config::from_toml(example).map_err(ctx("example parses"))?;
        assert!(
            unselected.resolve_active().is_err(),
            "Beispiel darf ohne ausdrückliche Profilwahl nicht aktivierbar sein"
        );
        let selected = example.replace(
            "# active_profile = \"selected-services\"",
            "active_profile = \"selected-services\"",
        );
        for (name, input) in [
            ("harw-dod/config.toml", include_str!("../../../config.toml")),
            ("harw-dod/packaging/config.example.toml", selected.as_str()),
        ] {
            let config = Config::from_toml(input).map_err(ctx(name))?;
            config.resolve_active().map_err(ctx(name))?;
            // Reader-first: ausgeliefert sind `[sentinel]` und `[probe_fs]`
            // auskommentiert ...
            assert_eq!(config.sentinel(), &SentinelSettings::new(), "{name}");
            assert_eq!(config.probe_fs(), &ProbeFsSettings::new(), "{name}");
            // ... und einkommentiert gelten genau die dokumentierten Vorgaben
            // des Sentinels.
            let enabled = uncomment_optional_tables(input);
            let config = Config::from_toml(&enabled).map_err(ctx(name))?;
            assert_eq!(
                config.sentinel().probe_peer_users(),
                Some(["harw-probe-bpf".to_owned(), "harw-probe-fs".to_owned()].as_slice()),
                "{name}"
            );
            assert_eq!(
                config.sentinel().finding_cooldown_secs(),
                Some(600),
                "{name}"
            );
            assert_eq!(
                config.sentinel().container_sensor(),
                Some(ContainerSensorMode::Auto),
                "{name}"
            );
            // Die Vorgaben von `harw-probe-fs` selbst prueft deren
            // `watch`-Modul gegen genau diese Dateien; hier nur, dass jede
            // Liste vorhanden und nicht leer ist.
            for (key, list) in [
                ("scope_roots", config.probe_fs().scope_roots()),
                ("home_roots", config.probe_fs().home_roots()),
                ("home_entries", config.probe_fs().home_entries()),
            ] {
                assert!(list.is_some_and(|list| !list.is_empty()), "{name}: {key}");
            }
        }
        Ok(())
    }

    /// Kommentiert die ausgelieferten, auskommentierten Tabellen
    /// `[sentinel]` und `[probe_fs]` ein (jeder Schluessel steht auf einer
    /// Zeile).
    fn uncomment_optional_tables(input: &str) -> String {
        input
            .replace("\n# [sentinel]\n", "\n[sentinel]\n")
            .replace("\n# probe_peer_users = ", "\nprobe_peer_users = ")
            .replace("\n# finding_cooldown_secs = ", "\nfinding_cooldown_secs = ")
            .replace("\n# container_sensor = ", "\ncontainer_sensor = ")
            .replace("\n# [probe_fs]\n", "\n[probe_fs]\n")
            .replace("\n# scope_roots = ", "\nscope_roots = ")
            .replace("\n# home_roots = ", "\nhome_roots = ")
            .replace("\n# home_entries = ", "\nhome_entries = ")
    }

    /// `HOST_PROFILE` mit einer zusätzlichen Tabelle `[probe_fs]`.
    fn with_probe_fs(table: &str) -> String {
        format!("{HOST_PROFILE}\n[probe_fs]\n{table}\n")
    }

    /// PL-100 WP1.6: `[probe_fs]` ist optional; jeder Schlüssel einzeln,
    /// leere Listen sind gültig, die Reihenfolge bleibt erhalten.
    #[test]
    fn probe_fs_table_is_optional_and_each_key_is_optional() -> TestResult {
        let without = Config::from_toml(HOST_PROFILE).map_err(ctx("no probe_fs table"))?;
        assert_eq!(without.probe_fs(), &ProbeFsSettings::default());
        assert_eq!(without.probe_fs().scope_roots(), None);

        let all = Config::from_toml(&with_probe_fs(
            "scope_roots = [\"/etc\", \"/var/spool/cron\"]\n\
             home_roots = [\"/home\", \"/root\"]\n\
             home_entries = [\".ssh\", \".config/systemd\", \".bashrc\"]",
        ))
        .map_err(ctx("all keys"))?;
        assert_eq!(
            all.probe_fs().scope_roots(),
            Some([PathBuf::from("/etc"), PathBuf::from("/var/spool/cron")].as_slice())
        );
        assert_eq!(
            all.probe_fs().home_roots(),
            Some([PathBuf::from("/home"), PathBuf::from("/root")].as_slice())
        );
        assert_eq!(
            all.probe_fs().home_entries(),
            Some(
                [
                    PathBuf::from(".ssh"),
                    PathBuf::from(".config/systemd"),
                    PathBuf::from(".bashrc")
                ]
                .as_slice()
            )
        );

        let empty = Config::from_toml(&with_probe_fs("home_roots = []"))
            .map_err(ctx("empty home roots"))?;
        assert_eq!(empty.probe_fs().home_roots(), Some([].as_slice()));
        assert_eq!(empty.probe_fs().scope_roots(), None);
        assert_eq!(empty.probe_fs().home_entries(), None);
        Ok(())
    }

    /// PL-100 WP1.6: jede Pfadform, die eine Wurzel weiten oder relativ
    /// machen würde, lehnt schon der Parser ab.
    #[test]
    fn probe_fs_table_rejects_invalid_paths() {
        for bad in [
            "/",
            "//",
            "",
            "etc",
            "/etc/",
            "/etc//cron.d",
            "/etc/./cron.d",
            "/etc/../root",
            "/etc/\\u0007bell",
        ] {
            for key in ["scope_roots", "home_roots"] {
                let input = with_probe_fs(&format!("{key} = [\"{bad}\"]"));
                assert!(
                    matches!(
                        Config::from_toml(&input),
                        Err(ConfigError::InvalidProbeFsPath { key: refused, .. }) if refused == key
                    ),
                    "{key} = {bad:?} must be refused"
                );
            }
        }
        for bad in [
            "",
            "/.ssh",
            ".ssh/",
            "..",
            "./.ssh",
            ".config//systemd",
            ".ssh/../x",
        ] {
            let input = with_probe_fs(&format!("home_entries = [\"{bad}\"]"));
            assert!(
                matches!(
                    Config::from_toml(&input),
                    Err(ConfigError::InvalidProbeFsPath {
                        key: "home_entries",
                        ..
                    })
                ),
                "home_entries = {bad:?} must be refused"
            );
        }
        let too_long = with_probe_fs(&format!(
            "scope_roots = [\"/{}\"]",
            "a".repeat(MAX_PROBE_FS_PATH_BYTES)
        ));
        assert!(matches!(
            Config::from_toml(&too_long),
            Err(ConfigError::InvalidProbeFsPath { .. })
        ));
        assert!(matches!(
            Config::from_toml(&with_probe_fs("home_entries = [\".ssh\", \".ssh\"]")),
            Err(ConfigError::DuplicateProbeFsPath {
                key: "home_entries",
                ..
            })
        ));
        let many: Vec<String> = (0..=MAX_PROBE_FS_PATHS)
            .map(|index| format!("\"/srv/{index}\""))
            .collect();
        assert!(matches!(
            Config::from_toml(&with_probe_fs(&format!(
                "scope_roots = [{}]",
                many.join(", ")
            ))),
            Err(ConfigError::TooManyProbeFsPaths {
                key: "scope_roots",
                ..
            })
        ));
        assert!(matches!(
            Config::from_toml(&with_probe_fs("scope_root = [\"/etc\"]")),
            Err(ConfigError::Parse(_))
        ));
    }

    /// Die Fehlermeldung zitiert einen abgelehnten Pfad nur escapt.
    #[test]
    fn probe_fs_error_display_escapes_the_entry() -> TestResult {
        let error = Config::from_toml(&with_probe_fs(
            "scope_roots = [\"/etc/\\u202eevil\\u001b[2J/\"]",
        ))
        .err()
        .ok_or(crate::test_support::TestError::Missing("a refused entry"))?;
        let text = error.to_string();
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(!text.contains('\u{202e}'), "{text:?}");
        assert!(text.contains("\\u{1b}"), "{text:?}");
        Ok(())
    }

    /// PL-100 WP1.8: `container_sensor` kennt genau drei Werte.
    #[test]
    fn sentinel_container_sensor_accepts_the_three_modes_only() -> TestResult {
        for (raw, mode) in [
            ("auto", ContainerSensorMode::Auto),
            ("on", ContainerSensorMode::On),
            ("off", ContainerSensorMode::Off),
        ] {
            let config =
                Config::from_toml(&with_sentinel(&format!("container_sensor = \"{raw}\"")))
                    .map_err(ctx("container mode"))?;
            assert_eq!(config.sentinel().container_sensor(), Some(mode));
            assert_eq!(mode.label(), raw);
        }
        let without = Config::from_toml(&with_sentinel("finding_cooldown_secs = 1"))
            .map_err(ctx("no container key"))?;
        assert_eq!(without.sentinel().container_sensor(), None);
        for bad in ["\"yes\"", "true", "\"On\""] {
            assert!(
                matches!(
                    Config::from_toml(&with_sentinel(&format!("container_sensor = {bad}"))),
                    Err(ConfigError::Parse(_))
                ),
                "{bad} must be refused"
            );
        }
        Ok(())
    }

    /// `HOST_PROFILE` mit einer zusätzlichen Tabelle `[sentinel]`.
    fn with_sentinel(table: &str) -> String {
        format!("{HOST_PROFILE}\n[sentinel]\n{table}\n")
    }

    /// PL-100: `[sentinel]` ist optional; jeder Schlüssel einzeln.
    #[test]
    fn sentinel_table_is_optional_and_each_key_is_optional() -> TestResult {
        let without = Config::from_toml(HOST_PROFILE).map_err(ctx("no sentinel table"))?;
        assert_eq!(without.sentinel(), &SentinelSettings::default());
        assert_eq!(without.sentinel().probe_peer_users(), None);
        assert_eq!(without.sentinel().finding_cooldown_secs(), None);

        let both = Config::from_toml(&with_sentinel(
            "probe_peer_users = [\"harw-probe-fs\", \"991\"]\nfinding_cooldown_secs = 0",
        ))
        .map_err(ctx("both keys"))?;
        assert_eq!(
            both.sentinel().probe_peer_users(),
            Some(["harw-probe-fs".to_owned(), "991".to_owned()].as_slice())
        );
        assert_eq!(both.sentinel().finding_cooldown_secs(), Some(0));

        let empty_list = Config::from_toml(&with_sentinel("probe_peer_users = []"))
            .map_err(ctx("empty peer list"))?;
        assert_eq!(
            empty_list.sentinel().probe_peer_users(),
            Some([].as_slice())
        );
        assert_eq!(empty_list.sentinel().finding_cooldown_secs(), None);

        let at_maximum = Config::from_toml(&with_sentinel(&format!(
            "finding_cooldown_secs = {MAX_FINDING_COOLDOWN_SECS}"
        )))
        .map_err(ctx("maximal cooldown"))?;
        assert_eq!(
            at_maximum.sentinel().finding_cooldown_secs(),
            Some(MAX_FINDING_COOLDOWN_SECS)
        );
        Ok(())
    }

    /// PL-100: ungültige `[sentinel]`-Werte lehnt schon der Parser ab, nie
    /// erst der Sentinel.
    #[test]
    fn sentinel_table_rejects_invalid_values() {
        let too_large = with_sentinel(&format!(
            "finding_cooldown_secs = {}",
            MAX_FINDING_COOLDOWN_SECS.saturating_add(1)
        ));
        assert!(matches!(
            Config::from_toml(&too_large),
            Err(ConfigError::FindingCooldownTooLarge { .. })
        ));
        assert!(matches!(
            Config::from_toml(&with_sentinel("finding_cooldown_secs = -1")),
            Err(ConfigError::Parse(_))
        ));
        for bad in ["\"\"", "\"a:b\"", "\"two words\"", "\"evil\\n\""] {
            let input = with_sentinel(&format!("probe_peer_users = [{bad}]"));
            assert!(
                matches!(
                    Config::from_toml(&input),
                    Err(ConfigError::InvalidProbePeerUser { .. })
                ),
                "{bad} must be refused"
            );
        }
        let too_long = with_sentinel(&format!(
            "probe_peer_users = [\"{}\"]",
            "a".repeat(MAX_PROBE_PEER_USER_BYTES.saturating_add(1))
        ));
        assert!(matches!(
            Config::from_toml(&too_long),
            Err(ConfigError::InvalidProbePeerUser { .. })
        ));
        assert!(matches!(
            Config::from_toml(&with_sentinel(
                "probe_peer_users = [\"harw-probe-fs\", \"harw-probe-fs\"]"
            )),
            Err(ConfigError::DuplicateProbePeerUser { .. })
        ));
        let many: Vec<String> = (0..=MAX_PROBE_PEER_USERS)
            .map(|index| format!("\"user{index}\""))
            .collect();
        assert!(matches!(
            Config::from_toml(&with_sentinel(&format!(
                "probe_peer_users = [{}]",
                many.join(", ")
            ))),
            Err(ConfigError::TooManyProbePeerUsers { .. })
        ));
        assert!(matches!(
            Config::from_toml(&with_sentinel("finding_cooldown = 600")),
            Err(ConfigError::Parse(_))
        ));
    }

    /// Die Fehlermeldung zitiert einen abgelehnten Eintrag nur escapt.
    #[test]
    fn sentinel_error_display_escapes_the_entry() -> TestResult {
        let error = Config::from_toml(&with_sentinel("probe_peer_users = [\"evil\\u001b[2J\"]"))
            .err()
            .ok_or(crate::test_support::TestError::Missing("a refused entry"))?;
        let text = error.to_string();
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("\\u{1b}"), "{text:?}");
        Ok(())
    }

    #[test]
    fn cgroup_ancestry_is_component_aware() -> TestResult {
        let config = Config::from_toml(
            r#"
schema_version = 1
mode = "observe"
active_profile = "services"

[profiles.services]
scope = "cgroups"
cgroup_paths = ["/a"]
include_descendants = true
sensors = ["exec"]
egress_allow_cidrs = []
"#,
        )
        .map_err(ctx("valid cgroup config"))?;
        let resolved = config.resolve_active().map_err(ctx("active profile"))?;
        let ObservationScope::Cgroups(scope) = resolved.scope() else {
            return Err(crate::test_support::TestError::Unexpected(
                "expected cgroup scope".into(),
            ));
        };
        let root = &scope.paths()[0];
        assert!(root.is_same_or_ancestor_of(&CgroupPath(PathBuf::from("/a/b"))));
        assert!(!root.is_same_or_ancestor_of(&CgroupPath(PathBuf::from("/ab"))));
        Ok(())
    }

    #[test]
    fn digest_changes_with_the_exact_config_bytes() -> TestResult {
        let first = Config::from_toml(HOST_PROFILE).map_err(ctx("valid config"))?;
        let second =
            Config::from_toml(&format!("{HOST_PROFILE}\n")).map_err(ctx("valid config"))?;
        assert_ne!(
            first
                .resolve_active()
                .map_err(ctx("profile"))?
                .config_digest(),
            second
                .resolve_active()
                .map_err(ctx("profile"))?
                .config_digest()
        );
        Ok(())
    }

    #[test]
    fn final_component_open_rejects_a_symlink() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = dir.path().join("target.toml");
        let link = dir.path().join("config.toml");
        fs::write(&target, HOST_PROFILE).map_err(ctx("write target"))?;
        symlink(&target, &link).map_err(ctx("symlink"))?;
        assert!(open_trusted_regular_file(CWD, &link, PathTrust::Explicit(link.clone())).is_err());
        Ok(())
    }

    #[test]
    fn explicit_reader_rejects_relative_paths() -> TestResult {
        assert!(matches!(
            load_config("config.toml"),
            Err(ConfigError::InvalidExplicitPath { .. })
        ));
        Ok(())
    }

    #[test]
    fn trusted_mode_rejects_group_writable_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        fs::write(&path, HOST_PROFILE).map_err(ctx("write config"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).map_err(ctx("chmod"))?;
        assert!(matches!(
            open_explicit_config(&path),
            Err(ConfigError::UntrustedExplicitPath { .. })
        ));
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
