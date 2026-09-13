//! Kommandozeilen-Grammatik von `harw-probe-bpf` — `clap`, wie jedes andere
//! Binary dieses Projekts.
//!
//! # Verantwortungsbereich
//! [`Cli`] bündelt jeden Startparameter dieser Sonde: das verbindliche
//! `--log`-Flag (siehe [`LogLevel`]-Moduldoku für die Abweichung von
//! `harw-cli`s Fehlerverhalten), den Sentinel-Socket (`--sentinel-socket`,
//! Pflicht), die beiden Sensor-Kennungen (`--sensor-id-procmon`,
//! `--sensor-id-flow` — siehe `crate`-Moduldoku, Abschnitt
//! „Das `SensorId`-Schema"), die optionalen Programmpfade
//! (`--procmon-program-path`, `--flow-program-path`) und die
//! Melderegel-Eingabe für `harw-dod-flow` (`--egress-allow-cidr`,
//! wiederholbar). Diese Datei parst nur — sie kennt weder `BpfLoader` noch
//! `NetworkScope` noch einen Sensor.
//!
//! # `--log`: ein ungültiger Wert ist ein Fehler
//! Wie `harw-sentinel` und `harw-probe-fs` (und anders als `harw-cli`, dessen
//! `--log` ein unvalidierter `String` ist, der bei einem ungültigen Wert
//! **still** auf `"warn"` zurückfällt) ist [`LogLevel`] ein
//! `clap::ValueEnum`: ein nicht in dieser Liste enthaltener Wert ist ein
//! **Parse-Fehler** (`clap::Error`, `main` beendet mit `ExitCode::FAILURE`),
//! keine stille Voreinstellung. Ein Prozess mit `CAP_BPF`, der eine falsch
//! getippte Kommandozeile still ignoriert, verhält sich anders, als der
//! Aufrufer erwartet hat — bei dieser Berechtigungsklasse kein akzeptables
//! Verhalten.
//!
//! # Warum es kein `--egress-allow-host`/`--egress-allow-suffix` gibt
//! `harw_sandbox::NetworkScope` kennt drei Zielarten
//! (`EgressTarget::Host`/`DnsSuffix`/`Cidr`), aber `EgressTarget::matches_addr`
//! — die einzige Prüfung, die `harw_dod_flow::observe` tatsächlich aufruft —
//! liefert für `Host` und `DnsSuffix` **strukturell immer `false`**: diese
//! beiden Zielarten sind für einen hostnamenbasierten Abgleich
//! (`NetworkScope::allows`) gedacht, den diese Sonde nie ausführt, weil ein
//! eBPF-Tracepoint rohe IP-Adressen beobachtet, keine aufgelösten
//! Hostnamen. Zwei Flags anzubieten, die ein Betreiber sinnvoll ausgefüllt
//! glaubt, die aber für diesen Konsumenten nie etwas bewirken, wäre
//! irreführender als ihr Fehlen — deshalb parst diese Datei ausschließlich
//! `--egress-allow-cidr`.
//!
//! # Exportierte Typen
//! [`Cli`], [`LogLevel`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! Keine eigenen — `clap::Parser::try_parse`/`try_parse_from` liefern
//! `clap::Error`, das `main` direkt behandelt (siehe `crate`-Moduldoku).
//!
//! # Examples
//! ```rust,ignore
//! use crate::cli::Cli;
//! use clap::Parser as _;
//!
//! let cli = Cli::try_parse_from([
//!     "harw-probe-bpf",
//!     "--sentinel-socket", "/run/harw-sentinel.sock",
//! ])
//! .expect("minimal valid arguments");
//! assert_eq!(cli.log, crate::cli::LogLevel::Info);
//! ```

use std::path::PathBuf;

use clap::Parser;
use ipnet::IpNet;

/// Vorgabe-Kennung des Prozessstart-Sensors, sofern `--sensor-id-procmon`
/// nicht gesetzt ist. Siehe `crate`-Moduldoku, Abschnitt „Das
/// `SensorId`-Schema".
pub const DEFAULT_SENSOR_ID_PROCMON: &str = "probe-bpf-procmon-0";

/// Vorgabe-Kennung des Verbindungs-Sensors, sofern `--sensor-id-flow` nicht
/// gesetzt ist. Siehe `crate`-Moduldoku, Abschnitt „Das `SensorId`-Schema".
pub const DEFAULT_SENSOR_ID_FLOW: &str = "probe-bpf-flow-0";

/// Gültige Werte für `--log`, in `tracing_subscriber::EnvFilter`-Syntax.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „--log: ein ungültiger Wert ist ein Fehler".
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum LogLevel {
    /// Feinste Stufe: einzelne Rohereignisse, Zwischenschritte der Formung.
    Trace,
    /// Entscheidungsverzweigungen (z. B. „innerhalb des Scopes verworfen").
    Debug,
    /// Phasengrenzen: Start, Sentinel-Verbindung, Landlock-Ausgang,
    /// Sensor-Registrierung, Sendebestätigung je Runde.
    Info,
    /// Wiederherstellbare Abweichung: ein transienter Sensorfehler.
    Warn,
    /// Nicht behebbarer Fehlschlag vor einem harten Start- oder
    /// Laufzeitabbruch.
    Error,
}

impl LogLevel {
    /// Wandelt diese Stufe in eine `tracing_subscriber::EnvFilter`-Direktive.
    ///
    /// # Returns
    /// Einen statischen Bezeichner (`"trace"`, `"debug"`, `"info"`, `"warn"`
    /// oder `"error"`), den [`tracing_subscriber::EnvFilter::try_new`] ohne
    /// weitere Übersetzung akzeptiert.
    ///
    /// # Examples
    /// ```rust,ignore
    /// use crate::cli::LogLevel;
    ///
    /// assert_eq!(LogLevel::Warn.as_filter_directive(), "warn");
    /// ```
    #[must_use]
    pub const fn as_filter_directive(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Parst einen `--egress-allow-cidr`-Wert als `ipnet::IpNet`.
///
/// # Description
/// Eigene Parserfunktion statt der `FromStr`-Ableitung von `clap`, damit die
/// Fehlermeldung von `ipnet` unverändert als `clap`-Parsefehler erscheint,
/// ohne von der `value_parser!`-Standardauflösung für einen fremden Typ
/// abhängig zu sein.
///
/// # Arguments
/// - `raw` (`&str`): der rohe Kommandozeilenwert.
///
/// # Returns
/// Das geparste `IpNet`.
///
/// # Errors
/// Die `Display`-Meldung von `ipnet::AddrParseError` als `String`, wenn
/// `raw` kein gültiges CIDR ist — `clap` macht daraus einen harten
/// Parsefehler, keinen stillen Rückfall.
fn parse_cidr(raw: &str) -> Result<IpNet, String> {
    raw.parse::<IpNet>().map_err(|err| err.to_string())
}

/// Vollständige Kommandozeilen-Grammatik von `harw-probe-bpf`.
#[derive(Debug, Parser)]
#[command(
    name = "harw-probe-bpf",
    bin_name = "harw-probe-bpf",
    version,
    about = "eBPF-Sonde, Push-Only: fährt die Prozessstart- und Verbindungssensoren und sendet ihre Ereignisse an den Sentinel."
)]
pub struct Cli {
    /// Log-Stufe für `tracing`. Siehe [`LogLevel`] für die Begründung, warum
    /// ein ungültiger Wert hier ein Fehler ist statt einer stillen
    /// Voreinstellung.
    #[arg(long, value_enum, default_value = "info")]
    pub log: LogLevel,

    /// Pfad des `SOCK_SEQPACKET`-Unix-Sockets, über den der Sentinel hört.
    /// Pflicht — es gibt keinen betriebsfähigen Vorgabewert, der nicht von
    /// einer laufenden Sentinel-Instanz abhinge.
    #[arg(long, value_name = "PATH")]
    pub sentinel_socket: PathBuf,

    /// Kennung, unter der der Prozessstart-Sensor seine Ereignisse meldet
    /// (`SecurityEvent::sensor`). Siehe `crate`-Moduldoku, Abschnitt „Das
    /// `SensorId`-Schema".
    #[arg(long = "sensor-id-procmon", value_name = "ID", default_value = DEFAULT_SENSOR_ID_PROCMON)]
    pub sensor_id_procmon: String,

    /// Kennung, unter der der Verbindungs-Sensor seine Ereignisse meldet
    /// (`SecurityEvent::sensor`). Siehe `crate`-Moduldoku, Abschnitt „Das
    /// `SensorId`-Schema".
    #[arg(long = "sensor-id-flow", value_name = "ID", default_value = DEFAULT_SENSOR_ID_FLOW)]
    pub sensor_id_flow: String,

    /// Optionaler Dateisystempfad des Prozessstart-Programmrumpfs
    /// (`harw_dod_bpf::BpfProgramSource::Path`). Ohne Angabe verwendet diese
    /// Sonde einen leeren, eingebetteten Platzhalterrumpf — siehe
    /// `crate::real_loader`-Moduldoku für die Begründung, warum in dieser
    /// Lieferung ohnehin kein realer Lader existiert, der einen Rumpf
    /// tatsächlich verwenden würde.
    #[arg(long = "procmon-program-path", value_name = "PATH")]
    pub procmon_program_path: Option<PathBuf>,

    /// Optionaler Dateisystempfad des Verbindungs-Programmrumpfs. Siehe
    /// [`Self::procmon_program_path`] für die Begründung des
    /// Platzhalterverhaltens ohne Angabe.
    #[arg(long = "flow-program-path", value_name = "PATH")]
    pub flow_program_path: Option<PathBuf>,

    /// Erlaubte Zielnetze für die Melderegel von `harw-dod-flow`
    /// (`EgressTarget::Cidr`). Wiederholbar; ohne Angabe ein leerer
    /// `NetworkScope`, der jede ausgehende Verbindung meldet — die sichere
    /// Grundeinstellung laut `harw_dod_flow::report`-Moduldoku.
    #[arg(long = "egress-allow-cidr", value_name = "CIDR", value_parser = parse_cidr)]
    pub egress_allow_cidr: Vec<IpNet>,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, LogLevel, DEFAULT_SENSOR_ID_FLOW, DEFAULT_SENSOR_ID_PROCMON};

    fn minimal_args() -> Vec<&'static str> {
        vec!["harw-probe-bpf", "--sentinel-socket", "/run/harw-sentinel.sock"]
    }

    #[test]
    fn test_log_defaults_to_info_when_omitted() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert_eq!(cli.log, LogLevel::Info);
    }

    #[test]
    fn test_log_accepts_every_documented_value() {
        for value in ["trace", "debug", "info", "warn", "error"] {
            let mut args = minimal_args();
            args.push("--log");
            args.push(value);
            let cli = Cli::try_parse_from(args)
                .unwrap_or_else(|err| panic!("expected {value} to parse, got {err}"));
            assert_eq!(cli.log.as_filter_directive(), value);
        }
    }

    #[test]
    fn test_log_rejects_invalid_value_as_a_hard_error() {
        let mut args = minimal_args();
        args.push("--log");
        args.push("verbose");
        let result = Cli::try_parse_from(args);
        assert!(
            result.is_err(),
            "an unrecognised --log value must be a parse error, not a silent default"
        );
    }

    #[test]
    fn test_sentinel_socket_is_required() {
        let result = Cli::try_parse_from(["harw-probe-bpf"]);
        assert!(result.is_err(), "missing --sentinel-socket must be rejected");
    }

    #[test]
    fn test_sensor_ids_use_documented_defaults() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert_eq!(cli.sensor_id_procmon, DEFAULT_SENSOR_ID_PROCMON);
        assert_eq!(cli.sensor_id_flow, DEFAULT_SENSOR_ID_FLOW);
    }

    #[test]
    fn test_program_paths_default_to_none() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert!(cli.procmon_program_path.is_none());
        assert!(cli.flow_program_path.is_none());
    }

    #[test]
    fn test_egress_allow_cidr_defaults_to_empty() {
        let cli = Cli::try_parse_from(minimal_args()).expect("minimal valid arguments");
        assert!(cli.egress_allow_cidr.is_empty());
    }

    #[test]
    fn test_egress_allow_cidr_collects_multiple_occurrences() {
        let mut args = minimal_args();
        args.extend(["--egress-allow-cidr", "10.0.0.0/24", "--egress-allow-cidr", "192.168.0.0/16"]);
        let cli = Cli::try_parse_from(args).expect("two valid CIDR values");
        assert_eq!(cli.egress_allow_cidr.len(), 2);
    }

    #[test]
    fn test_egress_allow_cidr_rejects_invalid_value_as_a_hard_error() {
        let mut args = minimal_args();
        args.extend(["--egress-allow-cidr", "not-a-cidr"]);
        let result = Cli::try_parse_from(args);
        assert!(result.is_err(), "an unparsable CIDR must be a parse error");
    }

    #[test]
    fn test_program_path_override_is_honoured() {
        let mut args = minimal_args();
        args.extend(["--procmon-program-path", "/opt/harw/procmon.bpf.o"]);
        let cli = Cli::try_parse_from(args).expect("valid override");
        assert_eq!(
            cli.procmon_program_path,
            Some(std::path::PathBuf::from("/opt/harw/procmon.bpf.o"))
        );
    }
}
