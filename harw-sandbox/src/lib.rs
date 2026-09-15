//! Canonical sandbox contract for work execution.
//!
//! This crate resolves configured workspace aliases into canonical filesystem
//! roots and represents monotonic permission reduction. It deliberately does
//! not execute processes, mount filesystems, or launch MCP servers: those
//! backends must consume a frozen [`SandboxSpec`] at their syscall boundary.
//!
//! Autorität wird in zwei unabhängigen Achsen geführt, die beide nur schrumpfen
//! können: [`PermissionSet`] beschreibt *welche Art* von Operation erlaubt ist,
//! [`NetworkScope`] beschreibt *welche Hosts* eine bereits per
//! [`Permission::NetworkAccess`] freigegebene Netzverbindung erreichen darf.
//! Beide Achsen werden ausschließlich per Schnittmenge verengt; es gibt keine
//! API, die einer Kind-Sandbox etwas zurückgibt, das der Elternteil nicht hat.
//!
//! Der ursprüngliche Ausbauplan sah für ausgehende Verbindungen einen neuen,
//! zu [`NetworkScope`] parallelen Typ `EgressSet` vor. Eine Analyse des
//! bestehenden Codes ergab: [`NetworkScope`] *ist bereits* der geforderte
//! Halbverband — mit `intersection`, `is_subset_of`, punktgrenzen-geschütztem
//! `allows` und Normalisierung in `from_hosts`, und ohne `add` oder
//! Vereinigung. Ein zweiter Typ daneben wäre eine zweite Wahrheit über
//! dieselbe Frage, mit demselben Risiko, auseinanderzulaufen. [`NetworkScope`]
//! wurde deshalb um [`EgressTarget`] (Hostname, DNS-Suffix, CIDR-Bereich)
//! *erweitert*, nicht dupliziert. Wer im Rahmen des Ausbauprogramms nach
//! `EgressSet` sucht: die geforderte Funktionalität lebt hier, unter dem
//! bestehenden Namen.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use harw_types::{TenantId, WorkspaceId};
// `Contains` ist absichtlich nicht importiert: die Methode `contains` ist auf
// `IpNet` inhärent definiert (`pub fn contains<T>(&self, other: T) -> bool
// where Self: Contains<T>`), der Trait selbst wird von `ipnet` nicht aus der
// Kronenebene re-exportiert und wird für den Aufruf auch nicht benötigt.
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod cargo;
pub use cargo::{CargoExecutionMode, CargoProfileError, CargoSandboxProfile};

mod bwrap;
pub use bwrap::{
    BwrapCommandPlan, BwrapLauncher, SANDBOX_PROXY_SOCKET_PATH, SANDBOX_RELAY_PATH, SandboxChild,
};

pub mod egress;
pub use egress::{EgressHost, EgressUrl, EgressUrlError, host_matches_suffix};

mod extra_roots;
pub use extra_roots::{ExtraRoot, ExtraRootError, ExtraRootsCell, MAX_EXTRA_ROOTS, validate_extra_root};

mod process_permit;
pub use process_permit::{
    GrantedProcessPermit, HostApprovalScope, ProcessEnvironment, ProcessPermitError,
    ProcessPermitId, ProcessPermitLedger, ProcessPermitRequest, request_for_workspace,
};

/// Operationsklassen, die eine Sandbox autorisieren kann.
///
/// Es gibt bewusst keine Permission für beliebigen Dateisystemzugriff außerhalb
/// des aufgelösten Workspace. Die einzige Ausnahme ist
/// [`Permission::ReadCargoRegistry`]: sie benennt genau einen fest verdrahteten,
/// nur lesbaren Pfad und keine vom Aufrufer wählbare Position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    ReadWorkspace,
    WriteWorkspace,
    ExecuteProcess,
    /// Erlaubt ausgehende Netzverbindungen. Die zusätzliche Einschränkung auf
    /// einzelne Ziel-Hosts leistet [`NetworkScope`], nicht diese Permission.
    NetworkAccess,
    ReadSecrets,
    ManagePlugins,
    /// Lesezugriff auf die entpackten Abhängigkeitsquellen unter
    /// `$CARGO_HOME/registry/src`.
    ///
    /// # Beschreibung
    /// Gedacht für Dependency-Quellen-Recherche: ein Werkzeug darf den Code
    /// lesen, den der eigene Build ohnehin bereits kompiliert. Die Permission
    /// gewährt ausdrücklich **keinen** Schreibzugriff (auch nicht auf die
    /// Registry selbst), **keinen** Netzzugriff (Herunterladen oder Aktualisieren
    /// von Crates bleibt [`Permission::NetworkAccess`] vorbehalten) und hat
    /// **keinen** Bezug zum Workspace: der Pfad liegt außerhalb des Workspace und
    /// wird deshalb nie über [`WorkspaceBinding`] aufgelöst.
    ///
    /// # Hinweis für Backends
    /// Der Kern hält nur die Autorisierung. Ein Backend muss sie in eine reine
    /// Lesebindung des Registry-Pfads übersetzen; ohne diese Übersetzung bleibt
    /// die Permission wirkungslos (fail-closed).
    ReadCargoRegistry,
}

/// A permission set with no mutation API. It can only be built from a trusted
/// policy decision and monotonically narrowed by intersection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSet {
    granted: BTreeSet<Permission>,
}

impl PermissionSet {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            granted: BTreeSet::new(),
        }
    }

    /// Constructs the result of a trusted policy evaluation. Channel and child
    /// code must subsequently call [`Self::intersection`], never re-grant.
    #[must_use]
    pub fn from_policy<I>(permissions: I) -> Self
    where
        I: IntoIterator<Item = Permission>,
    {
        Self {
            granted: permissions.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, permission: Permission) -> bool {
        self.granted.contains(&permission)
    }

    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            granted: self.granted.intersection(&other.granted).copied().collect(),
        }
    }

    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.granted.is_subset(&parent.granted)
    }

    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.granted.iter().copied()
    }
}

impl Default for PermissionSet {
    fn default() -> Self {
        Self::empty()
    }
}

/// Netzmodus eines Prozess-Sandkastens (W5 N-SBX, F-003/F-120).
///
/// # Description
/// Bestimmt, wie ein Bubblewrap-Backend Netz bereitstellt. Es gibt bewusst
/// **keinen** Modus, der den Host-Netz-Namespace teilt (`--share-net`): jeder
/// Modus erzeugt eine eigene, leere netns (`--unshare-net`).
///
/// - [`NetworkMode::None`] (Default): keine Netzverbindung, nur `lo`.
/// - [`NetworkMode::ProxyOnly`]: in derselben netns läuft `harw-netns-relay`
///   auf `127.0.0.1:<listen_port>` und leitet an den Unix-Socket des
///   Egress-Proxys im Harness weiter; der Kindprozess erhält
///   `ALL_PROXY=socks5h://127.0.0.1:<listen_port>`. Erfordert zusätzlich
///   [`Permission::NetworkAccess`].
///
/// # Concurrency
/// Reiner Wert (`Send + Sync`), keine Seiteneffekte.
///
/// # Examples
/// ```rust
/// use harw_sandbox::NetworkMode;
///
/// assert_eq!(NetworkMode::default(), NetworkMode::None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkMode {
    /// Eigene netns ohne Außenverbindung.
    #[default]
    None,
    /// Eigene netns; Ausgang nur über Relay → Egress-Proxy.
    ProxyOnly(RelaySpec),
}

/// Aufrufkonfiguration des netns-Relays für [`NetworkMode::ProxyOnly`].
///
/// # Description
/// Das Backend bindet `binary` nur lesend an einen festen Pfad in der Sandbox
/// und `proxy_socket` read-write an einen festen Socket-Pfad; Host-Pfade sind
/// in der Sandbox nicht sichtbar. Aufruf in der Sandbox:
/// `<relay> <listen_port> <socket> -- <cmd…>` (Exec-Modus, N-EGRESS).
///
/// # Invariants (geprüft beim Planen)
/// - `binary` und `proxy_socket` sind absolut und enthalten nur normale
///   Komponenten (kein `.`/`..`).
/// - `listen_port` ist nicht `0`.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// use harw_sandbox::{NetworkMode, RelaySpec};
///
/// let mode = NetworkMode::ProxyOnly(RelaySpec {
///     binary: PathBuf::from("/usr/libexec/harw/harw-netns-relay"),
///     listen_port: 1080,
///     proxy_socket: PathBuf::from("/run/user/1000/harw/egress.sock"),
/// });
/// assert_ne!(mode, NetworkMode::None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySpec {
    /// Absoluter Host-Pfad des Relay-Binaries `harw-netns-relay`.
    pub binary: PathBuf,
    /// TCP-Port, auf dem das Relay in der Sandbox an `127.0.0.1` lauscht.
    pub listen_port: u16,
    /// Absoluter Host-Pfad des Unix-Sockets des Egress-Proxys.
    pub proxy_socket: PathBuf,
}

/// Ein Ziel, das ein Sandkasten erreichen darf.
///
/// # Beschreibung
/// Vor dieser Erweiterung bedeutete jeder Eintrag in [`NetworkScope`]
/// implizit dasselbe: ein Suffix-Treffer an einer Punkt-Grenze. `EgressTarget`
/// macht diese Bedeutung explizit und fügt zwei engere Zielarten hinzu.
/// [`Self::DnsSuffix`] trägt exakt die alte Bedeutung weiter —
/// [`NetworkScope::from_hosts`] legt jeden normalisierten Eintrag weiterhin
/// als `DnsSuffix` ab, damit sich an [`NetworkScope::allows`] keine einzige
/// Zeile Verhalten ändert. [`Self::Host`] erlaubt genau einen Namen ohne
/// Subdomains, [`Self::Cidr`] einen Adressbereich für
/// [`NetworkScope::allows_addr`].
///
/// # Vergleich
/// [`Ord`]/[`PartialOrd`] sind abgeleitet, damit der Typ als Element eines
/// `BTreeSet` taugt: zuerst nach Variante in Deklarationsreihenfolge, dann
/// nach Inhalt. Für einen ausschließlich über [`NetworkScope::from_hosts`]
/// gebauten Scope (immer `DnsSuffix`) ist das identisch zur bisherigen
/// lexikografischen String-Ordnung von `allow_hosts: BTreeSet<String>`.
///
/// # Beispiele
/// ```rust
/// use harw_sandbox::EgressTarget;
///
/// let exact = EgressTarget::Host("api.example.com".to_owned());
/// let suffix = EgressTarget::DnsSuffix("example.com".to_owned());
/// assert_ne!(exact, suffix);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EgressTarget {
    /// Genau dieser Hostname, ohne Subdomains. Verglichen wird
    /// ASCII-case-insensitiv und — konsistent zu [`Self::DnsSuffix`] — mit
    /// Toleranz für je *einen* abschließenden Punkt auf beiden Seiten
    /// (`docs.rs.` ≙ `docs.rs`); ein leerer Name trifft nie.
    Host(String),
    /// Dieser Name und alles darunter, an Punktgrenzen — siehe
    /// [`NetworkScope::allows`].
    DnsSuffix(String),
    /// Dieser Adressbereich, geprüft über [`NetworkScope::allows_addr`].
    Cidr(IpNet),
}

// Wire-Form von `EgressTarget`: ein einzelner String, damit `NetworkScope` so
// nah wie möglich am bisherigen `allow_hosts: BTreeSet<String>` bleibt. Ein
// `DnsSuffix` schreibt den Namen unverändert — exakt das Format, das ältere
// Payloads bereits verwenden, und in das ein unpräfigierter, nicht als CIDR
// lesbarer String beim Lesen deshalb zurückfällt (siehe Testfall
// `network_scope_deserializes_from_its_host_list`). `Host` bekommt ein
// führendes `=`, weil ein bloßer Hostname sonst nicht von einem `DnsSuffix` zu
// unterscheiden wäre; `=` ist in einem Hostnamen nie gültig. `Cidr` schreibt
// die kanonische CIDR-Notation (`10.0.0.0/24`), die als Hostname ebenfalls nie
// gültig wäre — keine der drei Formen kann eine andere vortäuschen.
impl Serialize for EgressTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Host(host) => serializer.serialize_str(&format!("={host}")),
            Self::DnsSuffix(suffix) => serializer.serialize_str(suffix),
            Self::Cidr(net) => serializer.collect_str(net),
        }
    }
}

impl<'de> Deserialize<'de> for EgressTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if let Some(host) = raw.strip_prefix('=') {
            return Ok(Self::Host(host.to_owned()));
        }
        if let Ok(net) = raw.parse::<IpNet>() {
            return Ok(Self::Cidr(net));
        }
        Ok(Self::DnsSuffix(raw))
    }
}

/// Granulare Beschränkung der erreichbaren Ziel-Hosts und Adressbereiche.
///
/// # Beschreibung
/// [`Permission::NetworkAccess`] entscheidet, *ob* eine Sandbox überhaupt ins
/// Netz darf; `NetworkScope` entscheidet, *wohin*. Der Scope ist eine
/// Positivliste aus [`EgressTarget`]-Werten: der Standardwert ist leer und
/// erlaubt damit kein einziges Ziel. Wie [`PermissionSet`] kennt der Typ keine
/// Mutations-API — er wird einmal aus einer vertrauenswürdigen Quelle gebaut
/// und danach nur noch per [`Self::intersection`] verengt. Es gibt bewusst
/// kein `add`, kein `union` und kein `widen`: ein Scope kann auf keinem Weg
/// wachsen.
///
/// # Normalisierung
/// [`Self::from_hosts`] normalisiert jeden Eintrag: umgebende Leerzeichen
/// entfernen, ASCII-Kleinschreibung, führende Punkte entfernen (`.docs.rs` und
/// `docs.rs` sind derselbe Eintrag). Leere Einträge werden verworfen, weil ein
/// leerer Host sonst als Suffix jedes Namens wirken könnte. Internationalisierte
/// Namen muss der Aufrufer bereits als Punycode übergeben. Jeder so gebaute
/// Eintrag landet als [`EgressTarget::DnsSuffix`] im Scope — [`Self::allows`]
/// verhält sich dadurch exakt wie vor dieser Erweiterung.
///
/// # Vergleich
/// Ein per `serde`-`Deserialize` eingelesener Scope durchläuft `from_hosts` nicht.
/// [`Self::allows`] vergleicht deshalb case-insensitiv und ignoriert leere
/// Einträge; nicht normalisierte Einträge (z. B. mit führendem Punkt) greifen
/// dann schlicht nicht — der Fehlerfall ist immer die Ablehnung.
///
/// # Nebenläufigkeit
/// Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, beliebig teilbar,
/// keine Sperren.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkScope {
    #[serde(rename = "allow_hosts")]
    allowed: BTreeSet<EgressTarget>,
}

impl NetworkScope {
    /// Liefert den leeren Scope, der jedes Ziel ablehnt.
    ///
    /// # Returns
    /// Ein `NetworkScope` ohne erlaubte Ziele — identisch zu
    /// `NetworkScope::default()`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    /// Baut einen Scope aus einer vertrauenswürdigen Host-Liste.
    ///
    /// # Arguments
    /// - `hosts` (`impl IntoIterator<Item = String>`): Hostnamen ohne Port und
    ///   ohne Userinfo. Jeder Eintrag wird normalisiert (siehe Typ-Doku); leere
    ///   Einträge entfallen. Jeder verbleibende Eintrag wird als
    ///   [`EgressTarget::DnsSuffix`] abgelegt.
    ///
    /// # Returns
    /// Den normalisierten Scope. Duplikate fallen durch die Mengensemantik weg.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::NetworkScope;
    ///
    /// let scope = NetworkScope::from_hosts([" .Docs.RS ".to_owned()]);
    /// assert_eq!(scope.hosts().collect::<Vec<_>>(), vec!["docs.rs"]);
    /// ```
    #[must_use]
    pub fn from_hosts(hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed: hosts
                .into_iter()
                .map(|host| normalize_host(&host))
                .filter(|host| !host.is_empty())
                .map(EgressTarget::DnsSuffix)
                .collect(),
        }
    }

    /// Iteriert die erlaubten Hostnamen und DNS-Suffixe in stabiler, sortierter
    /// Reihenfolge.
    ///
    /// # Returns
    /// Die Namen aus [`EgressTarget::Host`] und [`EgressTarget::DnsSuffix`].
    /// [`EgressTarget::Cidr`]-Einträge sind keine Hostnamen und werden
    /// übersprungen; sie zählen aber weiterhin für [`Self::allows_addr`].
    pub fn hosts(&self) -> impl Iterator<Item = &str> + '_ {
        self.allowed.iter().filter_map(|target| match target {
            EgressTarget::Host(host) => Some(host.as_str()),
            EgressTarget::DnsSuffix(suffix) => Some(suffix.as_str()),
            EgressTarget::Cidr(_) => None,
        })
    }

    /// Gibt an, ob der Scope kein einziges Ziel erlaubt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Monotone Reduktion: Schnittmenge der erlaubten Ziele.
    ///
    /// # Beschreibung
    /// Für jedes Paar aus einem Ziel dieses Scopes und einem Ziel von `other`
    /// wird die paarweise Schnittregel angewendet; alle nicht-leeren
    /// Ergebnisse bilden zusammen den neuen Scope. Die Schnittregel je
    /// Variantenpaar:
    ///
    /// - `Host` ∩ `Host`: gleich → derselbe Host; sonst leer.
    /// - `DnsSuffix` ∩ `DnsSuffix`: gleich → derselbe Suffix; sonst leer. Eine
    ///   Verschachtelung (`example.com` und `api.example.com`) zählt hier
    ///   bewusst *nicht* als Treffer — das ist dieselbe strenge
    ///   Mengenoperation, die diese Methode schon vor `EgressTarget` hatte,
    ///   und bleibt aus Kompatibilitätsgründen erhalten (siehe
    ///   `network_scopes_only_reduce`).
    /// - `Host` ∩ `DnsSuffix` (beide Reihenfolgen): der Host liegt an einer
    ///   Punkt-Grenze innerhalb des Suffixes → der Host (die engere Seite);
    ///   sonst leer. Beispiel: `DnsSuffix("example.com")` ∩
    ///   `Host("api.example.com")` = `Host("api.example.com")`.
    /// - `Cidr` ∩ `Cidr`: identisch → derselbe Bereich; einer enthält den
    ///   anderen vollständig → der engere Bereich; sonst leer. Zwei
    ///   überlappende, aber nicht ineinander enthaltene Bereiche lassen sich
    ///   nicht immer exakt als ein einzelner CIDR-Bereich darstellen — die
    ///   leere Menge ist dann die einzig sichere Antwort, weil eine
    ///   Schnittmenge nie mehr zulassen darf als beide Eingaben zusammen
    ///   erlauben. Mehr zuzulassen wäre der einzige Fehler, der hier wirklich
    ///   weh tut.
    /// - Jede Kombination aus `Host`/`DnsSuffix` und `Cidr`: immer leer.
    ///   Hostnamen und Adressbereiche sind unterschiedliche
    ///   Vergleichsräume und können sich nie überschneiden.
    ///
    /// # Returns
    /// Einen Scope, der höchstens so viele Ziele erlaubt wie jede der beiden
    /// Seiten — siehe [`Self::is_subset_of`].
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        let mut allowed = BTreeSet::new();
        for left in &self.allowed {
            for right in &other.allowed {
                if let Some(narrower) = intersect_targets(left, right) {
                    allowed.insert(narrower);
                }
            }
        }
        Self { allowed }
    }

    /// Prüft, ob dieser Scope keine Ziele über `parent` hinaus erlaubt.
    ///
    /// # Arguments
    /// - `parent` (`&NetworkScope`): der Scope, gegen den begrenzt wird.
    ///
    /// # Returns
    /// `true`, wenn jedes eigene Ziel von mindestens einem Ziel aus `parent`
    /// abgedeckt ist. „Abgedeckt“ folgt derselben Variantenpaar-Regel wie
    /// [`Self::intersection`] (ein `DnsSuffix` ist nur bei exakter
    /// Übereinstimmung von einem anderen `DnsSuffix` abgedeckt, ein `Host`
    /// aber auch von einem `DnsSuffix`, das ihn an einer Punkt-Grenze
    /// enthält; ein `Cidr` von einem `Cidr`, das ihn vollständig enthält).
    /// Dieselbe Regel für beide Methoden ist die Voraussetzung dafür, dass
    /// `self.intersection(other).is_subset_of(self)` immer gilt.
    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.allowed
            .iter()
            .all(|child| parent.allowed.iter().any(|candidate| target_is_subset(child, candidate)))
    }

    /// Entscheidet, ob ein konkreter Host kontaktiert werden darf.
    ///
    /// # Beschreibung
    /// Erlaubt ist ein exakter Treffer oder ein Suffix-Treffer auf einer
    /// freigegebenen Domain, wobei das Suffix an einer Punkt-Grenze beginnen
    /// muss. `docs.rs` erlaubt daher `docs.rs` und `static.docs.rs`, aber weder
    /// `evildocs.rs` (kein Punkt vor dem Suffix) noch `docs.rs.evil.com` (die
    /// Domain steht nicht am Ende). Ohne die Punkt-Grenze könnte jeder Angreifer
    /// eine erlaubte Domain als bloßes Namens-Ende registrieren; ohne die
    /// Verankerung am Ende genügte eine eigene Subdomain mit dem erlaubten Namen
    /// als Präfix. Beide Regeln zusammen bilden exakt die Zugehörigkeit zur
    /// Domain ab. [`EgressTarget::Cidr`]-Einträge nehmen an dieser Prüfung nie
    /// teil — dafür ist [`Self::allows_addr`] zuständig.
    ///
    /// # Arguments
    /// - `host` (`&str`): reiner Hostname. Port und Userinfo müssen vom Aufrufer
    ///   bereits abgetrennt sein, sonst greift kein Eintrag.
    ///
    /// # Returns
    /// `true`, wenn der Host erlaubt ist. Ein leerer Scope, ein leerer Host und
    /// jeder nicht auswertbare Vergleich liefern `false`.
    ///
    /// # Panics
    /// Keine: es wird ausschließlich über prüfende Slice-Zugriffe verglichen.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::NetworkScope;
    ///
    /// let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
    /// assert!(scope.allows("DOCS.rs"));
    /// assert!(scope.allows("static.docs.rs"));
    /// assert!(!scope.allows("evildocs.rs"));
    /// assert!(!scope.allows("docs.rs.evil.com"));
    /// ```
    #[must_use]
    pub fn allows(&self, host: &str) -> bool {
        let needle = normalize_host(host);
        if needle.is_empty() {
            return false;
        }
        self.allowed
            .iter()
            .any(|target| target.matches_normalized_host(&needle))
    }

    /// Entscheidet, ob eine konkrete Adresse kontaktiert werden darf.
    ///
    /// # Beschreibung
    /// Prüft `addr` ausschließlich gegen die [`EgressTarget::Cidr`]-Einträge
    /// des Scopes, über `IpNet::contains`. `Host`- und `DnsSuffix`-Einträge
    /// nehmen nie teil: ein Hostname ist keine Adresse, und eine Auflösung
    /// (DNS) liegt außerhalb dessen, was dieser Typ modelliert.
    ///
    /// # Arguments
    /// - `addr` (`std::net::IpAddr`): die zu prüfende Adresse, IPv4 oder IPv6.
    ///
    /// # Returns
    /// `true`, wenn mindestens ein `Cidr`-Eintrag `addr` enthält. Ein leerer
    /// oder rein hostbasierter Scope liefert immer `false`.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::NetworkScope;
    ///
    /// let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
    /// assert!(!scope.allows_addr("127.0.0.1".parse().unwrap()));
    /// ```
    #[must_use]
    pub fn allows_addr(&self, addr: std::net::IpAddr) -> bool {
        self.allowed.iter().any(|target| target.matches_addr(addr))
    }

    /// Die erlaubten Ziele dieses Scopes, verlustfrei.
    ///
    /// # Beschreibung
    /// [`Self::hosts`] klappt `Host` und `DnsSuffix` zu nackten Namen zusammen
    /// und lässt `Cidr` ganz weg — das genügt für Diagnoseausgaben, aber nicht
    /// für einen Aufrufer, der aus dem Scope etwas **ableiten** muss.
    ///
    /// Diese Methode existiert, weil zwei unabhängige Konsumenten sonst über
    /// die Serde-Form round-trippen müssten, um an die Ziele zu kommen. Eine
    /// Wire-Darstellung als Zugriffsweg zu benutzen koppelt sie an ein Format,
    /// das für Bestandsdateien stabil bleiben muss — nicht für Programmierer.
    ///
    /// Der Rückgabewert ist geliehen und unveränderlich: ein Scope kann auch
    /// über diesen Weg nicht wachsen.
    ///
    /// # Returns
    /// Ein Iterator über die Ziele, in stabiler Ordnung.
    pub fn targets(&self) -> impl Iterator<Item = &EgressTarget> + '_ {
        self.allowed.iter()
    }

    /// Baut einen Scope aus fertigen Zielen.
    ///
    /// # Beschreibung
    /// [`Self::from_hosts`] erzeugt ausschließlich [`EgressTarget::DnsSuffix`]
    /// und ist der Weg für Bestandsaufrufer. Wer einen exakten Host oder einen
    /// Adressbereich ausdrücken will, braucht diesen Konstruktor — vorher gab
    /// es dafür von außen nur `Deserialize`, also wieder das Wire-Format als
    /// Umweg.
    ///
    /// **Das ist kein `add`.** Der Scope entsteht einmal aus einer fertigen
    /// Menge; es gibt weiterhin keinen Weg, ihn nachträglich zu erweitern.
    ///
    /// # Arguments
    /// - `targets`: die erlaubten Ziele. Namen werden **nicht** erneut
    ///   normalisiert — wer `Host`/`DnsSuffix` selbst baut, hält sich an die
    ///   in [`Self::from_hosts`] beschriebene Form.
    ///
    /// # Returns
    /// Ein `NetworkScope` mit genau diesen Zielen.
    #[must_use]
    pub fn from_targets(targets: impl IntoIterator<Item = EgressTarget>) -> Self {
        Self {
            allowed: targets.into_iter().collect(),
        }
    }
}

impl EgressTarget {
    /// Entscheidet, ob dieses Ziel den genannten Hostnamen zulässt.
    ///
    /// # Beschreibung
    /// Die Punktgrenzen-Regel lebt hier und nur hier. Sie ist die Zusage, an
    /// der ein Fehler wirklich weh tut: ein `DnsSuffix("docs.rs")` darf
    /// `evildocs.rs` **nicht** zulassen — die Zeichenkette endet zwar wie der
    /// Suffix, aber an keiner Punktgrenze.
    ///
    /// Die Regel stand vorher nur in [`NetworkScope::allows`] und wurde
    /// deshalb von einem zweiten Konsumenten nachgebaut. Zwei Kopien einer
    /// Sicherheitsregel driften; hier gibt es eine.
    ///
    /// # Arguments
    /// - `host`: ein Hostname in beliebiger Schreibweise. Wird normalisiert.
    ///
    /// # Returns
    /// `true`, wenn dieses Ziel den Namen zulässt. Ein [`Self::Cidr`] lässt
    /// nie einen Namen zu — eine Auflösung liegt außerhalb dessen, was dieser
    /// Typ modelliert.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::EgressTarget;
    ///
    /// let target = EgressTarget::DnsSuffix("docs.rs".to_owned());
    /// assert!(target.matches_host("api.docs.rs"));
    /// assert!(!target.matches_host("evildocs.rs"));
    /// ```
    #[must_use]
    pub fn matches_host(&self, host: &str) -> bool {
        let needle = normalize_host(host);
        if needle.is_empty() {
            return false;
        }
        self.matches_normalized_host(&needle)
    }

    // Der innere Vergleich auf einem bereits normalisierten Namen. Getrennt,
    // damit `NetworkScope::allows` nicht je Ziel neu normalisiert.
    fn matches_normalized_host(&self, needle: &str) -> bool {
        match self {
            Self::Host(allowed) => host_matches_exact(allowed, needle),
            Self::DnsSuffix(allowed) => host_matches(allowed, needle),
            Self::Cidr(_) => false,
        }
    }

    /// Entscheidet, ob dieses Ziel die genannte Adresse zulässt.
    ///
    /// # Arguments
    /// - `addr`: die zu prüfende Adresse.
    ///
    /// # Returns
    /// `true` nur für ein [`Self::Cidr`], das `addr` enthält. Ein Hostname ist
    /// keine Adresse.
    #[must_use]
    pub fn matches_addr(&self, addr: std::net::IpAddr) -> bool {
        match self {
            Self::Cidr(net) => net.contains(&addr),
            Self::Host(_) | Self::DnsSuffix(_) => false,
        }
    }
}

// Kanonische Vergleichsform eines Hostnamens: siehe `NetworkScope`-Doku.
// ASCII-Kleinschreibung passt zum ASCII-insensitiven Vergleich in `host_matches`.
fn normalize_host(host: &str) -> String {
    host.trim().trim_start_matches('.').to_ascii_lowercase()
}

// Exakter Treffer oder Suffix-Treffer an einer Punkt-Grenze. Die Regel selbst
// lebt genau einmal in `egress::host_matches_suffix` (case-insensitiv, leere
// Einträge treffen nie, je ein abschließender Punkt toleriert); `needle` ist
// hier bereits normalisiert, `allowed` wird dort defensiv behandelt, weil ein
// deserialisierter Scope die Normalisierung von `from_hosts` nicht durchlaufen hat.
fn host_matches(allowed: &str, needle: &str) -> bool {
    egress::host_matches_suffix(allowed, needle)
}

// Exakter Namensvergleich für `EgressTarget::Host`: dieselbe Trailing-Dot- und
// Leer-Behandlung wie `egress::host_matches_suffix`, aber ohne Suffix-Regel.
fn host_matches_exact(allowed: &str, needle: &str) -> bool {
    let allowed = allowed.strip_suffix('.').unwrap_or(allowed);
    let needle = needle.strip_suffix('.').unwrap_or(needle);
    !allowed.is_empty() && needle.eq_ignore_ascii_case(allowed)
}

// Paarweise Schnittregel für `EgressTarget`, siehe `NetworkScope::intersection`
// für die vollständige Tabelle. Liefert `None`, wenn das Paar keine
// gemeinsame, exakt darstellbare Teilmenge hat.
fn intersect_targets(left: &EgressTarget, right: &EgressTarget) -> Option<EgressTarget> {
    match (left, right) {
        (EgressTarget::Host(a), EgressTarget::Host(b)) => {
            (a == b).then(|| EgressTarget::Host(a.clone()))
        }
        (EgressTarget::DnsSuffix(a), EgressTarget::DnsSuffix(b)) => {
            // Bewusst nur Gleichheit, keine Verschachtelung: siehe Doku von
            // `NetworkScope::intersection`.
            (a == b).then(|| EgressTarget::DnsSuffix(a.clone()))
        }
        (EgressTarget::Host(host), EgressTarget::DnsSuffix(suffix))
        | (EgressTarget::DnsSuffix(suffix), EgressTarget::Host(host)) => {
            host_matches(suffix, host).then(|| EgressTarget::Host(host.clone()))
        }
        (EgressTarget::Cidr(a), EgressTarget::Cidr(b)) => intersect_nets(a, b),
        _ => None,
    }
}

// CIDR-Schnitt: identisch oder ineinander enthalten ergibt den engeren
// Bereich; überlappend-aber-nicht-enthalten ergibt `None`, weil sich dieser
// Fall nicht immer exakt als ein einzelner CIDR-Bereich darstellen lässt und
// mehr zuzulassen als beide Eingaben der einzige hier relevante Fehler wäre.
fn intersect_nets(a: &IpNet, b: &IpNet) -> Option<EgressTarget> {
    if a == b {
        return Some(EgressTarget::Cidr(*a));
    }
    if a.contains(b) {
        return Some(EgressTarget::Cidr(*b));
    }
    if b.contains(a) {
        return Some(EgressTarget::Cidr(*a));
    }
    None
}

// Deckungsrelation für `NetworkScope::is_subset_of`: `child` gilt als von
// `parent` abgedeckt, wenn `intersect_targets(child, parent) == Some(child)`
// wäre. Beide Funktionen kodieren absichtlich dieselbe Regel, sonst würde
// `intersection(a, b).is_subset_of(a)` nicht mehr für jedes Paar gelten.
fn target_is_subset(child: &EgressTarget, parent: &EgressTarget) -> bool {
    match (child, parent) {
        (EgressTarget::Host(a), EgressTarget::Host(b)) => a == b,
        (EgressTarget::DnsSuffix(a), EgressTarget::DnsSuffix(b)) => a == b,
        (EgressTarget::Host(host), EgressTarget::DnsSuffix(suffix)) => host_matches(suffix, host),
        (EgressTarget::Cidr(child_net), EgressTarget::Cidr(parent_net)) => {
            child_net == parent_net || parent_net.contains(child_net)
        }
        _ => false,
    }
}

/// Authoritative server-side association of a tenant, configured workspace
/// alias, and canonical root directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    tenant: TenantId,
    workspace: WorkspaceId,
    canonical_root: PathBuf,
}

impl WorkspaceBinding {
    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    /// Resolves an existing workspace-relative target and rejects symlink or
    /// lexical escapes before a tool can operate on it.
    pub fn resolve_existing(&self, relative: &Path) -> SandboxResult<PathBuf> {
        let candidate = self.join_relative(relative)?;
        let canonical = candidate.canonicalize().map_err(|error| SandboxError::Io {
            path: candidate,
            reason: error.to_string(),
        })?;
        self.ensure_contained(canonical)
    }

    /// Resolves a new workspace-relative target. Its existing parent is
    /// canonicalized so a symlinked directory cannot redirect a write.
    pub fn resolve_for_create(&self, relative: &Path) -> SandboxResult<PathBuf> {
        let candidate = self.join_relative(relative)?;
        let parent = candidate
            .parent()
            .ok_or_else(|| SandboxError::InvalidRelativePath {
                path: relative.to_path_buf(),
            })?;
        let canonical_parent = parent.canonicalize().map_err(|error| SandboxError::Io {
            path: parent.to_path_buf(),
            reason: error.to_string(),
        })?;
        let contained_parent = self.ensure_contained(canonical_parent)?;
        let name = candidate
            .file_name()
            .ok_or_else(|| SandboxError::InvalidRelativePath {
                path: relative.to_path_buf(),
            })?;
        Ok(contained_parent.join(name))
    }

    fn join_relative(&self, relative: &Path) -> SandboxResult<PathBuf> {
        if relative.as_os_str().is_empty()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::Prefix(_) | Component::RootDir | Component::ParentDir
                )
            })
        {
            return Err(SandboxError::InvalidRelativePath {
                path: relative.to_path_buf(),
            });
        }
        Ok(self.canonical_root.join(relative))
    }

    fn ensure_contained(&self, canonical: PathBuf) -> SandboxResult<PathBuf> {
        if canonical.starts_with(&self.canonical_root) {
            Ok(canonical)
        } else {
            Err(SandboxError::PathEscapesWorkspace {
                path: canonical,
                workspace: self.workspace.clone(),
            })
        }
    }

    /// Wie [`Self::resolve_existing`], aber zusätzlich gegen registrierte
    /// Extra-Roots geprüft (`/add-workdir`, Slice A8).
    ///
    /// # Beschreibung
    /// [`WorkspaceBinding`] kennt die [`SandboxSpec`] nicht, die die
    /// Extra-Roots trägt; deshalb übergibt der Aufrufer die zugehörige
    /// [`ExtraRootsCell`] ausdrücklich, statt sie über globalen Zustand zu
    /// beziehen (typischerweise `spec.extra_roots()`). Ein absoluter
    /// `input`-Pfad wird direkt kanonisiert — `canonicalize` löst dabei auch
    /// `..`-Komponenten und Symlinks anhand des tatsächlichen Dateisystems
    /// auf, genau wie bei der primären Auflösung — und muss anschließend
    /// unter der primären Wurzel **oder** einer der `extra_roots` liegen. Ein
    /// relativer Pfad verhält sich unverändert wie [`Self::resolve_existing`]
    /// und wird ausschließlich gegen die primäre Wurzel aufgelöst: sonst
    /// könnten zwei Wurzeln um denselben relativen Namen konkurrieren.
    ///
    /// # Arguments
    /// - `input` (`&Path`): absoluter oder workspace-relativer Zielpfad.
    /// - `extra_roots` (`&ExtraRootsCell`): zusätzlich zulässige Wurzeln.
    ///
    /// # Errors
    /// [`SandboxError::Io`], wenn `input` nicht existiert; ansonsten wie
    /// [`Self::resolve_existing`], insbesondere
    /// [`SandboxError::PathEscapesWorkspace`], wenn keine Wurzel passt.
    pub fn resolve_existing_with_extra_roots(
        &self,
        input: &Path,
        extra_roots: &ExtraRootsCell,
    ) -> SandboxResult<PathBuf> {
        if !input.is_absolute() {
            return self.resolve_existing(input);
        }
        let canonical = input.canonicalize().map_err(|error| SandboxError::Io {
            path: input.to_path_buf(),
            reason: error.to_string(),
        })?;
        self.ensure_contained_with_extra(canonical, extra_roots)
    }

    /// Wie [`Self::resolve_for_create`], erweitert um Extra-Roots. Siehe
    /// [`Self::resolve_existing_with_extra_roots`] für die Begründung des
    /// zusätzlichen Parameters und das Verhalten bei relativen Pfaden.
    ///
    /// # Errors
    /// [`SandboxError::Io`], [`SandboxError::InvalidRelativePath`],
    /// [`SandboxError::PathEscapesWorkspace`] — siehe
    /// [`Self::resolve_for_create`] und [`Self::resolve_existing_with_extra_roots`].
    pub fn resolve_for_create_with_extra_roots(
        &self,
        input: &Path,
        extra_roots: &ExtraRootsCell,
    ) -> SandboxResult<PathBuf> {
        if !input.is_absolute() {
            return self.resolve_for_create(input);
        }
        let parent = input
            .parent()
            .ok_or_else(|| SandboxError::InvalidRelativePath {
                path: input.to_path_buf(),
            })?;
        let canonical_parent = parent.canonicalize().map_err(|error| SandboxError::Io {
            path: parent.to_path_buf(),
            reason: error.to_string(),
        })?;
        let contained_parent = self.ensure_contained_with_extra(canonical_parent, extra_roots)?;
        let name = input
            .file_name()
            .ok_or_else(|| SandboxError::InvalidRelativePath {
                path: input.to_path_buf(),
            })?;
        Ok(contained_parent.join(name))
    }

    // Wie `ensure_contained`, zusätzlich gegen jede registrierte Extra-Root
    // geprüft. Passt keine Wurzel, bleibt es fail-closed bei
    // `PathEscapesWorkspace`.
    fn ensure_contained_with_extra(
        &self,
        canonical: PathBuf,
        extra_roots: &ExtraRootsCell,
    ) -> SandboxResult<PathBuf> {
        if canonical.starts_with(&self.canonical_root) || extra_roots.contains_path(&canonical) {
            Ok(canonical)
        } else {
            Err(SandboxError::PathEscapesWorkspace {
                path: canonical,
                workspace: self.workspace.clone(),
            })
        }
    }
}

/// Configuration-derived registration, deliberately separate from the
/// resolved binding. Inbound channel/model payloads must never construct it.
#[derive(Debug, Clone)]
pub struct WorkspaceRegistration {
    pub tenant: TenantId,
    pub workspace: WorkspaceId,
    pub root: PathBuf,
}

/// Registry that resolves only configured `(tenant, workspace alias)` pairs.
#[derive(Debug, Clone)]
pub struct WorkspaceRegistry {
    bindings: BTreeMap<(String, String), WorkspaceBinding>,
}

impl WorkspaceRegistry {
    /// Canonicalizes every root once during startup and rejects missing,
    /// non-directory, duplicate, or harness-escaping registrations.
    pub fn build(
        harness_root: &Path,
        registrations: impl IntoIterator<Item = WorkspaceRegistration>,
    ) -> SandboxResult<Self> {
        let harness_root = harness_root
            .canonicalize()
            .map_err(|error| SandboxError::Io {
                path: harness_root.to_path_buf(),
                reason: error.to_string(),
            })?;
        if !harness_root.is_dir() {
            return Err(SandboxError::NotDirectory { path: harness_root });
        }

        let mut bindings = BTreeMap::new();
        for registration in registrations {
            let candidate = if registration.root.is_absolute() {
                registration.root
            } else {
                harness_root.join(registration.root)
            };
            let canonical_root = candidate.canonicalize().map_err(|error| SandboxError::Io {
                path: candidate,
                reason: error.to_string(),
            })?;
            if !canonical_root.is_dir() {
                return Err(SandboxError::NotDirectory {
                    path: canonical_root,
                });
            }
            if !canonical_root.starts_with(&harness_root) {
                return Err(SandboxError::WorkspaceEscapesHarness {
                    path: canonical_root,
                });
            }

            let key = (
                registration.tenant.as_str().to_owned(),
                registration.workspace.as_str().to_owned(),
            );
            let binding = WorkspaceBinding {
                tenant: registration.tenant,
                workspace: registration.workspace,
                canonical_root,
            };
            if bindings.insert(key.clone(), binding).is_some() {
                return Err(SandboxError::DuplicateWorkspaceBinding {
                    tenant: key.0,
                    workspace: key.1,
                });
            }
        }
        Ok(Self { bindings })
    }

    pub fn resolve(
        &self,
        tenant: &TenantId,
        workspace: &WorkspaceId,
    ) -> SandboxResult<WorkspaceBinding> {
        self.bindings
            .get(&(tenant.as_str().to_owned(), workspace.as_str().to_owned()))
            .cloned()
            .ok_or_else(|| SandboxError::WorkspaceNotBound {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
            })
    }
}

/// Frozen authorization passed to tools and child spawns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    workspace: WorkspaceBinding,
    permissions: PermissionSet,
    /// Ziel-Hosts für [`Permission::NetworkAccess`]. `#[serde(default)]` hält
    /// ältere, feldlose Payloads lesbar; sie ergeben den leeren, alles
    /// ablehnenden Scope.
    #[serde(default)]
    network_scope: NetworkScope,
    /// Zusätzliche Workspace-Wurzeln (`/add-workdir`, Slice A8). Wird nie
    /// (de-)serialisiert: Extra-Roots sind reiner Sitzungszustand, sonst
    /// könnte eine über das Netz übertragene Spezifikation heimlich erweitert
    /// werden. `#[serde(default)]` hält ältere, feldlose Payloads lesbar.
    ///
    /// Jobs und Kindprozesse erhalten grundsätzlich eine **leere** Zelle
    /// (siehe [`Self::restrict`], [`Self::restrict_network`],
    /// [`Self::restrict_with`]) — es findet nur Verengung statt, nie
    /// implizite Erweiterung. Wer Extra-Roots an eine abgeleitete Sandbox
    /// weitergeben will, muss das ausdrücklich über [`Self::with_extra_roots`]
    /// tun.
    #[serde(skip, default)]
    extra_roots: ExtraRootsCell,
}

impl SandboxSpec {
    /// Friert eine bereits getroffene Policy-Entscheidung ein.
    ///
    /// # Arguments
    /// - `workspace` (`WorkspaceBinding`): serverseitig aufgelöste Bindung.
    /// - `permissions` (`PermissionSet`): Ergebnis der Policy-Auswertung.
    ///
    /// # Returns
    /// Die Spezifikation mit leerem [`NetworkScope`] und ohne Extra-Roots.
    /// Netzziele müssen anschließend ausdrücklich über
    /// [`Self::with_network_scope`] gesetzt werden — der Standard erlaubt
    /// keinen Host. Extra-Roots werden entsprechend nur über
    /// [`Self::with_extra_roots`] ergänzt.
    #[must_use]
    pub fn from_resolved(workspace: WorkspaceBinding, permissions: PermissionSet) -> Self {
        Self {
            workspace,
            permissions,
            network_scope: NetworkScope::empty(),
            extra_roots: ExtraRootsCell::new(),
        }
    }

    /// Setzt die erlaubten Ziel-Hosts beim Aufbau der Spezifikation.
    ///
    /// # Beschreibung
    /// Builder-Schritt, der `self` konsumiert. Er ist nur beim erstmaligen
    /// Aufbau aus einer Policy-Entscheidung zu verwenden: er ersetzt den Scope
    /// und verengt ihn nicht. Für die Ableitung einer Kind-Sandbox sind
    /// [`Self::restrict_network`] oder [`Self::restrict_with`] zuständig, die
    /// Autorität nur reduzieren können.
    ///
    /// # Arguments
    /// - `scope` (`NetworkScope`): die neuen Ziel-Hosts.
    #[must_use]
    pub fn with_network_scope(mut self, scope: NetworkScope) -> Self {
        self.network_scope = scope;
        self
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceBinding {
        &self.workspace
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

    /// Liefert die erlaubten Ziel-Hosts dieser Sandbox.
    #[must_use]
    pub fn network_scope(&self) -> &NetworkScope {
        &self.network_scope
    }

    /// Liefert die zusätzlichen Workspace-Wurzeln dieser Sandbox
    /// (`/add-workdir`, Slice A8).
    #[must_use]
    pub fn extra_roots(&self) -> &ExtraRootsCell {
        &self.extra_roots
    }

    /// Setzt die zusätzlichen Workspace-Wurzeln beim Aufbau der
    /// Spezifikation.
    ///
    /// # Beschreibung
    /// Nur für den ausdrücklichen, expliziten Aufbau gedacht — etwa eine
    /// TUI-Sitzung, die ihre eigene, per `/add-workdir` gefüllte
    /// [`ExtraRootsCell`] an ihre Sandbox bindet. Kind-Sandboxen aus
    /// [`Self::restrict`], [`Self::restrict_network`] und
    /// [`Self::restrict_with`] erhalten *nie* automatisch die Extra-Roots
    /// des Elternteils, weil das eine implizite Erweiterung wäre, keine
    /// Verengung. Wer sie an ein Kind weiterreichen will, ruft diese Methode
    /// nach dem `restrict*`-Aufruf erneut ausdrücklich auf.
    ///
    /// # Arguments
    /// - `cell` (`ExtraRootsCell`): die neuen Extra-Roots, ersetzt die
    ///   bisherigen vollständig.
    #[must_use]
    pub fn with_extra_roots(mut self, cell: ExtraRootsCell) -> Self {
        self.extra_roots = cell;
        self
    }

    /// Liefert eine Kind-Sandbox mit identischem Workspace und geschnittenem
    /// Permission-Set. Die Operation kann Autorität nie erweitern.
    ///
    /// # Beschreibung
    /// Der [`NetworkScope`] bleibt unverändert: eine Permission-Obergrenze sagt
    /// nichts über einzelne Hosts aus, und stillschweigend geleerte Host-Listen
    /// wären ebenso irreführend wie stillschweigend übernommene. Wer beide
    /// Achsen verengen will, nutzt [`Self::restrict_with`]. Die Extra-Roots
    /// werden **nicht** übernommen (siehe [`Self::with_extra_roots`]): das
    /// Kind startet mit einer leeren Zelle.
    #[must_use]
    pub fn restrict(&self, ceiling: &PermissionSet) -> Self {
        Self {
            workspace: self.workspace.clone(),
            permissions: self.permissions.intersection(ceiling),
            network_scope: self.network_scope.clone(),
            extra_roots: ExtraRootsCell::new(),
        }
    }

    /// Liefert eine Kind-Sandbox mit unverändertem Permission-Set und
    /// geschnittenen Ziel-Hosts. Die Extra-Roots werden **nicht** übernommen
    /// (siehe [`Self::with_extra_roots`]): das Kind startet mit einer leeren
    /// Zelle.
    ///
    /// # Arguments
    /// - `scope` (`&NetworkScope`): Obergrenze der erlaubten Hosts.
    #[must_use]
    pub fn restrict_network(&self, scope: &NetworkScope) -> Self {
        Self {
            workspace: self.workspace.clone(),
            permissions: self.permissions.clone(),
            network_scope: self.network_scope.intersection(scope),
            extra_roots: ExtraRootsCell::new(),
        }
    }

    /// Verengt beide Autoritätsachsen in einem Schritt. Die Extra-Roots
    /// werden **nicht** übernommen (siehe [`Self::with_extra_roots`]): das
    /// Kind startet mit einer leeren Zelle.
    ///
    /// # Arguments
    /// - `ceiling` (`&PermissionSet`): Obergrenze der Operationsklassen.
    /// - `scope` (`&NetworkScope`): Obergrenze der erlaubten Hosts.
    ///
    /// # Returns
    /// Eine Kind-Sandbox, die [`Self::ensure_child_of`] gegenüber `self` immer
    /// besteht.
    #[must_use]
    pub fn restrict_with(&self, ceiling: &PermissionSet, scope: &NetworkScope) -> Self {
        Self {
            workspace: self.workspace.clone(),
            permissions: self.permissions.intersection(ceiling),
            network_scope: self.network_scope.intersection(scope),
            extra_roots: ExtraRootsCell::new(),
        }
    }

    /// Prüft, dass diese Sandbox eine zulässige Verengung von `parent` ist.
    ///
    /// # Beschreibung
    /// Geprüft werden vier Bedingungen: identischer Workspace, Permission-Set
    /// als Teilmenge, [`NetworkScope`] als Teilmenge und die Extra-Roots als
    /// Teilmenge ([`ExtraRootsCell::is_subset_of`]). Die dritte Bedingung
    /// verhindert, dass ein Kind bei gleichem [`Permission::NetworkAccess`] ein
    /// Ziel erreicht, das dem Elternteil verwehrt ist; die vierte verhindert,
    /// dass ein Kind über eine explizit gesetzte [`ExtraRootsCell`]
    /// (siehe [`Self::with_extra_roots`]) Zugriff auf eine Wurzel bekommt, die
    /// der Elternteil nicht selbst trägt.
    ///
    /// # Errors
    /// - [`SandboxError::ChildSandboxEscalation`]: der Workspace weicht ab, das
    ///   Permission-Set oder der `NetworkScope` enthält etwas nicht Ererbtes,
    ///   oder eine Extra-Root liegt außerhalb aller Extra-Roots des Elternteils.
    pub fn ensure_child_of(&self, parent: &Self) -> SandboxResult<()> {
        if self.workspace != parent.workspace
            || !self.permissions.is_subset_of(&parent.permissions)
            || !self.network_scope.is_subset_of(&parent.network_scope)
            || !self.extra_roots.is_subset_of(&parent.extra_roots)
        {
            return Err(SandboxError::ChildSandboxEscalation);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum SandboxError {
    Io {
        path: PathBuf,
        reason: String,
    },
    NotDirectory {
        path: PathBuf,
    },
    WorkspaceEscapesHarness {
        path: PathBuf,
    },
    DuplicateWorkspaceBinding {
        tenant: String,
        workspace: String,
    },
    WorkspaceNotBound {
        tenant: TenantId,
        workspace: WorkspaceId,
    },
    InvalidRelativePath {
        path: PathBuf,
    },
    PathEscapesWorkspace {
        path: PathBuf,
        workspace: WorkspaceId,
    },
    ChildSandboxEscalation,
    ProcessExecutionDenied,
    MissingSandboxCommand,
    InvalidSandboxWorkspaceDestination {
        path: PathBuf,
    },
    SandboxProcessSpawn {
        executable: PathBuf,
        reason: String,
    },
    /// [`NetworkMode::ProxyOnly`] angefordert, aber die Sandbox trägt
    /// [`Permission::NetworkAccess`] nicht (fail-closed).
    NetworkModeNotGranted,
    /// Ein Cargo-Fetch benötigt den expliziten Proxy-Netzpfad und mindestens ein erlaubtes Ziel.
    CargoFetchNetworkDenied,
    /// [`RelaySpec`] verletzt eine Invariante; `field` benennt das Feld.
    InvalidRelaySpec {
        field: &'static str,
        reason: String,
    },
}

pub type SandboxResult<T> = Result<T, SandboxError>;

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } => write!(f, "sandbox I/O at '{}': {reason}", path.display()),
            Self::NotDirectory { path } => {
                write!(f, "sandbox path is not a directory: '{}'", path.display())
            }
            Self::WorkspaceEscapesHarness { path } => {
                write!(f, "workspace escapes harness root: '{}'", path.display())
            }
            Self::DuplicateWorkspaceBinding { tenant, workspace } => write!(
                f,
                "duplicate workspace binding '{workspace}' for tenant '{tenant}'"
            ),
            Self::WorkspaceNotBound { tenant, workspace } => write!(
                f,
                "workspace '{workspace}' is not bound to tenant '{tenant}'"
            ),
            Self::InvalidRelativePath { path } => {
                write!(f, "invalid workspace-relative path: '{}'", path.display())
            }
            Self::PathEscapesWorkspace { path, workspace } => write!(
                f,
                "path '{}' escapes workspace '{workspace}'",
                path.display()
            ),
            Self::ChildSandboxEscalation => {
                write!(f, "child sandbox is not a subset of its parent")
            }
            Self::ProcessExecutionDenied => {
                write!(f, "sandbox does not permit process execution")
            }
            Self::MissingSandboxCommand => write!(f, "sandbox launch command is empty"),
            Self::InvalidSandboxWorkspaceDestination { path } => write!(
                f,
                "sandbox workspace mount destination must be absolute and normal: '{}'",
                path.display()
            ),
            Self::SandboxProcessSpawn { executable, reason } => write!(
                f,
                "could not launch sandbox executable '{}': {reason}",
                executable.display()
            ),
            Self::NetworkModeNotGranted => write!(
                f,
                "proxy-only network mode requires the network access permission"
            ),
            Self::CargoFetchNetworkDenied => write!(
                f,
                "Cargo fetch requires proxy-only network access and a non-empty network scope"
            ),
            Self::InvalidRelaySpec { field, reason } => {
                write!(f, "invalid egress relay configuration ({field}): {reason}")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use std::marker::PhantomData;

    use serde::de::value::{Error as ValueError, MapDeserializer, SeqDeserializer};

    fn temp_directory(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("harwness-sandbox-{name}-{}", uuid()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn uuid() -> String {
        format!("{}", std::process::id())
    }

    fn registry(root: &Path) -> WorkspaceRegistry {
        fs::create_dir_all(root.join("project/src")).unwrap();
        WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("acme"),
                workspace: WorkspaceId::from_str("project"),
                root: PathBuf::from("project"),
            }],
        )
        .unwrap()
    }

    #[test]
    fn permissions_only_reduce() {
        let policy = PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::NetworkAccess,
        ]);
        let remote = PermissionSet::from_policy([Permission::ReadWorkspace]);
        let effective = policy.intersection(&remote);

        assert!(effective.contains(Permission::ReadWorkspace));
        assert!(!effective.contains(Permission::WriteWorkspace));
        assert!(effective.is_subset_of(&policy));
    }

    #[test]
    fn registry_resolves_only_a_configured_tenant_workspace_pair() {
        let root = temp_directory("resolve");
        let registry = registry(&root);

        assert!(
            registry
                .resolve(
                    &TenantId::from_str("acme"),
                    &WorkspaceId::from_str("project")
                )
                .is_ok()
        );
        assert!(matches!(
            registry.resolve(
                &TenantId::from_str("other"),
                &WorkspaceId::from_str("project")
            ),
            Err(SandboxError::WorkspaceNotBound { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relative_path_validation_rejects_parent_traversal() {
        let root = temp_directory("paths");
        let binding = registry(&root)
            .resolve(
                &TenantId::from_str("acme"),
                &WorkspaceId::from_str("project"),
            )
            .unwrap();

        assert!(matches!(
            binding.resolve_for_create(Path::new("../escape.txt")),
            Err(SandboxError::InvalidRelativePath { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn children_keep_the_workspace_and_cannot_escalate_permissions() {
        let root = temp_directory("children");
        let binding = registry(&root)
            .resolve(
                &TenantId::from_str("acme"),
                &WorkspaceId::from_str("project"),
            )
            .unwrap();
        let parent = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let child = parent.restrict(&PermissionSet::from_policy([Permission::ReadWorkspace]));

        assert!(child.ensure_child_of(&parent).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restricted_children_never_inherit_extra_roots() {
        let root = temp_directory("children-extra-roots");
        let binding = registry(&root)
            .resolve(
                &TenantId::from_str("acme"),
                &WorkspaceId::from_str("project"),
            )
            .unwrap();
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        let extra_roots = ExtraRootsCell::new();
        extra_roots
            .add(&extra_dir, false, binding.canonical_root(), None)
            .unwrap();

        let parent = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        )
        .with_extra_roots(extra_roots);
        let child = parent.restrict(&PermissionSet::from_policy([Permission::ReadWorkspace]));

        assert!(child.extra_roots().snapshot().is_empty());
        assert!(child.ensure_child_of(&parent).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_existing_with_extra_roots_accepts_absolute_path_inside_extra_root() {
        let root = temp_directory("extra-accept");
        let binding = bound_workspace(&root);
        let extra_dir = root.join("extra");
        fs::create_dir_all(extra_dir.join("sub")).unwrap();
        fs::write(extra_dir.join("sub/file.txt"), b"x").unwrap();
        let extra_roots = ExtraRootsCell::new();
        extra_roots
            .add(&extra_dir, false, binding.canonical_root(), None)
            .unwrap();

        let resolved = binding
            .resolve_existing_with_extra_roots(&extra_dir.join("sub/file.txt"), &extra_roots)
            .unwrap();
        assert_eq!(
            resolved,
            extra_dir.canonicalize().unwrap().join("sub/file.txt")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_existing_with_extra_roots_rejects_path_outside_every_root() {
        let root = temp_directory("extra-reject");
        let binding = bound_workspace(&root);
        let outside = temp_directory("extra-reject-outside");
        let extra_roots = ExtraRootsCell::new();

        let result = binding.resolve_existing_with_extra_roots(&outside, &extra_roots);
        assert!(matches!(
            result,
            Err(SandboxError::PathEscapesWorkspace { .. })
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn resolve_existing_with_extra_roots_rejects_parent_dir_escape() {
        let root = temp_directory("extra-escape");
        let binding = bound_workspace(&root);
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        let outside = root.join("sibling");
        fs::create_dir_all(&outside).unwrap();
        let extra_roots = ExtraRootsCell::new();
        extra_roots
            .add(&extra_dir, false, binding.canonical_root(), None)
            .unwrap();

        // `..` verlässt die Extra-Root lexikalisch; `canonicalize` löst das
        // anhand des tatsächlichen Dateisystems auf und muss das Ergebnis
        // trotzdem als Escape erkennen.
        let escape = extra_dir.join("../sibling");
        let result = binding.resolve_existing_with_extra_roots(&escape, &extra_roots);
        assert!(matches!(
            result,
            Err(SandboxError::PathEscapesWorkspace { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_existing_with_extra_roots_still_resolves_relative_paths_against_primary() {
        let root = temp_directory("extra-relative");
        let binding = bound_workspace(&root);
        fs::write(binding.canonical_root().join("src/lib.rs"), b"//").unwrap();
        let extra_roots = ExtraRootsCell::new();

        let resolved = binding
            .resolve_existing_with_extra_roots(Path::new("src/lib.rs"), &extra_roots)
            .unwrap();
        assert_eq!(
            resolved,
            binding.canonical_root().join("src/lib.rs")
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Löst dieselbe Bindung auf wie `registry`, nur ohne Wiederholung im Test.
    fn bound_workspace(root: &Path) -> WorkspaceBinding {
        registry(root)
            .resolve(
                &TenantId::from_str("acme"),
                &WorkspaceId::from_str("project"),
            )
            .unwrap()
    }

    fn hosts(scope: &NetworkScope) -> Vec<&str> {
        scope.hosts().collect()
    }

    // Baut einen Scope direkt aus `EgressTarget`-Werten. Es gibt bewusst
    // keinen öffentlichen Konstruktor dafür (siehe Modul-Doku: kein `add`,
    // kein `union`) — Tests für `Host`/`Cidr`-Ziele greifen deshalb, wie hier,
    // auf das private Feld zu.
    fn scope_of(targets: impl IntoIterator<Item = EgressTarget>) -> NetworkScope {
        NetworkScope {
            allowed: targets.into_iter().collect(),
        }
    }

    fn net(cidr: &str) -> IpNet {
        cidr.parse().expect("test CIDR literal must parse")
    }

    fn addr(ip: &str) -> std::net::IpAddr {
        ip.parse().expect("test IP literal must parse")
    }

    #[test]
    fn network_scope_matches_only_at_a_dot_boundary() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned(), "crates.io".to_owned()]);

        assert!(scope.allows("docs.rs"));
        assert!(scope.allows("static.docs.rs"));
        assert!(scope.allows("a.b.docs.rs"));
        assert!(scope.allows("crates.io"));

        assert!(!scope.allows("evildocs.rs"));
        assert!(!scope.allows("docs.rs.evil.com"));
        assert!(!scope.allows("rs"));
        assert!(!scope.allows("docs.rsx"));
        assert!(!scope.allows(""));
    }

    #[test]
    fn network_scope_normalizes_case_whitespace_and_leading_dots() {
        let scope = NetworkScope::from_hosts([
            " .DOCS.rs ".to_owned(),
            "docs.rs".to_owned(),
            String::new(),
        ]);

        assert_eq!(hosts(&scope), vec!["docs.rs"]);
        assert!(scope.allows("DOCS.RS"));
        assert!(scope.allows("Static.Docs.Rs"));
        assert!(scope.allows(" docs.rs "));
    }

    #[test]
    fn an_empty_network_scope_denies_every_host() {
        let scope = NetworkScope::empty();

        assert!(scope.is_empty());
        assert_eq!(scope, NetworkScope::default());
        assert!(!scope.allows("docs.rs"));
        assert!(!scope.allows("localhost"));
        assert!(!scope.allows("anything.at.all"));
    }

    #[test]
    fn network_scopes_only_reduce() {
        let parent = NetworkScope::from_hosts(["docs.rs".to_owned(), "crates.io".to_owned()]);
        let requested = NetworkScope::from_hosts(["crates.io".to_owned(), "evil.com".to_owned()]);
        let effective = parent.intersection(&requested);

        assert_eq!(hosts(&effective), vec!["crates.io"]);
        assert!(effective.is_subset_of(&parent));
        assert!(effective.is_subset_of(&requested));
        assert!(!requested.is_subset_of(&parent));
        assert!(!effective.allows("evil.com"));
        assert!(!effective.allows("docs.rs"));

        // Ein Suffix zählt für die Mengenoperation nicht als ererbter Host.
        let subdomain = NetworkScope::from_hosts(["static.docs.rs".to_owned()]);
        assert!(!subdomain.is_subset_of(&parent));
        assert!(parent.intersection(&subdomain).is_empty());
    }

    #[test]
    fn a_resolved_sandbox_starts_without_any_allowed_host() {
        let root = temp_directory("netscope");
        let spec = SandboxSpec::from_resolved(
            bound_workspace(&root),
            PermissionSet::from_policy([Permission::NetworkAccess]),
        );

        assert!(spec.network_scope().is_empty());
        assert!(!spec.network_scope().allows("docs.rs"));

        let scoped = spec
            .clone()
            .with_network_scope(NetworkScope::from_hosts(["docs.rs".to_owned()]));
        assert!(scoped.network_scope().allows("static.docs.rs"));
        assert!(spec.network_scope().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn permission_and_host_ceilings_narrow_independently() {
        let root = temp_directory("netrestrict");
        let spec = SandboxSpec::from_resolved(
            bound_workspace(&root),
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::NetworkAccess]),
        )
        .with_network_scope(NetworkScope::from_hosts([
            "docs.rs".to_owned(),
            "crates.io".to_owned(),
        ]));

        // Eine Permission-Obergrenze lässt die Host-Liste unangetastet.
        let fewer_permissions =
            spec.restrict(&PermissionSet::from_policy([Permission::ReadWorkspace]));
        assert!(!fewer_permissions.permissions().contains(Permission::NetworkAccess));
        assert_eq!(fewer_permissions.network_scope(), spec.network_scope());

        // Eine Host-Obergrenze lässt das Permission-Set unangetastet.
        let fewer_hosts = spec.restrict_network(&NetworkScope::from_hosts([
            "docs.rs".to_owned(),
            "evil.com".to_owned(),
        ]));
        assert_eq!(fewer_hosts.permissions(), spec.permissions());
        assert_eq!(hosts(fewer_hosts.network_scope()), vec!["docs.rs"]);
        assert!(!fewer_hosts.network_scope().allows("evil.com"));

        let both = spec.restrict_with(
            &PermissionSet::from_policy([Permission::ReadWorkspace]),
            &NetworkScope::from_hosts(["crates.io".to_owned()]),
        );
        assert!(!both.permissions().contains(Permission::NetworkAccess));
        assert_eq!(hosts(both.network_scope()), vec!["crates.io"]);
        assert!(both.ensure_child_of(&spec).is_ok());
        assert!(fewer_permissions.ensure_child_of(&spec).is_ok());
        assert!(fewer_hosts.ensure_child_of(&spec).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_child_cannot_add_a_host_the_parent_lacks() {
        let root = temp_directory("netchild");
        let parent = SandboxSpec::from_resolved(
            bound_workspace(&root),
            PermissionSet::from_policy([Permission::NetworkAccess]),
        )
        .with_network_scope(NetworkScope::from_hosts(["docs.rs".to_owned()]));

        let widened = parent.clone().with_network_scope(NetworkScope::from_hosts([
            "docs.rs".to_owned(),
            "evil.com".to_owned(),
        ]));
        assert!(matches!(
            widened.ensure_child_of(&parent),
            Err(SandboxError::ChildSandboxEscalation)
        ));

        // Auch eine Subdomain eines ererbten Hosts ist kein ererbter Host.
        let subdomain = parent
            .clone()
            .with_network_scope(NetworkScope::from_hosts(["static.docs.rs".to_owned()]));
        assert!(matches!(
            subdomain.ensure_child_of(&parent),
            Err(SandboxError::ChildSandboxEscalation)
        ));

        let narrowed = parent.restrict_network(&NetworkScope::from_hosts(["docs.rs".to_owned()]));
        assert!(narrowed.ensure_child_of(&parent).is_ok());

        let cleared = parent.restrict_network(&NetworkScope::empty());
        assert!(cleared.network_scope().is_empty());
        assert!(cleared.ensure_child_of(&parent).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reading_cargo_registry_sources_is_its_own_permission() {
        let policy = PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ReadCargoRegistry,
        ]);

        assert!(policy.contains(Permission::ReadCargoRegistry));
        // Registry-Lesezugriff impliziert weder Schreiben noch Netz.
        assert!(!policy.contains(Permission::WriteWorkspace));
        assert!(!policy.contains(Permission::NetworkAccess));

        let ceiling = PermissionSet::from_policy([Permission::ReadWorkspace]);
        let reduced = policy.intersection(&ceiling);
        assert!(!reduced.contains(Permission::ReadCargoRegistry));
        assert!(reduced.is_subset_of(&policy));
    }

    #[test]
    fn a_missing_network_scope_field_defaults_to_deny_all() {
        // Beide Typen müssen serde-fähig bleiben: `SandboxSpec` wird persistiert
        // und über Prozessgrenzen weitergereicht.
        fn assert_serde<T: Serialize + for<'de> Deserialize<'de>>(_type: PhantomData<T>) {}
        assert_serde(PhantomData::<SandboxSpec>);
        assert_serde(PhantomData::<NetworkScope>);

        // `#[serde(default)]` setzt genau diesen Wert ein, wenn ein älteres
        // Payload das Feld nicht führt.
        assert_eq!(NetworkScope::default(), NetworkScope::empty());
        assert!(NetworkScope::default().is_empty());
        assert!(!NetworkScope::default().allows("docs.rs"));
    }

    #[test]
    fn network_scope_deserializes_from_its_host_list() {
        let entries = std::iter::once((
            "allow_hosts",
            SeqDeserializer::new(["docs.rs".to_owned(), "crates.io".to_owned()].into_iter()),
        ));
        let parsed: Result<NetworkScope, ValueError> =
            NetworkScope::deserialize(MapDeserializer::new(entries));

        let scope = parsed.unwrap();
        assert_eq!(hosts(&scope), vec!["crates.io", "docs.rs"]);
        assert!(scope.allows("static.docs.rs"));
        assert!(!scope.allows("evil.com"));
    }

    // Regression: der wichtigste Test dieser Aufgabe. `allows` darf sich für
    // reine Hostnamen durch die Umstellung von `allow_hosts: BTreeSet<String>`
    // auf `allowed: BTreeSet<EgressTarget>` nicht ändern — insbesondere nicht
    // der Punktgrenzen-Schutz, der `evildocs.rs` gegen einen Eintrag
    // `docs.rs` ablehnt.
    #[test]
    fn network_scope_allows_regression_matches_pre_migration_hostname_behavior() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);

        assert!(scope.allows("docs.rs"));
        assert!(scope.allows("DOCS.rs"));
        assert!(scope.allows("static.docs.rs"));
        assert!(scope.allows("a.b.docs.rs"));

        assert!(!scope.allows("evildocs.rs"));
        assert!(!scope.allows("docs.rs.evil.com"));
        assert!(!scope.allows("rs"));
        assert!(!scope.allows("docs.rsx"));
        assert!(!scope.allows(""));
    }

    #[test]
    fn network_scope_allows_addr_matches_an_ipv4_cidr_range() {
        let scope = scope_of([EgressTarget::Cidr(net("10.0.0.0/8"))]);

        assert!(scope.allows_addr(addr("10.1.2.3")));
        assert!(!scope.allows_addr(addr("11.0.0.0")));
        // Ein reiner CIDR-Scope erlaubt keinen Hostnamen.
        assert!(!scope.allows("docs.rs"));
    }

    #[test]
    fn network_scope_allows_addr_matches_an_ipv6_cidr_range() {
        let scope = scope_of([EgressTarget::Cidr(net("2001:db8::/32"))]);

        assert!(scope.allows_addr(addr("2001:db8::1")));
        assert!(!scope.allows_addr(addr("2001:db9::1")));
    }

    #[test]
    fn network_scope_allows_addr_ignores_host_and_suffix_entries() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);

        assert!(!scope.allows_addr(addr("127.0.0.1")));
        assert!(!scope.allows_addr(addr("::1")));
    }

    #[test]
    fn egress_target_intersection_host_pairs() {
        let same = scope_of([EgressTarget::Host("api.example.com".to_owned())]);
        let other = scope_of([EgressTarget::Host("other.example.com".to_owned())]);

        assert_eq!(
            hosts(&same.intersection(&same)),
            vec!["api.example.com"]
        );
        assert!(same.intersection(&other).is_empty());
    }

    #[test]
    fn egress_target_intersection_dns_suffix_pairs_require_exact_match() {
        // Verschachtelung zählt hier bewusst nicht als Treffer, siehe
        // `NetworkScope::intersection`-Doku und die bestehende
        // `network_scopes_only_reduce`-Regression.
        let example = scope_of([EgressTarget::DnsSuffix("example.com".to_owned())]);
        let sub = scope_of([EgressTarget::DnsSuffix("api.example.com".to_owned())]);
        let same_again = scope_of([EgressTarget::DnsSuffix("example.com".to_owned())]);

        assert!(example.intersection(&sub).is_empty());
        assert_eq!(hosts(&example.intersection(&same_again)), vec!["example.com"]);
    }

    #[test]
    fn egress_target_intersection_host_and_dns_suffix_narrows_to_the_host() {
        let suffix = scope_of([EgressTarget::DnsSuffix("example.com".to_owned())]);
        let inside = scope_of([EgressTarget::Host("api.example.com".to_owned())]);
        let outside = scope_of([EgressTarget::Host("api.other.com".to_owned())]);

        let narrowed = suffix.intersection(&inside);
        assert_eq!(hosts(&narrowed), vec!["api.example.com"]);
        // Kommutativ: die Reihenfolge der Operanden ändert das Ergebnis nicht.
        assert_eq!(narrowed, inside.intersection(&suffix));

        assert!(suffix.intersection(&outside).is_empty());
    }

    #[test]
    fn egress_target_intersection_cidr_pairs() {
        let wide = scope_of([EgressTarget::Cidr(net("10.0.0.0/8"))]);
        let narrow = scope_of([EgressTarget::Cidr(net("10.1.0.0/16"))]);
        let identical = scope_of([EgressTarget::Cidr(net("10.0.0.0/8"))]);
        let disjoint = scope_of([EgressTarget::Cidr(net("192.168.0.0/16"))]);
        // Zwei CIDR-Bereiche derselben Adressfamilie sind aus der
        // Bit-Präfix-Struktur heraus immer entweder ineinander enthalten oder
        // vollständig disjunkt, nie nur teilweise überlappend — das folgt
        // direkt daraus, dass jeder Bereich an einer Zweierpotenz-Grenze
        // ausgerichtet ist. Der "sonst leer"-Zweig deckt hier also den
        // disjunkten Fall ab.
        let nested = scope_of([EgressTarget::Cidr(net("10.0.0.0/9"))]);
        let other_ninth = scope_of([EgressTarget::Cidr(net("9.128.0.0/9"))]);

        // `narrow` liegt vollständig in `wide` — der engere Bereich gewinnt.
        assert_eq!(wide.intersection(&narrow), narrow);
        assert_eq!(wide.intersection(&identical), wide);
        assert!(wide.intersection(&disjoint).is_empty());
        // 10.0.0.0/9 liegt vollständig im getesteten /8 — der engere Bereich
        // gewinnt.
        assert_eq!(wide.intersection(&nested), nested);
        // 9.128.0.0/9 liegt außerhalb von 10.0.0.0/9 — disjunkt, also leer.
        assert!(nested.intersection(&other_ninth).is_empty());
    }

    #[test]
    fn egress_target_intersection_cross_domain_pairs_are_always_empty() {
        let host = scope_of([EgressTarget::Host("docs.rs".to_owned())]);
        let suffix = scope_of([EgressTarget::DnsSuffix("docs.rs".to_owned())]);
        let cidr = scope_of([EgressTarget::Cidr(net("127.0.0.0/8"))]);

        assert!(host.intersection(&cidr).is_empty());
        assert!(cidr.intersection(&host).is_empty());
        assert!(suffix.intersection(&cidr).is_empty());
        assert!(cidr.intersection(&suffix).is_empty());
    }

    #[test]
    fn network_scope_is_subset_of_is_reflexive() {
        let mixed = scope_of([
            EgressTarget::Host("api.example.com".to_owned()),
            EgressTarget::DnsSuffix("docs.rs".to_owned()),
            EgressTarget::Cidr(net("10.0.0.0/8")),
        ]);

        assert!(mixed.is_subset_of(&mixed));
        assert!(NetworkScope::empty().is_subset_of(&mixed));
    }

    #[test]
    fn network_scope_intersection_is_commutative_and_idempotent() {
        let a = scope_of([
            EgressTarget::DnsSuffix("example.com".to_owned()),
            EgressTarget::Cidr(net("10.0.0.0/8")),
        ]);
        let b = scope_of([
            EgressTarget::Host("api.example.com".to_owned()),
            EgressTarget::Cidr(net("10.1.0.0/16")),
        ]);

        assert_eq!(a.intersection(&b), b.intersection(&a));
        assert_eq!(a.intersection(&a), a);
        assert_eq!(b.intersection(&b), b);
    }

    // Eigenschaft: das Ergebnis von `intersection` ist immer `is_subset_of`
    // beider Eingaben. Das ist die eigentliche Zusage des Halbverbands — hier
    // über mehrere konstruierte Paare geprüft, nicht an einem Einzelbeispiel.
    #[test]
    fn network_scope_intersection_is_always_a_subset_of_both_inputs() {
        let pairs: Vec<(NetworkScope, NetworkScope)> = vec![
            (
                NetworkScope::from_hosts(["docs.rs".to_owned(), "crates.io".to_owned()]),
                NetworkScope::from_hosts(["crates.io".to_owned(), "evil.com".to_owned()]),
            ),
            (
                NetworkScope::from_hosts(["docs.rs".to_owned()]),
                NetworkScope::from_hosts(["static.docs.rs".to_owned()]),
            ),
            (
                scope_of([EgressTarget::DnsSuffix("example.com".to_owned())]),
                scope_of([EgressTarget::Host("api.example.com".to_owned())]),
            ),
            (
                scope_of([EgressTarget::Cidr(net("10.0.0.0/8"))]),
                scope_of([EgressTarget::Cidr(net("10.1.0.0/16"))]),
            ),
            (
                scope_of([EgressTarget::Cidr(net("10.0.0.0/9"))]),
                scope_of([EgressTarget::Cidr(net("9.128.0.0/9"))]),
            ),
            (
                scope_of([
                    EgressTarget::Host("api.example.com".to_owned()),
                    EgressTarget::Cidr(net("192.168.0.0/16")),
                ]),
                scope_of([
                    EgressTarget::DnsSuffix("example.com".to_owned()),
                    EgressTarget::Cidr(net("192.168.1.0/24")),
                ]),
            ),
            (NetworkScope::empty(), NetworkScope::from_hosts(["docs.rs".to_owned()])),
        ];

        for (left, right) in &pairs {
            let effective = left.intersection(right);
            assert!(
                effective.is_subset_of(left),
                "intersection must stay inside the left-hand scope"
            );
            assert!(
                effective.is_subset_of(right),
                "intersection must stay inside the right-hand scope"
            );
        }
    }

    // W0B-04: `NetworkScope::allows` nutzt die eine Suffix-Regel aus
    // `egress::host_matches_suffix` — Punktgrenze bleibt, ein abschließender
    // Punkt (absoluter DNS-Name) wird toleriert.
    #[test]
    fn network_scope_allows_uses_egress_suffix_rule() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        assert!(scope.allows("docs.rs."));
        assert!(scope.allows("Static.Docs.RS."));
        assert!(!scope.allows("notdocs.rs"));
        assert!(!scope.allows("docs.rs.evil.com"));
        assert!(!scope.allows("evil.com"));

        let url = EgressUrl::parse("https://evil.com\\@docs.rs/").unwrap();
        assert!(!scope.allows(&url.host_str()));
    }

    // Z0-F4 / R2-08: `EgressTarget::Host` toleriert wie `DnsSuffix` genau einen
    // abschließenden Punkt, bleibt aber exakt (keine Subdomains).
    #[test]
    fn egress_target_host_tolerates_one_trailing_dot() {
        let plain = EgressTarget::Host("docs.rs".to_owned());
        let absolute = EgressTarget::Host("Docs.RS.".to_owned());
        for target in [&plain, &absolute] {
            assert!(target.matches_host("docs.rs"), "{target:?}");
            assert!(target.matches_host("DOCS.rs."), "{target:?}");
            assert!(!target.matches_host("docs.rs.."), "{target:?}");
            assert!(!target.matches_host("static.docs.rs"), "{target:?}");
            assert!(!target.matches_host("notdocs.rs"), "{target:?}");
        }
        for empty in ["", "."] {
            let target = EgressTarget::Host(empty.to_owned());
            assert!(!target.matches_host("."), "{empty:?}");
            assert!(!target.matches_host("docs.rs"), "{empty:?}");
        }

        // Der Schnitt `Host("docs.rs.") ∩ DnsSuffix("docs.rs")` ist
        // `Host("docs.rs.")` — und dieses Ergebnis lässt `docs.rs` jetzt auch zu.
        let host = scope_of([EgressTarget::Host("docs.rs.".to_owned())]);
        let suffix = scope_of([EgressTarget::DnsSuffix("docs.rs".to_owned())]);
        let effective = host.intersection(&suffix);
        assert_eq!(effective, host);
        assert!(effective.allows("docs.rs"));
        assert!(effective.allows("docs.rs."));
        assert!(!effective.allows("api.docs.rs"));
        assert!(effective.is_subset_of(&host));
        assert!(effective.is_subset_of(&suffix));
    }
}
