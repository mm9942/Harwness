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

        let active_profile = active_profile.map(ProfileId::parse).transpose()?;
        Ok(Self {
            active_profile,
            profiles,
            digest,
        })
    }

    /// Digest der exakt gelesenen Konfigurationsbytes.
    #[must_use]
    pub const fn config_digest(&self) -> ConfigDigest {
        self.digest
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
    profiles: BTreeMap<String, RawProfile>,
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

    #[test]
    fn cgroup_ancestry_is_component_aware() {
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
        .expect("valid cgroup config");
        let resolved = config.resolve_active().expect("active profile");
        let ObservationScope::Cgroups(scope) = resolved.scope() else {
            panic!("expected cgroup scope");
        };
        let root = &scope.paths()[0];
        assert!(root.is_same_or_ancestor_of(&CgroupPath(PathBuf::from("/a/b"))));
        assert!(!root.is_same_or_ancestor_of(&CgroupPath(PathBuf::from("/ab"))));
    }

    #[test]
    fn digest_changes_with_the_exact_config_bytes() {
        let first = Config::from_toml(HOST_PROFILE).expect("valid config");
        let second = Config::from_toml(&format!("{HOST_PROFILE}\n")).expect("valid config");
        assert_ne!(
            first.resolve_active().expect("profile").config_digest(),
            second.resolve_active().expect("profile").config_digest()
        );
    }

    #[test]
    fn final_component_open_rejects_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.toml");
        let link = dir.path().join("config.toml");
        fs::write(&target, HOST_PROFILE).expect("write target");
        symlink(&target, &link).expect("symlink");
        assert!(open_trusted_regular_file(CWD, &link, PathTrust::Explicit(link.clone())).is_err());
    }

    #[test]
    fn explicit_reader_rejects_relative_paths() {
        assert!(matches!(
            load_config("config.toml"),
            Err(ConfigError::InvalidExplicitPath { .. })
        ));
    }

    #[test]
    fn trusted_mode_rejects_group_writable_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, HOST_PROFILE).expect("write config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).expect("chmod");
        assert!(matches!(
            open_explicit_config(&path),
            Err(ConfigError::UntrustedExplicitPath { .. })
        ));
    }
}
