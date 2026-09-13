//! Gate 2: Privilegienbudget je Binary.
//!
//! # Verantwortungsbereich
//! Bildet für jedes der vier Binaries die **vollständige Abhängigkeitshülle**
//! und prüft sie gegen die deklarierte Berechtigungsklasse. Ein
//! unprivilegiertes Binary darf nichts erreichen, das eine erhöhte Fähigkeit
//! braucht.
//!
//! Die vier: `harw-sentinel` (unprivilegiert), `harw-probe-fs`
//! (`CAP_SYS_ADMIN`), `harw-probe-bpf` (`CAP_BPF`), `harw-warden`
//! (systemd-Socket, kein Netz).
//!
//! # Warum die *Hülle* und nicht die direkten Abhängigkeiten
//! Eine Fähigkeit wandert über transitive Kanten. Ein Sensor, der nur
//! `harw-dod-readfs` nennt, aber über drei Ecken `harw-dod-bpf` erreicht,
//! zieht dessen Ladefähigkeit ins Binary — und die Rechtematrix behauptet
//! weiterhin die kleine Klasse.
//!
//! # Was dieses Gate nicht prüft
//! Ob das Binary die Fähigkeit zur Laufzeit **tatsächlich** anfordert. Es
//! prüft, was es anfordern *könnte*.
//!
//! # Woher die Fähigkeitszuordnung kommt
//! `harw-dod-cap::Capability` ist reine **Laufzeitinformation**: ein Sensor
//! bindet seine Fähigkeit über `SensorHandle::new(id, Capability::...)` in
//! einer `impl`-Methode (z. B. `ThermalSensor::CAPABILITY`,
//! `AuditBackend::capability()`, `harw_dod_bpf::REQUIRED_CAPABILITY`). Aus
//! `Cargo.toml` allein ist das nicht ablesbar — zwei Crates mit identischer
//! Abhängigkeitsliste können unterschiedliche Fähigkeiten binden.
//!
//! Vor dieser Tabelle wurden zwei Alternativen geprüft und verworfen:
//! - **`[package.metadata]` in den Sensor-`Cargo.toml`s**: keine einzige
//!   Sensor-Crate im Workspace trägt einen solchen Abschnitt (geprüft per
//!   Volltextsuche über alle `Cargo.toml`-Dateien). Es gibt nichts
//!   Bestehendes, das sich lesen ließe, ohne selbst erst eingeführt zu
//!   werden — und eine neu eingeführte Metadaten-Konvention wäre exakt so
//!   pflegeintensiv wie die Tabelle unten, nur an vierzehn Stellen verstreut
//!   statt an einer.
//! - **Ein greifbares Quelltextmuster** (z. B. eine Marker-Konstante mit
//!   festem Namen wie `CAPABILITY`): existiert bei elf der vierzehn Sensoren
//!   (`const CAPABILITY: Capability = ...`), aber nicht einheitlich — die
//!   BPF-Sensoren exportieren `REQUIRED_CAPABILITY` auf Modulebene, die
//!   `authlog`-Crate wählt die Fähigkeit erst zur Laufzeit zwischen zwei
//!   Backends. Ein Parser, der Quelltext nach einem Konstantennamen
//!   durchsucht, wäre kein Cargo-Metadaten-Ersatz mehr, sondern ein
//!   Mini-Compiler für Rust-Quelltext — mehr Fläche für stille Fehler als
//!   die Tabelle, die er ersetzen sollte.
//!
//! Es bleibt also bei einer **handgepflegten Tabelle**
//! ([`CRATE_PRIVILEGE`]) — ausdrücklich benannt, nicht versteckt: das ist
//! genau die Machtstruktur, gegen die `gate_writescopes.rs` mit gutem Grund
//! antritt ("ein unvollständiges Prüfinstrument ist schlechter als keins,
//! weil es Sicherheit suggeriert"). Der Unterschied, der diese Tabelle
//! rechtfertigt, ist die nächste Sektion.
//!
//! # Warum eine unbekannte Crate ein Verstoß ist
//! Die Schreibbereichstabelle in `gate_writescopes.rs` scheiterte, weil eine
//! **fehlende Zeile als "kein Problem" galt** — sechzehn Lücken blieben
//! unbemerkt, weil Schweigen dort Zustimmung bedeutete. Diese Tabelle macht
//! das Gegenteil zur Regel: [`lookup_privilege`] liefert `None` für jede
//! nicht eingetragene Crate, und [`evaluate`] behandelt `None` **immer** als
//! Verstoß, nie als Freigabe. Eine neue Sensor-Crate, die in der Hülle eines
//! der vier Binaries auftaucht, ohne hier eingetragen zu sein, macht das
//! Gate rot — nicht grün. Die Tabelle darf unvollständig sein; sie darf nur
//! nicht **schweigend** unvollständig sein.
//!
//! # Geltungsbereich der Hülle: nur interne, produktive Kanten
//! Die Hülle folgt ausschließlich `CrateNode::deps` — den normalen,
//! workspace-internen `[dependencies]`. Drei bewusste Ausschlüsse:
//!
//! - **`dev_deps` zählt nicht.** `harw-macros` hängt in `[dev-dependencies]`
//!   von `harw-tools`, `harw-observe`, `harw-dod-cap` (und weiteren) ab —
//!   eine dokumentierte zyklische Ausnahme für Positivtests/`trybuild`
//!   (siehe `harw-macros/Cargo.toml`). Eine Dev-Abhängigkeit landet nicht im
//!   ausgelieferten Binary; sie in die Hülle zu ziehen, röte dieses Gate aus
//!   einem Grund, der keiner ist, und noch schlimmer: über `harw-macros`
//!   hinge praktisch jedes Binary zyklisch an praktisch jeder anderen Crate.
//! - **`build_deps` zählt nicht**, aus demselben Grund: ein Build-Skript
//!   läuft zur Bauzeit des Hosts, nicht im ausgelieferten Binary, und trägt
//!   dort keine Fähigkeit.
//! - **`external_deps` zählt nicht.** Das ist die Frage, die
//!   `gate_edges.rs`s `PURE_CRATES`-Prüfung für `harw-lens-rank` stellt
//!   ("erreicht die Hülle eine I/O-fähige *externe* Bibliothek wie `jiff`
//!   oder `tokio`?") — eine berechtigte, aber **andere** Frage als diese
//!   hier. Dieses Gate fragt: "deklariert eine *Sensor-Crate dieses
//!   Workspace* eine erhöhte `Capability`?", und `Capability` ist
//!   ausschließlich internes Vokabular. Externe Crates wie `rustix`,
//!   `landlock` oder `clap` nehmen an diesem Vokabular nicht teil; sie hier
//!   pauschal als "unbekannt = Verstoß" zu behandeln, würde jedes der vier
//!   Binaries allein durch seine Laufzeitbibliotheken rot färben, ohne dass
//!   das etwas über die *Sensor*-Fähigkeit aussagt, um die es hier geht. Für
//!   die Lieferketten-Frage ("ist eine externe Crate an sich riskant?")
//!   trägt dieser Workspace bereits `deny.toml`.
//!
//! # Proc-Macros
//! `harw-macros` (`[lib] proc-macro = true`) taucht in der Hülle von
//! `harw-sentinel` und `harw-probe-fs` auf ([`CrateNode`] unterscheidet
//! Proc-Macro-Crates nicht von gewöhnlichen Bibliotheks-Crates — dieses
//! Merkmal wird von `harw-code-graph` nicht erfasst). Sie ist in
//! [`CRATE_PRIVILEGE`] dennoch geführt, und zwar als [`RequiredPrivilege::Unprivileged`]:
//! ein Proc-Macro läuft während der Kompilierung des *abhängigen* Crates,
//! nicht im ausgelieferten Binary, und bindet dort keinen `SensorHandle` und
//! keine `Capability` — es erzeugt zur Bauzeit Quelltext, der das täte. Sie
//! aus der Hülle herauszunehmen wäre eleganter, ist aber mit den Feldern von
//! [`CrateNode`] nicht unterscheidbar von einer echten Laufzeitabhängigkeit;
//! sie stattdessen explizit als unprivilegiert einzutragen erreicht dieselbe
//! Wirkung, ohne eine neue Fähigkeit des Graphen vorauszusetzen.
//!
//! # Fehlendes Binary
//! Zwei der vier Crates (`harw-probe-bpf`, `harw-warden`) sind heute reine
//! Gerüste: ihre `[dependencies]`-Tabelle ist vollständig leer (siehe
//! `harw-probe-bpf/Cargo.toml`, `harw-warden/Cargo.toml`) — kein interner und
//! kein externer Eintrag. [`missing_binaries`] erkennt das über genau dieses
//! Signal (leer *und* leer, nicht bloß "kein interner Dep"), statt über eine
//! zweite, gepflegte Liste "welche Binaries gibt es schon" — eine zweite
//! Liste wäre ihrerseits eine Quelle für stille Lücken. Ein so erkanntes
//! fehlendes Binary macht das Gate **nicht rot**: [`evaluate`] überspringt es
//! vollständig (keine Prüfung, kein Verstoß) und `checked` zählt nur die
//! tatsächlich geprüften Binaries — bei zwei Gerüsten und zwei gebauten
//! Binaries steht dort `2`, nie `4`. Sichtbar bleibt der Rest trotzdem:
//! [`run`] gibt für jedes fehlende Binary vor dem Gate-Ergebnis eine eigene
//! Zeile aus (`GateReport` selbst hat kein Feld für Randbemerkungen dieser
//! Art; siehe „Meldung an den Verteiler" unten für die Alternative, die
//! *nicht* umgesetzt wurde).
//!
//! # Warum keine neue Abhängigkeit auf `harw-dod-cap`
//! Dieses Gate bräuchte inhaltlich `harw_dod_cap::CapabilityClass` — aber
//! `xtask` hängt bislang ausschließlich von `harw-code-graph` ab, exakt
//! deshalb, weil beide Gates ohne Subprozess auskommen sollen (siehe
//! `xtask/Cargo.toml`-Kommentar). `harw-dod-cap` ist eine Laufzeit-Crate für
//! Sensoren, kein Analysewerkzeug; sie als Compile-Abhängigkeit eines
//! CI-Gates einzuziehen, nur um ein Vier-Varianten-Enum wiederzuverwenden,
//! wäre eine neue Abhängigkeit für eine Konstante. Stattdessen spiegelt
//! dieses Modul die Klassifikation lokal als [`RequiredPrivilege`] —
//! namentlich und in der Bedeutung deckungsgleich mit
//! `harw_dod_cap::CapabilityClass`, aber ohne den Cargo-Kantenzug. Diese
//! Datei fügt **keine neue Abhängigkeit** zu `xtask/Cargo.toml` hinzu.
//!
//! # Meldung an den Verteiler (nicht umgesetzt)
//! Sauberer wäre ein `notes: Vec<String>`-Feld auf [`crate::gates::GateReport`]
//! für genau diese Art von „geprüft, aber nicht bewertet"-Information, statt
//! des `println!` in [`run`]. Das ist eine Änderung an `gates.rs`, das laut
//! Auftrag nicht in diesem Knoten geändert werden soll — hiermit gemeldet,
//! nicht vorgenommen.
//!
//! # Zählen, auch wenn nichts da ist
//! Solange die vier Binaries leer sind, ist das Gate grün — aber es gibt die
//! Zahl der geprüften Binaries aus. Ein Gate, das schweigend nichts tut, ist
//! von einem grünen nicht zu unterscheiden.
//!
//! # Stand
//! Knoten **AW0-10b**: gelandet. [`evaluate`] prüft gegen konstruierte
//! Graphen (siehe `tests`); [`run`] liest den echten Workspace über
//! `harw-code-graph`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use harw_code_graph::{CrateNode, WorkspaceGraph};

use super::GateReport;

/// Lokaler Spiegel von `harw_dod_cap::CapabilityClass` — siehe Moduldoku,
/// Abschnitt „Warum keine neue Abhängigkeit auf `harw-dod-cap`".
///
/// # Description
/// Die Fähigkeitsklasse, die eine einzelne Crate in ihrer Hülle mitbringen
/// kann. Deckungsgleich in Bedeutung und Variantenzahl mit
/// `harw_dod_cap::CapabilityClass`, aber ohne Compile-Abhängigkeit auf jene
/// Crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredPrivilege {
    /// Reines Lesen aus `/proc` oder `/sys`, oder eine reine Vokabular-/
    /// Zugriffsschicht ohne eigene Kernel-Fähigkeit.
    Unprivileged,
    /// Erfordert einen Netlink-Socket (z. B. `AUDIT`).
    Netlink,
    /// Erfordert Dateisystem-Beobachtung (fanotify/inotify).
    FileWatch,
    /// Erfordert das Laden eines BPF-Programms in den Kernel.
    Bpf,
}

impl RequiredPrivilege {
    /// Menschlich lesbare Kennung dieser Fähigkeitsklasse für Verstoßmeldungen.
    ///
    /// # Description
    /// Bewusst an reale Linux-Capability-/Deployment-Namen angelehnt statt an
    /// die internen Rust-Bezeichner: `Netlink` erscheint als
    /// `CAP_AUDIT_READ` (die Fähigkeit, die einen lesenden `AUDIT`-Netlink-
    /// Socket öffnet), `FileWatch` als `CAP_SYS_ADMIN` (deckungsgleich mit
    /// der von `harw-probe-fs` deklarierten Klasse) und `Bpf` als `CAP_BPF`
    /// (deckungsgleich mit `harw-probe-bpf`). Wer eine Verstoßmeldung im CI
    /// liest, muss ohne Rust-Kenntnis verstehen, welche reale Fähigkeit
    /// gemeint ist.
    ///
    /// # Returns
    /// Einen statischen, nicht-leeren Bezeichner.
    ///
    /// # Errors
    /// Keine — totale, panikfreie Funktion.
    ///
    /// # Examples
    /// ```text
    /// RequiredPrivilege::Bpf.label() == "CAP_BPF"
    /// ```
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Unprivileged => "unprivilegiert",
            Self::Netlink => "CAP_AUDIT_READ",
            Self::FileWatch => "CAP_SYS_ADMIN",
            Self::Bpf => "CAP_BPF",
        }
    }
}

/// Die deklarierte Berechtigungsklasse eines der vier überwachten Binaries —
/// die Spalte „Klasse" aus dem Auftrag dieses Knotens.
///
/// # Description
/// Bestimmt, welche [`RequiredPrivilege`]-Werte ein Binary in seiner Hülle
/// tragen darf ([`Self::allows`]). Anders als [`RequiredPrivilege`] ist dies
/// kein Spiegel eines fremden Typs, sondern originär: kein Sensor deklariert
/// je "ich bin CAP_SYS_ADMIN", das ist eine Eigenschaft des *Binaries*, nicht
/// einer einzelnen Fähigkeit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryBudget {
    /// `harw-sentinel`: keine einzige erhöhte Fähigkeit.
    Unprivileged,
    /// `harw-probe-fs`: `CAP_SYS_ADMIN`, die einzige Fähigkeit, die
    /// `fanotify_mark` auf beliebigen Zielen erlaubt.
    CapSysAdmin,
    /// `harw-probe-bpf`: `CAP_BPF` allein.
    CapBpf,
    /// `harw-warden`: authentifiziert über einen systemd-Socket
    /// (`SO_PEERCRED`), kein Netz, keine der vierzehn Sensor-Fähigkeiten.
    SystemdSocketNoNet,
}

impl BinaryBudget {
    /// Menschlich lesbare Kennung dieser Budgetklasse für Verstoßmeldungen.
    ///
    /// # Description
    /// Wörtlich die Klassenbezeichnung aus dem Auftrag dieses Knotens, damit
    /// eine Verstoßmeldung ohne Nachschlagen in dieser Datei verständlich
    /// ist.
    ///
    /// # Returns
    /// Einen statischen, nicht-leeren Bezeichner.
    ///
    /// # Errors
    /// Keine — totale, panikfreie Funktion.
    ///
    /// # Examples
    /// ```text
    /// BinaryBudget::SystemdSocketNoNet.label() == "systemd-Socket, kein Netz"
    /// ```
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Unprivileged => "unprivilegiert",
            Self::CapSysAdmin => "CAP_SYS_ADMIN",
            Self::CapBpf => "CAP_BPF",
            Self::SystemdSocketNoNet => "systemd-Socket, kein Netz",
        }
    }

    /// Ob eine Crate mit der Fähigkeit `required` in diesem Budget zulässig ist.
    ///
    /// # Description
    /// Jedes Budget erlaubt [`RequiredPrivilege::Unprivileged`] uneingeschränkt.
    /// `CapSysAdmin` erlaubt zusätzlich [`RequiredPrivilege::FileWatch`] (die
    /// Fähigkeit, für die `harw-probe-fs` existiert); `CapBpf` zusätzlich
    /// [`RequiredPrivilege::Bpf`]. `SystemdSocketNoNet` erlaubt ausschließlich
    /// `Unprivileged` — `harw-warden` hält keine der vierzehn
    /// Sensor-Fähigkeiten, und „kein Netz" schließt insbesondere
    /// [`RequiredPrivilege::Netlink`] aus. `RequiredPrivilege::Netlink` ist in
    /// keinem der vier Budgets enthalten: keines der vier Binaries deklariert
    /// eine Netlink-Fähigkeit als seine eigene.
    ///
    /// # Arguments
    /// - `required` (`RequiredPrivilege`): die Fähigkeit, die eine erreichte
    ///   Crate mitbringt.
    ///
    /// # Returns
    /// `true`, wenn `required` innerhalb dieses Budgets liegt.
    ///
    /// # Errors
    /// Keine — totale, panikfreie Funktion.
    ///
    /// # Examples
    /// ```text
    /// BinaryBudget::CapSysAdmin.allows(RequiredPrivilege::FileWatch) == true
    /// BinaryBudget::CapSysAdmin.allows(RequiredPrivilege::Bpf) == false
    /// ```
    #[must_use]
    pub const fn allows(&self, required: RequiredPrivilege) -> bool {
        match self {
            Self::Unprivileged | Self::SystemdSocketNoNet => {
                matches!(required, RequiredPrivilege::Unprivileged)
            }
            Self::CapSysAdmin => matches!(
                required,
                RequiredPrivilege::Unprivileged | RequiredPrivilege::FileWatch
            ),
            Self::CapBpf => matches!(
                required,
                RequiredPrivilege::Unprivileged | RequiredPrivilege::Bpf
            ),
        }
    }
}

/// Eines der vier überwachten Binaries samt seinem deklarierten Budget.
///
/// # Description
/// Ein Eintrag in [`MONITORED_BINARIES`]. Der Binary-Name ist zugleich der
/// Crate-Name im Workspace-Graphen (`CrateNode::name`).
#[derive(Debug, Clone, Copy)]
pub struct MonitoredBinary {
    /// Crate-/Binary-Name, wie er im Workspace-Graphen steht.
    pub name: &'static str,
    /// Die deklarierte Berechtigungsklasse dieses Binaries.
    pub budget: BinaryBudget,
}

/// Die vier Binaries dieses Knotens samt ihrer Klasse aus dem Auftrag.
pub const MONITORED_BINARIES: &[MonitoredBinary] = &[
    MonitoredBinary {
        name: "harw-sentinel",
        budget: BinaryBudget::Unprivileged,
    },
    MonitoredBinary {
        name: "harw-probe-fs",
        budget: BinaryBudget::CapSysAdmin,
    },
    MonitoredBinary {
        name: "harw-probe-bpf",
        budget: BinaryBudget::CapBpf,
    },
    MonitoredBinary {
        name: "harw-warden",
        budget: BinaryBudget::SystemdSocketNoNet,
    },
];

/// Die handgepflegte Crate-→-Fähigkeit-Zuordnung — siehe Moduldoku,
/// Abschnitt „Woher die Fähigkeitszuordnung kommt". Jede hier fehlende
/// interne Crate ist in [`evaluate`] ein Verstoß, keine stille Freigabe.
///
/// Gruppiert nach Fähigkeit, nicht alphabetisch, damit ein neuer Eintrag an
/// der richtigen Stelle landet statt irgendwo im Alphabet.
pub const CRATE_PRIVILEGE: &[(&str, RequiredPrivilege)] = &[
    // --- Unprivilegierte Sensoren (Capability::class() == Unprivileged) ---
    ("harw-dod-cpu", RequiredPrivilege::Unprivileged),
    ("harw-dod-thermal", RequiredPrivilege::Unprivileged),
    ("harw-dod-memory", RequiredPrivilege::Unprivileged),
    ("harw-dod-blockio", RequiredPrivilege::Unprivileged),
    ("harw-dod-netcounters", RequiredPrivilege::Unprivileged),
    ("harw-dod-gpu", RequiredPrivilege::Unprivileged),
    ("harw-dod-cgroup", RequiredPrivilege::Unprivileged),
    ("harw-dod-listener", RequiredPrivilege::Unprivileged),
    ("harw-dod-scanreport", RequiredPrivilege::Unprivileged),
    ("harw-dod-workspace", RequiredPrivilege::Unprivileged),
    // --- Zugriffs-/Vokabularschichten (keine eigene Capability) ---
    ("harw-dod-cap", RequiredPrivilege::Unprivileged),
    // Reine Vokabularschicht (Netzbereiche, Egress-Ziele) ohne eigene
    // `Capability`-Deklaration — wie `harw-dod-cap` und `harw-dod-signals`.
    // Fehlte hier und hätte `harw-probe-bpf` fälschlich als Verstoß gemeldet,
    // sobald dessen Hülle sie über `harw-dod-flow` erreicht. Gemeldet von AW7-01d.
    ("harw-sandbox", RequiredPrivilege::Unprivileged),
    ("harw-dod-signals", RequiredPrivilege::Unprivileged),
    ("harw-dod-readfs", RequiredPrivilege::Unprivileged),
    ("harw-dod-sentinel", RequiredPrivilege::Unprivileged),
    // Keine eigene `Capability::`-Deklaration im Quelltext gefunden (Stand
    // dieses Knotens); als Zugriffs-/Protokollschicht ohne eigenen
    // Sensor-Handle vorläufig unprivilegiert eingestuft. Niedrigere
    // Sicherheit als die obigen Zeilen — siehe Abschlussbericht dieses
    // Knotens.
    ("harw-dod-netlink", RequiredPrivilege::Unprivileged),
    ("harw-dod-netpolicy", RequiredPrivilege::Unprivileged),
    ("harw-dod-warden", RequiredPrivilege::Unprivileged),
    ("harw-dod-warden-proto", RequiredPrivilege::Unprivileged),
    ("harw-dod-escalate", RequiredPrivilege::Unprivileged),
    // `harw-sentinel` wertet seit kurzem im Poll-Loop Regeln über
    // `harw_dod_rules::run_rules` aus (vorher lief die Regelauswertung
    // nirgends) — deshalb liegt `harw-dod-rules` überhaupt erst in der
    // Sentinel-Hülle. Die Crate liest ausschließlich bereits gesammelte
    // `SecurityEvent`/`HostSample`-Werte (siehe `rule.rs`, `engine.rs`,
    // `rules/*.rs`) und erzeugt daraus `Finding<S>` — kein Dateizugriff,
    // kein Syscall, kein Netz. Der einzige `std::net`-Import
    // (`rules/egress_flow.rs`) ist `IpAddr` als reiner Werttyp für den
    // Adressvergleich gegen `NetworkScope`, kein offener Socket. Keine
    // eigene `Capability`-Deklaration im Quelltext gefunden — unprivilegiert.
    ("harw-dod-rules", RequiredPrivilege::Unprivileged),
    // Kommt transitiv über `harw-dod-rules::confidence` (Aufruf von
    // `epistemic_confidence_for`) in die Sentinel-Hülle. Liefert nur eine
    // Konfidenzskala (`Confidence`) und deren Konvertierung aus einer
    // `Hardness`/`Severity`-Eingabe — reine Werttypen und `serde`/`jiff`-
    // Zeitstempel, kein Dateizugriff, kein Netz, kein Syscall. Unprivilegiert.
    ("harw-research", RequiredPrivilege::Unprivileged),
    // --- Kern-/Hilfscrates, tatsächlich in den heutigen Hüllen gesehen ---
    ("harw-code-graph", RequiredPrivilege::Unprivileged),
    ("harw-home", RequiredPrivilege::Unprivileged),
    ("harw-macros", RequiredPrivilege::Unprivileged), // Proc-Macro, siehe Moduldoku.
    ("harw-observe", RequiredPrivilege::Unprivileged),
    ("harw-observe-file", RequiredPrivilege::Unprivileged),
    ("harw-observe-prom", RequiredPrivilege::Unprivileged),
    ("harw-observe-otlp", RequiredPrivilege::Unprivileged),
    ("harw-types", RequiredPrivilege::Unprivileged),
    // --- Netlink (Capability::class() == Netlink) ---
    // Konservativ: `AuditBackend` trägt `Capability::ReadAuditNetlink`; das
    // (laut eigener Moduldoku bewusst nicht existierende) Journal-Backend
    // wäre unprivilegiert, zählt hier aber nicht — die Crate kann die
    // höhere Fähigkeit anfordern, und genau das prüft dieses Gate ("was sie
    // anfordern könnte", nicht was sie im Standardfall tut).
    ("harw-dod-authlog", RequiredPrivilege::Netlink),
    // --- FileWatch (Capability::class() == FileWatch) ---
    ("harw-dod-fsmon", RequiredPrivilege::FileWatch),
    // --- Bpf (Capability::class() == Bpf) ---
    ("harw-dod-bpf", RequiredPrivilege::Bpf),
    ("harw-dod-procmon", RequiredPrivilege::Bpf),
    ("harw-dod-flow", RequiredPrivilege::Bpf),
];

/// Schlägt die Fähigkeitsklasse einer Crate in [`CRATE_PRIVILEGE`] nach.
///
/// # Description
/// Lineare Suche über eine Tabelle mit derzeit rund drei Dutzend Einträgen —
/// ein `HashMap` wäre hier vorzeitige Optimierung für eine Tabelle, die
/// einmal je Gate-Lauf komplett durchlaufen wird.
///
/// # Arguments
/// - `name` (`&str`): der Crate-Name, wie er im Workspace-Graphen steht.
///
/// # Returns
/// `Some(RequiredPrivilege)`, wenn `name` eingetragen ist, sonst `None` —
/// `None` ist in [`evaluate`] immer ein Verstoß, nie eine stille Freigabe.
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
///
/// # Examples
/// ```text
/// lookup_privilege("harw-dod-bpf") == Some(RequiredPrivilege::Bpf)
/// lookup_privilege("irgendeine-neue-crate") == None
/// ```
#[must_use]
fn lookup_privilege(name: &str) -> Option<RequiredPrivilege> {
    CRATE_PRIVILEGE
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, class)| *class)
}

/// Ob ein Binary-Knoten als "bereits gebaut" gilt — siehe Moduldoku,
/// Abschnitt „Fehlendes Binary".
///
/// # Description
/// Ein Binary gilt als noch nicht vorhanden, wenn es entweder gar kein
/// Mitglied des Workspace ist (`node` ist `None`), oder wenn sein
/// `[dependencies]`-Abschnitt vollständig leer ist — weder ein interner noch
/// ein externer Eintrag. Letzteres ist der heutige Zustand von
/// `harw-probe-bpf` und `harw-warden`: reine Gerüst-Crates ohne eine einzige
/// Abhängigkeit.
///
/// # Arguments
/// - `node` (`Option<&CrateNode>`): der Graphknoten des Binaries, oder
///   `None`, wenn der Name kein Workspace-Mitglied ist.
///
/// # Returns
/// `true`, wenn das Binary geprüft werden kann (mindestens eine reguläre
/// Abhängigkeit trägt).
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
///
/// # Examples
/// ```text
/// is_binary_present(None) == false
/// ```
#[must_use]
fn is_binary_present(node: Option<&CrateNode>) -> bool {
    node.is_some_and(|n| !n.deps.is_empty() || !n.external_deps.is_empty())
}

/// Die Namen aller überwachten Binaries, die derzeit als nicht vorhanden gelten.
///
/// # Description
/// Öffentlich, damit sowohl [`run`] (zur Ausgabe je einer Hinweiszeile) als
/// auch Tests (ohne stdout mitschneiden zu müssen) dasselbe Urteil abfragen
/// können — siehe [`is_binary_present`] für das Kriterium.
///
/// # Arguments
/// - `graph` (`&WorkspaceGraph`): der zu prüfende Workspace-Graph.
///
/// # Returns
/// Die Namen aus [`MONITORED_BINARIES`], die aktuell fehlen, in der
/// Reihenfolge der Tabelle.
///
/// # Errors
/// Keine — totale, panikfreie Funktion.
///
/// # Examples
/// ```text
/// // Auf dem echten Workspace, solange AW7-01d/AW5-04b nicht gelandet sind:
/// missing_binaries(&graph) == vec!["harw-probe-bpf", "harw-warden"]
/// ```
#[must_use]
pub fn missing_binaries(graph: &WorkspaceGraph) -> Vec<&'static str> {
    MONITORED_BINARIES
        .iter()
        .filter(|spec| !is_binary_present(graph.get(spec.name)))
        .map(|spec| spec.name)
        .collect()
}

/// Alle über `deps` (nur reguläre, interne, produktive Kanten) von `binary`
/// aus erreichbaren Crates, je mit der kürzesten Kette dorthin.
///
/// # Description
/// Breitensuche (FIFO-Warteschlange), damit die zurückgegebene Kette für
/// jede erreichte Crate die **kürzeste** ist — die knappste, damit die
/// Verstoßmeldung sich auf das Wesentliche beschränkt. Folgt bewusst weder
/// `dev_deps` noch `build_deps` noch `external_deps` — siehe Moduldoku,
/// Abschnitt „Geltungsbereich der Hülle". `binary` selbst erscheint nicht in
/// der Ergebnisliste, wohl aber als erstes Glied jeder Kette.
///
/// # Arguments
/// - `binary` (`&str`): Name des Wurzel-Crates (eines der vier Binaries).
/// - `by_name` (`&HashMap<&str, &CrateNode>`): alle Crates des Graphen,
///   indiziert nach Name, für O(1)-Nachschlagen während der Suche.
///
/// # Returns
/// Paare aus erreichtem Crate-Namen und der Kette (`binary` eingeschlossen,
/// erreichte Crate als letztes Glied) dorthin, in Entdeckungsreihenfolge.
///
/// # Errors
/// Keine — totale Funktion; ein unbekannter `binary`-Name liefert eine leere
/// Liste (kein Knoten, keine Kanten).
///
/// # Examples
/// ```text
/// // Graph: a -> b -> c
/// reachable_with_chain("a", &by_name)
///     == vec![("b", vec!["a", "b"]), ("c", vec!["a", "b", "c"])]
/// ```
#[must_use]
fn reachable_with_chain(
    binary: &str,
    by_name: &HashMap<&str, &CrateNode>,
) -> Vec<(String, Vec<String>)> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    let mut reached = Vec::new();

    if let Some(node) = by_name.get(binary) {
        for dep in &node.deps {
            queue.push_back((dep.clone(), vec![binary.to_owned(), dep.clone()]));
        }
    }

    while let Some((name, chain)) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        reached.push((name.clone(), chain.clone()));
        if let Some(node) = by_name.get(name.as_str()) {
            for dep in &node.deps {
                if !visited.contains(dep) {
                    let mut extended = chain.clone();
                    extended.push(dep.clone());
                    queue.push_back((dep.clone(), extended));
                }
            }
        }
    }

    reached
}

/// Führt Gate 2 aus.
///
/// # Description
/// Lädt den echten Workspace-Graphen über `harw-code-graph` (kein
/// `cargo`-Subprozess) und delegiert die eigentliche Prüfung an [`evaluate`].
/// Gibt vor dem Ergebnis für jedes über [`missing_binaries`] erkannte,
/// noch nicht gebaute Binary eine eigene Hinweiszeile aus — das ist die in
/// der Moduldoku ("Fehlendes Binary") angekündigte Sichtbarkeit, die im
/// `checked`-Feld allein nicht steckt.
///
/// # Returns
/// Einen [`GateReport`] mit der Zahl der **tatsächlich geprüften** Binaries
/// (nicht `4`, solange Binaries fehlen) und den gefundenen Verstößen.
///
/// # Errors
/// Wenn der Workspace-Graph nicht gelesen werden kann (kaputtes oder
/// fehlendes `Cargo.toml`). Ein **Verstoß** ist dagegen kein `Err`, sondern
/// erscheint im Bericht: das Gate hat dann erfolgreich geprüft und etwas
/// gefunden.
///
/// # Examples
/// ```text
/// // In xtask/src/gates.rs:
/// let report = privileges::run()?;
/// println!("{}", report.summary());
/// ```
pub fn run() -> Result<GateReport, String> {
    let graph = WorkspaceGraph::load(Path::new("."))
        .map_err(|error| format!("Workspace-Graph nicht lesbar: {error}"))?;

    for binary in missing_binaries(&graph) {
        println!(
            "privileges: '{binary}' hat noch keine Abhängigkeiten (Gerüst) — nicht geprüft"
        );
    }

    Ok(evaluate(&graph))
}

/// Prüft einen bereits geladenen Graphen.
///
/// # Description
/// Getrennt von [`run`], damit die Prüfung gegen einen **konstruierten**
/// Graphen möglich ist — wie in `gate_edges.rs`. Für jedes in
/// [`MONITORED_BINARIES`] gelistete, tatsächlich vorhandene Binary
/// ([`is_binary_present`]) wird die vollständige Hülle
/// ([`reachable_with_chain`]) gebildet; jede erreichte Crate wird gegen
/// [`CRATE_PRIVILEGE`] geprüft: fehlt der Eintrag, ist das ein Verstoß
/// (Moduldoku, „Warum eine unbekannte Crate ein Verstoß ist"); ist die
/// Fähigkeit außerhalb des Budgets ([`BinaryBudget::allows`]), ebenfalls.
/// Ein fehlendes Binary wird übersprungen, ohne `checked` zu erhöhen und
/// ohne einen Verstoß zu erzeugen.
///
/// # Arguments
/// - `graph` (`&WorkspaceGraph`): der zu prüfende Abhängigkeitsgraph.
///
/// # Returns
/// Den Bericht mit der Zahl der geprüften Binaries und den gefundenen
/// Verstößen, je einer als Zeile mit Binary, Budget, erreichter Crate und
/// der vollständigen Kette dorthin.
///
/// # Errors
/// Keine — totale, panikfreie Funktion; [`Self`] ist hier nicht anwendbar,
/// diese Funktion gibt immer `Ok`-artig einen [`GateReport`] zurück (kein
/// `Result`).
///
/// # Examples
/// ```text
/// let report = evaluate(&graph);
/// assert!(report.is_green() || !report.violations.is_empty());
/// ```
#[must_use]
pub fn evaluate(graph: &WorkspaceGraph) -> GateReport {
    let by_name: HashMap<&str, &CrateNode> =
        graph.crates.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut violations = Vec::new();
    let mut checked = 0usize;

    for spec in MONITORED_BINARIES {
        if !is_binary_present(graph.get(spec.name)) {
            continue;
        }
        checked += 1;

        for (crate_name, chain) in reachable_with_chain(spec.name, &by_name) {
            let chain_str = chain.join(" → ");
            match lookup_privilege(&crate_name) {
                None => {
                    violations.push(format!(
                        "{} ({}) erreicht '{}' über {} — keine Fähigkeitszuordnung \
                         hinterlegt: eine unbekannte Crate in der Hülle zählt als Verstoß, \
                         kein stilles Durchwinken",
                        spec.name,
                        spec.budget.label(),
                        crate_name,
                        chain_str
                    ));
                }
                Some(required) if !spec.budget.allows(required) => {
                    violations.push(format!(
                        "{} ({}) erreicht {} ({}) über {}",
                        spec.name,
                        spec.budget.label(),
                        crate_name,
                        required.label(),
                        chain_str
                    ));
                }
                Some(_) => {}
            }
        }
    }

    GateReport {
        name: "privileges",
        checked,
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn node(name: &str, deps: &[&str]) -> CrateNode {
        CrateNode {
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: PathBuf::from(format!("{name}/Cargo.toml")),
            dir: PathBuf::from(name),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: Vec::new(),
            is_leaf: deps.is_empty(),
            level: 0,
        }
    }

    fn graph(crates: Vec<CrateNode>) -> WorkspaceGraph {
        WorkspaceGraph {
            root: PathBuf::from("."),
            crates,
        }
    }

    #[test]
    fn test_evaluate_unprivileged_binary_reaching_bpf_crate_is_a_violation() {
        let g = graph(vec![
            node("harw-sentinel", &["harw-dod-cpu", "harw-dod-bpf"]),
            node("harw-dod-cpu", &[]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert_eq!(report.checked, 1, "nur harw-sentinel ist im Graphen vorhanden");
        assert!(
            report.violations.iter().any(|v| {
                v.contains("harw-sentinel (unprivilegiert)")
                    && v.contains("harw-dod-bpf (CAP_BPF)")
                    && v.contains("harw-sentinel → harw-dod-bpf")
            }),
            "Meldung muss Binary, Klasse, Ziel-Crate und Kette nennen: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_same_binary_without_bpf_edge_is_green() {
        let g = graph(vec![
            node("harw-sentinel", &["harw-dod-cpu"]),
            node("harw-dod-cpu", &[]),
        ]);

        let report = evaluate(&g);

        assert!(report.is_green(), "{:?}", report.violations);
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn test_evaluate_crate_missing_from_mapping_is_a_violation() {
        let g = graph(vec![
            node("harw-sentinel", &["harw-brandneue-sensor-crate"]),
            node("harw-brandneue-sensor-crate", &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report.violations.iter().any(|v| {
                v.contains("harw-brandneue-sensor-crate")
                    && v.contains("keine Fähigkeitszuordnung")
            }),
            "eine nicht eingetragene Crate muss als Verstoß erscheinen: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_missing_binary_is_not_red_and_not_counted() {
        // Nur harw-sentinel ist im Graphen; die anderen drei Binaries fehlen
        // vollständig (kein Workspace-Mitglied in diesem konstruierten Graphen).
        let g = graph(vec![
            node("harw-sentinel", &["harw-dod-cpu"]),
            node("harw-dod-cpu", &[]),
        ]);

        let report = evaluate(&g);

        assert!(report.is_green());
        assert_eq!(report.checked, 1, "nur harw-sentinel wurde tatsächlich geprüft");
        assert_eq!(
            missing_binaries(&g),
            vec!["harw-probe-fs", "harw-probe-bpf", "harw-warden"]
        );
    }

    #[test]
    fn test_evaluate_stub_binary_with_empty_deps_counts_as_missing() {
        // Entspricht dem heutigen Zustand von harw-probe-bpf/harw-warden:
        // Workspace-Mitglied, aber [dependencies] vollständig leer.
        let g = graph(vec![node("harw-probe-bpf", &[])]);

        let report = evaluate(&g);

        assert!(report.is_green());
        assert_eq!(report.checked, 0);
        assert_eq!(
            missing_binaries(&g),
            vec!["harw-sentinel", "harw-probe-fs", "harw-probe-bpf", "harw-warden"]
        );
    }

    #[test]
    fn test_evaluate_dev_dependency_on_privileged_crate_does_not_count() {
        let mut sentinel = node("harw-sentinel", &["harw-dod-cpu"]);
        sentinel.dev_deps = vec!["harw-dod-bpf".to_owned()];
        let g = graph(vec![
            sentinel,
            node("harw-dod-cpu", &[]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(
            report.is_green(),
            "Dev-Abhängigkeiten landen nicht im ausgelieferten Binary: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_build_dependency_on_privileged_crate_does_not_count() {
        let mut sentinel = node("harw-sentinel", &["harw-dod-cpu"]);
        sentinel.build_deps = vec!["harw-dod-bpf".to_owned()];
        let g = graph(vec![
            sentinel,
            node("harw-dod-cpu", &[]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(
            report.is_green(),
            "Build-Abhängigkeiten laufen zur Bauzeit, nicht im Binary: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_transitive_edge_over_three_hops_is_found() {
        // Nicht direkt: harw-sentinel -> harw-dod-cpu -> harw-dod-thermal -> harw-dod-bpf.
        let g = graph(vec![
            node("harw-sentinel", &["harw-dod-cpu"]),
            node("harw-dod-cpu", &["harw-dod-thermal"]),
            node("harw-dod-thermal", &["harw-dod-bpf"]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert_eq!(
            report.violations.len(),
            1,
            "nur die eine Bpf-Kante ist ein Verstoß: {:?}",
            report.violations
        );
        assert!(
            report.violations[0]
                .contains("harw-sentinel → harw-dod-cpu → harw-dod-thermal → harw-dod-bpf"),
            "die transitive Kette muss vollständig benannt werden: {:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_cap_sys_admin_allows_filewatch_is_green() {
        let g = graph(vec![
            node("harw-probe-fs", &["harw-dod-fsmon"]),
            node("harw-dod-fsmon", &[]),
        ]);

        let report = evaluate(&g);

        assert!(report.is_green(), "{:?}", report.violations);
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn test_evaluate_cap_sys_admin_reaching_bpf_is_a_violation() {
        let g = graph(vec![
            node("harw-probe-fs", &["harw-dod-bpf"]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.contains("harw-probe-fs (CAP_SYS_ADMIN)") && v.contains("CAP_BPF")),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_cap_bpf_allows_bpf_is_green() {
        let g = graph(vec![
            node("harw-probe-bpf", &["harw-dod-bpf"]),
            node("harw-dod-bpf", &[]),
        ]);

        let report = evaluate(&g);

        assert!(report.is_green(), "{:?}", report.violations);
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn test_evaluate_systemd_socket_reaching_netlink_is_a_violation() {
        let g = graph(vec![
            node("harw-warden", &["harw-dod-authlog"]),
            node("harw-dod-authlog", &[]),
        ]);

        let report = evaluate(&g);

        assert!(!report.is_green());
        assert!(
            report.violations.iter().any(|v| {
                v.contains("harw-warden (systemd-Socket, kein Netz)")
                    && v.contains("harw-dod-authlog (CAP_AUDIT_READ)")
            }),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn test_evaluate_empty_graph_is_green_with_zero_checked() {
        let report = evaluate(&graph(Vec::new()));

        assert!(report.is_green());
        assert_eq!(report.checked, 0);
        assert_eq!(missing_binaries(&graph(Vec::new())).len(), 4);
    }

    #[test]
    fn test_evaluate_sentinel_reaching_dod_rules_and_research_is_green() {
        // Reproduziert die reale Verdrahtung: harw-sentinel -> harw-dod-rules
        // -> harw-research (transitiv über epistemic_confidence_for).
        let g = graph(vec![
            node("harw-sentinel", &["harw-dod-rules"]),
            node("harw-dod-rules", &["harw-research"]),
            node("harw-research", &[]),
        ]);

        let report = evaluate(&g);

        assert!(
            report.is_green(),
            "beide Crates sind unprivilegiert eingetragen: {:?}",
            report.violations
        );
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn test_lookup_privilege_finds_dod_rules_and_research_as_unprivileged() {
        assert_eq!(
            lookup_privilege("harw-dod-rules"),
            Some(RequiredPrivilege::Unprivileged)
        );
        assert_eq!(
            lookup_privilege("harw-research"),
            Some(RequiredPrivilege::Unprivileged)
        );
    }

    #[test]
    fn test_lookup_privilege_returns_none_for_unknown_and_some_for_known() {
        assert_eq!(lookup_privilege("harw-dod-bpf"), Some(RequiredPrivilege::Bpf));
        assert_eq!(lookup_privilege("irgendeine-neue-crate"), None);
    }

    #[test]
    fn test_binary_budget_labels_match_the_task_table() {
        assert_eq!(BinaryBudget::Unprivileged.label(), "unprivilegiert");
        assert_eq!(BinaryBudget::CapSysAdmin.label(), "CAP_SYS_ADMIN");
        assert_eq!(BinaryBudget::CapBpf.label(), "CAP_BPF");
        assert_eq!(
            BinaryBudget::SystemdSocketNoNet.label(),
            "systemd-Socket, kein Netz"
        );
    }

    #[test]
    fn test_binary_budget_allows_unprivileged_everywhere() {
        for budget in [
            BinaryBudget::Unprivileged,
            BinaryBudget::CapSysAdmin,
            BinaryBudget::CapBpf,
            BinaryBudget::SystemdSocketNoNet,
        ] {
            assert!(budget.allows(RequiredPrivilege::Unprivileged));
        }
    }

    #[test]
    fn test_binary_budget_no_budget_allows_netlink() {
        for budget in [
            BinaryBudget::Unprivileged,
            BinaryBudget::CapSysAdmin,
            BinaryBudget::CapBpf,
            BinaryBudget::SystemdSocketNoNet,
        ] {
            assert!(!budget.allows(RequiredPrivilege::Netlink));
        }
    }
}
