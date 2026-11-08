//! Sammelschleife: Audit-Records holen, in Anmeldeereignisse wandeln, senden.
//!
//! # Verantwortungsbereich
//! [`convert_records`] wandelt einen Stapel `harw_dod_netlink::RawRecord`
//! über die eine Regel in `harw_dod_authlog`
//! (`auth_record_from_raw`, `auth_record_to_event`) in
//! `SecurityEvent`s mit `EventKind::AuthEvent`; [`run_once`] holt einen
//! Stapel aus der Quelle, wandelt ihn und schiebt jedes zugelassene Ereignis
//! über die Senke; [`run_forever`] wiederholt das, bis ein dauerhafter
//! Fehler auftritt. Die Regel, **welche** Records Anmeldeereignisse sind,
//! steht nicht hier, sondern in `harw_dod_authlog::LOGIN_RECORD_TYPES`
//! (`USER_AUTH`, `USER_LOGIN`); alle übrigen Records des Audit-Stroms
//! (`SYSCALL`, `PROCTITLE`, `USER_CMD`, …) werden gezählt und verworfen —
//! mit einer Ausnahme, siehe „Abgeschaltetes Audit".
//!
//! # Datenstrom statt Fenster
//! Anders als `harw_dod_authlog::AuthlogSensor` (Rückschaufenster,
//! mindestens einmal) sieht diese Sonde jeden Record genau einmal: die
//! Multicast-Gruppe liefert ihn einmal, und er wird sofort gesendet. Es gibt
//! deshalb keinen `since`-Filter und keine Dubletten aus überlappenden
//! Fenstern; `observed_at` ist der Zeitstempel des Records, nicht die
//! Ankunftszeit.
//!
//! # Anmeldebudget: eine Anmeldeflut blendet die anderen Sonden nicht
//! Der Sentinel legt die Ereignisse **aller** Sonden in eine gemeinsame
//! IPC-Inbox mit Verdrängung des Ältesten (`harw-sentinel`,
//! `IPC_INBOX_CAPACITY` = 256 je Zyklus von 5 s). Jeder gescheiterte
//! SSH-Versuch ergibt zwei bis drei `AuthEvent`s; ein entfernter, nicht
//! angemeldeter Angreifer mit 20 Versuchen je Sekunde erzeugt also über 50
//! Ereignisse je Sekunde und würde ohne Grenze die Exec- und
//! Egress-Ereignisse von `harw-probe-bpf`/`harw-probe-fs` aus der Inbox
//! drängen (B2/B10/I10). Deshalb sendet [`run_once`] je
//! [`AUTH_WINDOW`] (gemessen an der injizierten Zeit `now`) höchstens
//! [`MAX_AUTH_FAILURES_PER_WINDOW`] gescheiterte und
//! [`MAX_AUTH_SUCCESSES_PER_WINDOW`] gelungene Anmeldeereignisse
//! ([`AuthBudget`]) — zusammen 32.
//!
//! Getrennte Budgets, weil die Flut, die ein nicht angemeldeter Angreifer
//! erzeugen kann, nur aus Fehlschlägen besteht: sie darf die **eine**
//! gelungene Anmeldung, auf die eine Brute-Force-Regel wartet, nicht aus
//! dem Fenster drängen. Was über dem Budget liegt, wird gezählt
//! ([`CycleStats::rate_limited`]) und wie ein Verlust gedrosselt als
//! `SensorDegraded` gemeldet (nächster Abschnitt). Eine Brute-Force-Regel
//! sieht auch gedrosselt noch 24 Fehlschläge je 5 s — weit über jeder
//! sinnvollen Schwelle.
//!
//! # Verlust wird gemeldet, nicht verschwiegen
//! Liest die Sonde zu langsam, verwirft der Kernel Records und meldet
//! `ENOBUFS` (`harw_dod_netlink::NetlinkError::Overrun`); war ein Datagramm
//! größer als der Lesepuffer, ist sein Record ebenfalls verloren
//! (`harw_dod_netlink::NetlinkError::Truncated`). Beides beendet die Sonde
//! nicht. Jeder Verlust wird gezählt ([`CycleStats::overruns`],
//! [`CycleStats::truncated`]), ebenso jedes gedrosselte Anmeldeereignis
//! ([`CycleStats::rate_limited`]) und jeder fehlerhafte Anmelderecord
//! ([`CycleStats::malformed`]). Für all das geht ein
//! `EventKind::SensorDegraded` an den Sentinel — aber **höchstens eines je
//! [`DEGRADED_REPORT_INTERVAL`]** ([`OverrunState`], gemessen an der
//! injizierten Zeit `now`).
//!
//! Fehlerhafte Records gehören dazu, weil sie sonst still blieben: ändert
//! ein Kernel oder libaudit das Format so, dass jeder `USER_AUTH`/
//! `USER_LOGIN` unparsebar wird, sendet die Sonde null `AuthEvent`s, und der
//! Sentinel könnte das nicht von „keine Anmeldungen" unterscheiden (B19).
//!
//! Warum zeitlich und nicht „einmal je Verlustphase": unter Dauerlast
//! wechselt der Kernel zwischen `ENOBUFS` und einer geleerten, wieder
//! lesbaren Warteschlange (`NETLINK_S_CONGESTED` fällt, sobald sie leer
//! ist). Eine Phase, die mit dem nächsten nicht leeren Lesevorgang endet,
//! ergäbe dann eine Meldung je `ENOBUFS` — viele je Minute, genau dann, wenn
//! der Host ohnehin unter Last steht, und genug, um den Ereignisring des
//! Sentinels mit `SensorDegraded` zu füllen (B2/B10). Die Zähler laufen
//! dagegen ungedrosselt weiter und stehen in der periodischen Statistik.
//!
//! # Abgeschaltetes Audit
//! Ohne `CAP_AUDIT_CONTROL` kann die Sonde den Kernel nicht fragen, ob
//! Auditing an ist (`AUDIT_GET` verlangt diese Fähigkeit). Mit dem
//! Bootparameter `audit=0` registriert der Kernel die Audit-Netlink-Familie
//! gar nicht; dann scheitert schon der Start (`main::run`), sichtbar als
//! gescheiterte Unit.
//! Für die beiden stillen Fälle gibt es zwei Hinweise, beide als
//! `SensorDegraded`:
//! - **Abschalten zur Laufzeit** (`auditctl -e 0`): der Kernel schreibt dabei
//!   noch einen `CONFIG_CHANGE`-Record (Typ 1305) mit `audit_enabled=0` und
//!   `res=1` in die Multicast-Gruppe. [`convert_records`] erkennt ihn
//!   ([`CycleStats::audit_disabled`]); gemeldet wird sofort, mit eigener
//!   Drossel (höchstens eines je [`DEGRADED_REPORT_INTERVAL`]), damit eine
//!   kurz zuvor gemeldete Anmeldeflut die Meldung nicht verschluckt. Den
//!   Typ 1305 setzt nur der Kernel; Userspace darf nur `USER_*`-Typen
//!   senden, ein `audit_enabled=0` im Text eines `USER_CMD` zählt nicht.
//! - **Stille** (Audit war nie an, etwa ohne `auditd` und ohne `audit=1`,
//!   oder der Strom ist aus anderem Grund versiegt): kommt
//!   [`AUDIT_SILENCE_INTERVAL`] lang **kein** Record irgendeiner Art an,
//!   meldet [`SilenceWatch`] ein `SensorDegraded`, danach höchstens eines je
//!   weiterem Intervall Stille. Auf einem Host mit eingeschaltetem Audit
//!   kommen Records auch ohne Anmeldungen (Dienststarts, Cron-Sitzungen,
//!   meist schon der `EVENT_LISTENER`-Record des eigenen Beitritts); auf
//!   einem sehr ruhigen Host ist die Meldung ein Fehlalarm, höchstens einer
//!   je Stunde — der Preis dafür, dass eine blinde Sonde nicht als „keine
//!   Anmeldungen" erscheint (B19).
//!
//! # Was nie im Log steht
//! Kein Recordinhalt, kein Benutzer-, Host- oder Terminalname, keine Adresse:
//! geloggt werden nur Zähler.
//!
//! # Exportierte Typen
//! [`CycleStats`], [`Converted`], [`OverrunState`], [`AuthBudget`],
//! [`SilenceWatch`], [`LoopState`], die Funktionen [`convert_records`],
//! [`run_once`], [`run_forever`] und die Konstanten [`READ_TIMEOUT`],
//! [`STATS_LOG_INTERVAL`], [`DEGRADED_REPORT_INTERVAL`], [`AUTH_WINDOW`],
//! [`MAX_AUTH_FAILURES_PER_WINDOW`], [`MAX_AUTH_SUCCESSES_PER_WINDOW`],
//! [`AUDIT_SILENCE_INTERVAL`].
//!
//! # Nebenläufigkeit
//! Läuft vollständig im Hauptthread; kein weiterer Thread.
//!
//! # Fehler
//! [`crate::error::ProbeError::AuditRead`] für einen dauerhaften Lesefehler
//! der Quelle (nie für `Overrun` oder `Truncated`), jeder Fehler aus
//! [`crate::sink::EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::collect::{LoopState, run_once};
//!
//! let mut state = LoopState::default();
//! let stats = run_once(source, &sensor, sink, &mut state, jiff::Timestamp::now())?;
//! ```

use std::time::{Duration, Instant};

use harw_dod_authlog::{auth_record_from_raw, auth_record_to_event};
use harw_dod_netlink::{AuditSource, NetlinkError, RawRecord, parse_record};
use harw_dod_signals::{AuthOutcome, EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::{SignedDuration, Timestamp};

use crate::error::ProbeError;
use crate::sink::EventSink;

/// Wartezeit je Lesevorgang auf der Audit-Quelle.
///
/// Kurz genug, dass die Schleife auch ohne Records regelmäßig ihre
/// Statistik und die Stille prüft; lang genug, dass eine ruhige Quelle
/// keine Last erzeugt.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Abstand, in dem [`run_forever`] die kumulierten Zähler auf `info` loggt.
pub const STATS_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Mindestabstand zweier `SensorDegraded`-Meldungen derselben Drossel:
/// **60 Sekunden**, gemessen an der injizierten Zeit.
///
/// # Description
/// Einmal je Minute reicht, damit der Sentinel weiß, dass die Quelle Lücken
/// hat; mehr Meldungen würden seinen Ereignisring nur mit Wiederholungen
/// füllen. Gleich [`STATS_LOG_INTERVAL`], damit Meldung und Statistik im
/// selben Takt stehen.
pub const DEGRADED_REPORT_INTERVAL: SignedDuration = SignedDuration::from_secs(60);

/// Länge eines Fensters des Anmeldebudgets ([`AuthBudget`]): **5 Sekunden**.
///
/// # Description
/// Gleich dem Abrufzyklus des Sentinels (`harw-sentinel`,
/// `DEFAULT_INTERVAL_SECS`), dessen IPC-Inbox je Zyklus 256 Ereignisse
/// fasst; das Budget dieser Sonde belegt davon höchstens
/// [`MAX_AUTH_FAILURES_PER_WINDOW`] + [`MAX_AUTH_SUCCESSES_PER_WINDOW`] = 32.
pub const AUTH_WINDOW: SignedDuration = SignedDuration::from_secs(5);

/// Höchstzahl gesendeter **gescheiterter** Anmeldeereignisse je
/// [`AUTH_WINDOW`]: **24**.
///
/// # Description
/// Das Budget, das ein nicht angemeldeter Angreifer ausschöpfen kann. 24 je
/// 5 s sind fast fünf Fehlschläge je Sekunde — genug für jede
/// Brute-Force-Schwelle, wenig genug, dass die Inbox des Sentinels den
/// anderen Sonden bleibt.
pub const MAX_AUTH_FAILURES_PER_WINDOW: u32 = 24;

/// Höchstzahl gesendeter **gelungener** Anmeldeereignisse je
/// [`AUTH_WINDOW`]: **8**.
///
/// # Description
/// Eigenes Budget, damit eine Flut von Fehlschlägen die gelungene Anmeldung
/// nicht verdrängt (siehe Moduldoku). Kleiner, weil eine Flut gelungener
/// Anmeldungen gültige Zugangsdaten voraussetzt.
pub const MAX_AUTH_SUCCESSES_PER_WINDOW: u32 = 8;

/// So lange darf der Audit-Strom ganz schweigen, bevor [`SilenceWatch`] ein
/// `SensorDegraded` meldet: **eine Stunde**.
///
/// # Description
/// Siehe Moduldoku, „Abgeschaltetes Audit": kürzer ergäbe auf ruhigen Hosts
/// regelmäßig Fehlalarme, länger ließe eine blinde Sonde zu lange
/// unbemerkt.
pub const AUDIT_SILENCE_INTERVAL: SignedDuration = SignedDuration::from_hours(1);

// `nlmsg_type` eines `CONFIG_CHANGE`-Records (`<linux/audit.h>`); nur der
// Kernel setzt ihn.
const CONFIG_CHANGE_TYPE: u16 = 1305;

// Der Name derselben Recordart, wie `harw_dod_netlink::parse_record` ihn
// liefert (auch für die auditd-Textform).
const CONFIG_CHANGE_NAME: &str = "CONFIG_CHANGE";

// Das Token, mit dem der Kernel das Abschalten meldet
// (`audit_log_config_change`: `op=set audit_enabled=0 old=1 … res=1`).
const AUDIT_DISABLED_TOKEN: &str = "audit_enabled=0";

/// Zähler eines Durchlaufs oder, kumuliert, der ganzen Laufzeit.
///
/// # Description
/// Inhaltsfrei: nur Anzahlen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CycleStats {
    /// Gelesene Records.
    pub records: u64,
    /// Gesendete Anmeldeereignisse.
    pub sent: u64,
    /// Records, die keine Anmeldeart sind (`SYSCALL`, `USER_CMD`, …).
    pub skipped: u64,
    /// Records, die nicht parsebar oder als Anmeldeereignis unvollständig
    /// waren.
    pub malformed: u64,
    /// Lesevorgänge, bei denen der Kernel Records verworfen hat
    /// (`ENOBUFS`).
    pub overruns: u64,
    /// Datagramme, die größer als der Lesepuffer oder nicht rahmbar waren;
    /// ihr Record ist verloren.
    pub truncated: u64,
    /// Anmeldeereignisse, die das [`AuthBudget`] nicht gesendet hat.
    pub rate_limited: u64,
    /// `CONFIG_CHANGE`-Records, mit denen Auditing abgeschaltet wurde
    /// (unter `skipped` mitgezählt).
    pub audit_disabled: u64,
    /// Gesendete `SensorDegraded`-Meldungen (jeder Grund).
    pub degraded_reports: u64,
    /// Davon wegen eines schweigenden Audit-Stroms ([`SilenceWatch`]).
    pub silence_reports: u64,
}

impl CycleStats {
    /// Addiert `other` gesättigt auf diese Zähler.
    ///
    /// # Arguments
    /// - `other` (`CycleStats`): die Zähler eines weiteren Durchlaufs.
    pub fn absorb(&mut self, other: Self) {
        self.records = self.records.saturating_add(other.records);
        self.sent = self.sent.saturating_add(other.sent);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.malformed = self.malformed.saturating_add(other.malformed);
        self.overruns = self.overruns.saturating_add(other.overruns);
        self.truncated = self.truncated.saturating_add(other.truncated);
        self.rate_limited = self.rate_limited.saturating_add(other.rate_limited);
        self.audit_disabled = self.audit_disabled.saturating_add(other.audit_disabled);
        self.degraded_reports = self.degraded_reports.saturating_add(other.degraded_reports);
        self.silence_reports = self.silence_reports.saturating_add(other.silence_reports);
    }

    // Ob dieser Durchlauf etwas verloren, gedrosselt oder nicht verstanden
    // hat — dann ist eine (gedrosselte) `SensorDegraded`-Meldung fällig.
    fn has_loss(&self) -> bool {
        self.overruns > 0 || self.truncated > 0 || self.rate_limited > 0 || self.malformed > 0
    }

    // Ob der Audit-Strom in diesem Durchlauf gelebt hat: ein gelesener
    // Record, oder ein Verlust (dann gab es Records, nur zu viele).
    fn stream_alive(&self) -> bool {
        self.records > 0 || self.overruns > 0 || self.truncated > 0
    }
}

/// Das Ergebnis von [`convert_records`].
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    /// Die Anmeldeereignisse in Quellreihenfolge.
    pub events: Vec<SecurityEvent>,
    /// Die Zähler dieses Stapels (`sent` und `rate_limited` bleiben 0:
    /// gezählt wird beim Senden).
    pub stats: CycleStats,
}

/// Drosselt `SensorDegraded`-Meldungen auf eine je
/// [`DEGRADED_REPORT_INTERVAL`].
///
/// # Description
/// Siehe Moduldoku, „Verlust wird gemeldet, nicht verschwiegen": höchstens
/// ein `SensorDegraded` je [`DEGRADED_REPORT_INTERVAL`], egal wie viele
/// `ENOBUFS`, abgeschnittene Datagramme, gedrosselte oder fehlerhafte
/// Records dazwischen liegen und ob zwischendurch wieder Records ankommen.
/// Gezählt wird trotzdem alles ([`CycleStats`]). [`LoopState`] hält eine
/// Drossel für Verluste und eine zweite für abgeschaltetes Audit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverrunState {
    /// Zeitpunkt der letzten gesendeten Meldung; `None`, solange noch
    /// keine gesendet wurde.
    last_reported: Option<Timestamp>,
}

impl OverrunState {
    /// Ob jetzt gemeldet werden soll; merkt sich dann `now`.
    ///
    /// # Description
    /// Fällig ist eine Meldung, wenn noch keine gesendet wurde oder seit der
    /// letzten mindestens [`DEGRADED_REPORT_INTERVAL`] vergangen ist. Liegt
    /// `now` **vor** der letzten Meldung (die Uhr wurde zurückgestellt), ist
    /// sie ebenfalls fällig — sonst bliebe jede weitere Meldung bis zum
    /// Wiedererreichen der alten Zeit unterdrückt.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): die injizierte Zeit des Anlasses.
    ///
    /// # Returns
    /// `true`, wenn jetzt ein `SensorDegraded` gesendet werden soll.
    pub fn should_report(&mut self, now: Timestamp) -> bool {
        let due = self.last_reported.is_none_or(|last| {
            let elapsed = now.duration_since(last);
            elapsed.is_negative() || elapsed >= DEGRADED_REPORT_INTERVAL
        });
        if due {
            self.last_reported = Some(now);
        }
        due
    }
}

/// Das Anmeldebudget je [`AUTH_WINDOW`].
///
/// # Description
/// Feste Fenster ab dem ersten Ereignis, gemessen an der injizierten Zeit:
/// ein Fenster beginnt beim ersten Ereignis nach dem Ende des vorigen; je
/// Fenster lässt es höchstens [`MAX_AUTH_FAILURES_PER_WINDOW`]
/// Fehlschläge und [`MAX_AUTH_SUCCESSES_PER_WINDOW`] Erfolge zu. Liegt `now`
/// vor dem Fensterbeginn (die Uhr wurde zurückgestellt), beginnt ein neues
/// Fenster — sonst bliebe das Budget bis zum Wiedererreichen der alten Zeit
/// erschöpft.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthBudget {
    /// Beginn des laufenden Fensters; `None` vor dem ersten Ereignis.
    window_start: Option<Timestamp>,
    /// Im laufenden Fenster zugelassene Fehlschläge.
    failures: u32,
    /// Im laufenden Fenster zugelassene Erfolge.
    successes: u32,
}

impl AuthBudget {
    /// Ob ein Anmeldeereignis mit `outcome` jetzt gesendet werden darf;
    /// zählt es dann gegen das Budget.
    ///
    /// # Arguments
    /// - `outcome` (`harw_dod_signals::AuthOutcome`): das Ergebnis der
    ///   Anmeldung; bestimmt, welches der beiden Budgets gilt.
    /// - `now` (`jiff::Timestamp`): die injizierte Zeit.
    ///
    /// # Returns
    /// `true`, wenn das Ereignis gesendet werden soll; `false`, wenn sein
    /// Budget im laufenden Fenster erschöpft ist.
    pub fn admit(&mut self, outcome: AuthOutcome, now: Timestamp) -> bool {
        let new_window = self.window_start.is_none_or(|start| {
            let elapsed = now.duration_since(start);
            elapsed.is_negative() || elapsed >= AUTH_WINDOW
        });
        if new_window {
            *self = Self {
                window_start: Some(now),
                failures: 0,
                successes: 0,
            };
        }
        let (used, limit) = match outcome {
            AuthOutcome::Failure => (&mut self.failures, MAX_AUTH_FAILURES_PER_WINDOW),
            AuthOutcome::Success => (&mut self.successes, MAX_AUTH_SUCCESSES_PER_WINDOW),
        };
        if *used >= limit {
            return false;
        }
        *used = used.saturating_add(1);
        true
    }
}

/// Erkennt einen schweigenden Audit-Strom.
///
/// # Description
/// Siehe Moduldoku, „Abgeschaltetes Audit": misst die Zeit seit dem letzten
/// Lebenszeichen des Stroms (oder dem Start) an der injizierten Zeit und
/// meldet nach [`AUDIT_SILENCE_INTERVAL`] Stille; danach beginnt die Messung
/// neu, also höchstens eine Meldung je Intervall. Liegt `now` vor dem
/// Messbeginn (die Uhr wurde zurückgestellt), beginnt die Messung neu, statt
/// eine Stille zu melden, die es nicht gab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SilenceWatch {
    /// Beginn der laufenden Messung; `None` vor dem ersten Durchlauf.
    since: Option<Timestamp>,
}

impl SilenceWatch {
    /// Verbucht einen Durchlauf; meldet, ob jetzt Stille zu melden ist.
    ///
    /// # Arguments
    /// - `alive` (`bool`): ob der Strom in diesem Durchlauf gelebt hat
    ///   (ein Record oder ein Verlust).
    /// - `now` (`jiff::Timestamp`): die injizierte Zeit.
    ///
    /// # Returns
    /// `true`, wenn seit dem Messbeginn mindestens
    /// [`AUDIT_SILENCE_INTERVAL`] ohne Lebenszeichen vergangen ist.
    pub fn observe(&mut self, alive: bool, now: Timestamp) -> bool {
        let Some(since) = self.since.filter(|_| !alive) else {
            self.since = Some(now);
            return false;
        };
        let elapsed = now.duration_since(since);
        if elapsed.is_negative() || elapsed >= AUDIT_SILENCE_INTERVAL {
            self.since = Some(now);
            return !elapsed.is_negative();
        }
        false
    }
}

/// Der Zustand von [`run_once`] zwischen zwei Durchläufen.
///
/// # Description
/// Bündelt die beiden Meldedrosseln, das Anmeldebudget und den
/// Stille-Wächter; `Default` ist der Zustand beim Start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopState {
    /// Drossel für Verlust, gedrosselte und fehlerhafte Records.
    loss: OverrunState,
    /// Eigene Drossel für abgeschaltetes Audit.
    audit_off: OverrunState,
    /// Das Anmeldebudget.
    auth: AuthBudget,
    /// Der Stille-Wächter.
    silence: SilenceWatch,
}

/// Ob `raw` der `CONFIG_CHANGE`-Record ist, mit dem Auditing abgeschaltet
/// wurde.
///
/// # Description
/// Siehe Moduldoku, „Abgeschaltetes Audit": Recordart `CONFIG_CHANGE`
/// (aus dem `nlmsghdr`, sonst aus `type=`), ein eigenständiges Token
/// `audit_enabled=0` und ein erfolgreiches Ergebnis (`res=1`; ein gesperrtes
/// Audit lehnt mit `res=0` ab). Ein Netlink-Record eines anderen Typs wird
/// nie geparst.
///
/// # Arguments
/// - `raw` (`&harw_dod_netlink::RawRecord`): ein Record, der keine
///   Anmeldeart ist.
fn is_audit_disable_record(raw: &RawRecord) -> bool {
    if raw
        .msg_type()
        .is_some_and(|code| code != CONFIG_CHANGE_TYPE)
    {
        return false;
    }
    let disables = raw
        .as_str()
        .split(|c: char| c.is_ascii_whitespace())
        .any(|token| token == AUDIT_DISABLED_TOKEN);
    disables
        && parse_record(raw).is_ok_and(|fields| {
            fields.record_type == CONFIG_CHANGE_NAME && fields.success == Some(true)
        })
}

/// Wandelt einen Stapel Rohrecords in Anmeldeereignisse.
///
/// # Description
/// Je Record `harw_dod_authlog::auth_record_from_raw`: ein Anmeldeereignis
/// wird über `harw_dod_authlog::auth_record_to_event` zum `SecurityEvent`
/// mit `sensor`; jede andere Recordart zählt als `skipped` (ein
/// `CONFIG_CHANGE`, der Auditing abschaltet, zusätzlich als
/// `audit_disabled`), ein fehlerhafter Record als `malformed`. Kein Record
/// lässt den Stapel scheitern. Das Anmeldebudget wendet erst [`run_once`]
/// an.
///
/// # Arguments
/// - `sensor` (`&harw_types::SensorId`): die Kennung dieser Sonde.
/// - `records` (`&[harw_dod_netlink::RawRecord]`): der gelesene Stapel.
///
/// # Returns
/// Die Ereignisse und die Zähler.
///
/// # Concurrency
/// Reine Funktion.
#[must_use]
pub fn convert_records(sensor: &SensorId, records: &[RawRecord]) -> Converted {
    let mut events = Vec::new();
    let mut stats = CycleStats::default();
    for raw in records {
        stats.records = stats.records.saturating_add(1);
        match auth_record_from_raw(raw) {
            Ok(Some(record)) => events.push(auth_record_to_event(sensor, record)),
            Ok(None) => {
                stats.skipped = stats.skipped.saturating_add(1);
                if is_audit_disable_record(raw) {
                    stats.audit_disabled = stats.audit_disabled.saturating_add(1);
                }
            }
            // Inhaltsfrei (`MalformedSource`); gezählt, nicht geloggt.
            Err(_) => stats.malformed = stats.malformed.saturating_add(1),
        }
    }
    Converted { events, stats }
}

/// Baut eine `SensorDegraded`-Meldung dieser Sonde.
///
/// # Arguments
/// - `sensor` (`&harw_types::SensorId`): die Kennung dieser Sonde.
/// - `now` (`jiff::Timestamp`): der Zeitpunkt der Meldung.
fn degraded_event(sensor: &SensorId, now: Timestamp) -> SecurityEvent {
    SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: None,
        kind: EventKind::SensorDegraded {
            sensor: sensor.clone(),
        },
    }
}

/// Ein Durchlauf: einen Stapel lesen, wandeln, senden, Störungen melden.
///
/// # Description
/// Liest über `source` mit [`READ_TIMEOUT`] und wandelt die Records über
/// [`convert_records`]. Jedes Anmeldeereignis, das das [`AuthBudget`]
/// zulässt, wird einzeln gesendet; die übrigen zählen als `rate_limited`.
/// Ein `NetlinkError::Overrun` bzw. `NetlinkError::Truncated` wird gezählt.
/// Danach höchstens **ein** `SensorDegraded` mit `observed_at = now`, wenn
/// - der Durchlauf etwas verloren, gedrosselt oder nicht verstanden hat und
///   die Verlustdrossel es zulässt ([`OverrunState`]),
/// - ein `CONFIG_CHANGE` Auditing abgeschaltet hat und dessen eigene
///   Drossel es zulässt, oder
/// - der Strom [`AUDIT_SILENCE_INTERVAL`] lang geschwiegen hat
///   ([`SilenceWatch`]).
///
/// # Arguments
/// - `source` (`&dyn harw_dod_netlink::AuditSource`): die Audit-Quelle.
/// - `sensor` (`&harw_types::SensorId`): die Kennung dieser Sonde.
/// - `sink` (`&dyn EventSink`): die Push-only-Senke zum Sentinel.
/// - `state` (`&mut LoopState`): Drosseln, Budget und Stille-Wächter.
/// - `now` (`jiff::Timestamp`): injizierte Zeit für Budget, Drosseln und
///   eine `SensorDegraded`-Meldung.
///
/// # Returns
/// Die Zähler dieses Durchlaufs.
///
/// # Errors
/// [`ProbeError::AuditRead`] für jeden Lesefehler außer `Overrun` und
/// `Truncated`; jeder Fehler aus [`EventSink::send`] (dann sind die vorher
/// gesendeten Ereignisse dieses Stapels bereits beim Sentinel).
pub fn run_once(
    source: &dyn AuditSource,
    sensor: &SensorId,
    sink: &dyn EventSink,
    state: &mut LoopState,
    now: Timestamp,
) -> Result<CycleStats, ProbeError> {
    let (events, mut stats) = match source.read_records(READ_TIMEOUT) {
        Ok(records) => {
            let Converted { events, stats } = convert_records(sensor, &records);
            (events, stats)
        }
        Err(NetlinkError::Overrun) => (
            Vec::new(),
            CycleStats {
                overruns: 1,
                ..CycleStats::default()
            },
        ),
        Err(NetlinkError::Truncated) => (
            Vec::new(),
            CycleStats {
                truncated: 1,
                ..CycleStats::default()
            },
        ),
        Err(other) => return Err(ProbeError::AuditRead(other)),
    };

    for event in &events {
        let outcome = match &event.kind {
            EventKind::AuthEvent { outcome } => *outcome,
            // `convert_records` baut nur `AuthEvent`s; jede andere Art
            // zählte gegen das Budget, das ein Angreifer ohnehin erreicht.
            _ => AuthOutcome::Failure,
        };
        if state.auth.admit(outcome, now) {
            sink.send(event)?;
            stats.sent = stats.sent.saturating_add(1);
        } else {
            stats.rate_limited = stats.rate_limited.saturating_add(1);
        }
    }

    // Jede Drossel wird befragt (und rückt dabei vor), auch wenn eine
    // andere schon meldet; gesendet wird höchstens eine Meldung.
    let loss_due = stats.has_loss() && state.loss.should_report(now);
    let off_due = stats.audit_disabled > 0 && state.audit_off.should_report(now);
    let silence_due = state.silence.observe(stats.stream_alive(), now);
    if loss_due || off_due || silence_due {
        sink.send(&degraded_event(sensor, now))?;
        stats.degraded_reports = 1;
        if silence_due {
            stats.silence_reports = 1;
        }
    }
    Ok(stats)
}

/// Betreibt [`run_once`] in einer Endlosschleife.
///
/// # Description
/// `now` für Budget, Drosseln und eine `SensorDegraded`-Meldung kommt hier,
/// an der Binary-Kante, von `jiff::Timestamp::now`. Die kumulierten Zähler
/// gehen alle [`STATS_LOG_INTERVAL`] auf `info` ins Log (nur Anzahlen); eine
/// gesendete `SensorDegraded`-Meldung zusätzlich sofort auf `warn` (nur
/// Anzahlen, also höchstens einmal je Drosselabstand), jede gedrosselte
/// Störung auf `debug`.
///
/// # Arguments
/// - `source` (`&dyn harw_dod_netlink::AuditSource`): die Audit-Quelle.
/// - `sensor` (`&harw_types::SensorId`): die Kennung dieser Sonde.
/// - `sink` (`&dyn EventSink`): die Push-only-Senke zum Sentinel.
///
/// # Returns
/// Nur den Fehler, der die Schleife beendet hat — sie endet nie anders.
pub fn run_forever(
    source: &dyn AuditSource,
    sensor: &SensorId,
    sink: &dyn EventSink,
) -> ProbeError {
    let mut state = LoopState::default();
    let mut total = CycleStats::default();
    let mut last_log = Instant::now();

    loop {
        let stats = match run_once(source, sensor, sink, &mut state, Timestamp::now()) {
            Ok(stats) => stats,
            Err(err) => {
                log_totals(&total);
                return err;
            }
        };
        total.absorb(stats);
        log_cycle(&stats, &total);
        if last_log.elapsed() >= STATS_LOG_INTERVAL {
            log_totals(&total);
            last_log = Instant::now();
        }
    }
}

/// Loggt die Störungen eines Durchlaufs (nur Anzahlen).
///
/// # Arguments
/// - `stats` (`&CycleStats`): die Zähler dieses Durchlaufs.
/// - `total` (`&CycleStats`): die kumulierten Zähler.
fn log_cycle(stats: &CycleStats, total: &CycleStats) {
    if stats.audit_disabled > 0 {
        tracing::warn!(
            audit_disabled = total.audit_disabled,
            "kernel auditing was switched off; login records stop"
        );
    }
    if stats.silence_reports > 0 {
        tracing::warn!(
            silence_reports = total.silence_reports,
            "no audit record for the silence interval; kernel auditing may be off; reported as sensor degraded"
        );
    } else if stats.degraded_reports > 0 {
        tracing::warn!(
            overruns = total.overruns,
            truncated = total.truncated,
            rate_limited = total.rate_limited,
            malformed = total.malformed,
            degraded_reports = total.degraded_reports,
            "audit netlink probe lost, limited or rejected records; reported as sensor degraded"
        );
    } else if stats.has_loss() {
        tracing::debug!(
            overruns = total.overruns,
            truncated = total.truncated,
            rate_limited = total.rate_limited,
            malformed = total.malformed,
            "audit netlink probe lost, limited or rejected records; report throttled"
        );
    }
}

/// Loggt die kumulierten Zähler auf `info`.
fn log_totals(total: &CycleStats) {
    tracing::info!(
        records = total.records,
        sent = total.sent,
        skipped = total.skipped,
        malformed = total.malformed,
        overruns = total.overruns,
        truncated = total.truncated,
        rate_limited = total.rate_limited,
        audit_disabled = total.audit_disabled,
        degraded_reports = total.degraded_reports,
        silence_reports = total.silence_reports,
        "audit netlink probe statistics"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    use harw_dod_cap::SensorError;
    use harw_dod_netlink::{AuditSource, NetlinkError, RawRecord};
    use harw_dod_signals::{AuthOutcome, EventKind, SecurityEvent};
    use harw_types::SensorId;
    use jiff::{SignedDuration, Timestamp};

    use super::{
        AUDIT_SILENCE_INTERVAL, AUTH_WINDOW, AuthBudget, CycleStats, DEGRADED_REPORT_INTERVAL,
        LoopState, MAX_AUTH_FAILURES_PER_WINDOW, MAX_AUTH_SUCCESSES_PER_WINDOW, OverrunState,
        SilenceWatch, convert_records, run_once,
    };
    use crate::error::ProbeError;
    use crate::sink::EventSink;
    use crate::test_support::{TestError, TestResult, ctx, missing};

    const USER_LOGIN: &str = "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 ses=2 subj=unconfined msg='op=login id=1000 exe=\"/usr/sbin/sshd\" hostname=? addr=192.0.2.10 terminal=/dev/pts/0 res=success'";

    const USER_AUTH_FAILED: &str = "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 ses=2 subj=unconfined msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/bin/sudo\" hostname=? addr=? terminal=/dev/pts/0 res=failed'";

    const USER_AUTH_SUCCESS: &str = "audit(1699999999.457:459): pid=4301 uid=0 auid=4294967295 ses=4294967295 subj=unconfined msg='op=PAM:authentication grantors=pam_unix acct=\"alice\" exe=\"/usr/sbin/sshd\" hostname=192.0.2.10 addr=192.0.2.10 terminal=ssh res=success'";

    const USER_CMD: &str = "audit(1699999999.789:458): pid=4400 uid=1000 auid=1000 ses=2 subj=unconfined msg='cwd=\"/home/alice\" cmd=6C73202D6C61 exe=\"/usr/bin/sudo\" terminal=pts/0 res=success'";

    /// `auditctl -e 0` als Kernel-Record (`CONFIG_CHANGE`, 1305).
    const AUDIT_OFF: &str = "audit(1700000000.000:500): op=set audit_enabled=0 old=1 auid=0 ses=3 subj=unconfined res=1";

    /// `nlmsg_type` von `CONFIG_CHANGE`.
    const CONFIG_CHANGE: u16 = 1305;

    /// Eine Quelle, die eine feste Folge von Ergebnissen liefert und danach
    /// leere Stapel.
    struct ScriptedSource {
        script: Mutex<VecDeque<Result<Vec<RawRecord>, NetlinkError>>>,
    }

    impl ScriptedSource {
        fn new(script: Vec<Result<Vec<RawRecord>, NetlinkError>>) -> Self {
            Self {
                script: Mutex::new(script.into()),
            }
        }
    }

    impl AuditSource for ScriptedSource {
        fn read_records(&self, _timeout: Duration) -> Result<Vec<RawRecord>, NetlinkError> {
            let Ok(mut script) = self.script.lock() else {
                return Err(NetlinkError::from(SensorError::SourceUnavailable));
            };
            script.pop_front().unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    /// Eine Senke, die jedes Ereignis mitschreibt.
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<SecurityEvent>>,
    }

    impl RecordingSink {
        fn sent(&self) -> TestResult<Vec<SecurityEvent>> {
            self.sent
                .lock()
                .map(|sent| sent.to_vec())
                .map_err(|_| TestError::Unexpected("Senke vergiftet".into()))
        }
    }

    impl EventSink for RecordingSink {
        fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError> {
            let mut sent = self
                .sent
                .lock()
                .map_err(|_| ProbeError::SentinelSendFailed)?;
            sent.push(event.clone());
            Ok(())
        }
    }

    /// Eine Senke, deren Sentinel verschwunden ist.
    struct BrokenSink;

    impl EventSink for BrokenSink {
        fn send(&self, _event: &SecurityEvent) -> Result<(), ProbeError> {
            Err(ProbeError::SentinelSendFailed)
        }
    }

    fn sensor() -> SensorId {
        SensorId::from_str("probe-netlink-auth-0")
    }

    fn now() -> TestResult<Timestamp> {
        Timestamp::new(1_700_000_000, 0).map_err(ctx("gültiger Zeitstempel"))
    }

    /// `now()` plus `seconds` Sekunden.
    fn later(seconds: i64) -> TestResult<Timestamp> {
        now()?
            .checked_add(SignedDuration::from_secs(seconds))
            .map_err(ctx("Zeitstempel im Bereich"))
    }

    /// Die `SensorDegraded`-Ereignisse unter allem Gesendeten.
    fn degraded_events(sent: &[SecurityEvent]) -> Vec<&SecurityEvent> {
        sent.iter()
            .filter(|event| matches!(event.kind, EventKind::SensorDegraded { .. }))
            .collect()
    }

    /// Die Anmeldeergebnisse unter allem Gesendeten, in Sendereihenfolge.
    fn auth_outcomes(sent: &[SecurityEvent]) -> Vec<AuthOutcome> {
        sent.iter()
            .filter_map(|event| match event.kind {
                EventKind::AuthEvent { outcome } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    /// `count` Kopien desselben Netlink-Records.
    fn repeated(msg_type: u16, payload: &str, count: usize) -> Vec<RawRecord> {
        std::iter::repeat_n(RawRecord::from_netlink(msg_type, payload), count).collect()
    }

    /// Führt `run_once` für jeden Zeitpunkt aus und summiert die Zähler.
    fn run_at(
        source: &ScriptedSource,
        sink: &dyn EventSink,
        state: &mut LoopState,
        times: &[Timestamp],
    ) -> TestResult<CycleStats> {
        let mut total = CycleStats::default();
        for at in times {
            total.absorb(
                run_once(source, &sensor(), sink, state, *at)
                    .map_err(ctx("Durchlauf beendet die Sonde nicht"))?,
            );
        }
        Ok(total)
    }

    #[test]
    fn test_convert_records_maps_login_records_and_counts_the_rest() -> TestResult {
        let records = vec![
            RawRecord::from_netlink(1112, USER_LOGIN),
            RawRecord::from_netlink(1100, USER_AUTH_FAILED),
            RawRecord::from_netlink(1123, USER_CMD),
            RawRecord::from_netlink(1327, "audit(1699999999.789:458): proctitle=7375646F"),
            RawRecord::from_netlink(1100, "audit(notanumber.1:2): pid=1"),
            RawRecord::from_netlink(1112, "audit(1699999999.123:459): auid=abc"),
        ];
        let converted = convert_records(&sensor(), &records);

        assert_eq!(
            converted.stats,
            CycleStats {
                records: 6,
                sent: 0,
                skipped: 2,
                malformed: 2,
                overruns: 0,
                truncated: 0,
                rate_limited: 0,
                audit_disabled: 0,
                degraded_reports: 0,
                silence_reports: 0,
            }
        );
        let kinds: Vec<EventKind> = converted
            .events
            .iter()
            .map(|event| event.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::AuthEvent {
                    outcome: AuthOutcome::Success
                },
                EventKind::AuthEvent {
                    outcome: AuthOutcome::Failure
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn test_convert_records_event_carries_sensor_record_time_and_actor() -> TestResult {
        let converted = convert_records(&sensor(), &[RawRecord::from_netlink(1112, USER_LOGIN)]);
        let event = converted
            .events
            .first()
            .ok_or_else(missing("ein Ereignis"))?;
        assert_eq!(event.sensor, sensor());
        assert_eq!(event.observed_at.as_second(), 1_699_999_999);
        let actor = event.actor.as_ref().ok_or_else(missing("actor"))?;
        assert_eq!(actor.uid, 0);
        assert_eq!(actor.auid, Some(1000));
        assert_eq!(
            actor.process.as_ref().map(|process| process.pid),
            Some(4242)
        );
        Ok(())
    }

    #[test]
    fn test_convert_records_text_form_also_works() -> TestResult {
        let converted = convert_records(
            &sensor(),
            &[RawRecord::new(
                "type=USER_AUTH msg=audit(1699999999.456:457): pid=4300 uid=0 auid=1000 ses=2 msg='op=PAM:authentication acct=\"root\" res=failed'",
            )],
        );
        assert_eq!(converted.events.len(), 1);
        assert_eq!(converted.stats.malformed, 0);
        Ok(())
    }

    #[test]
    fn test_convert_records_events_never_carry_names_or_addresses() -> TestResult {
        let converted = convert_records(
            &sensor(),
            &[
                RawRecord::from_netlink(1112, USER_LOGIN),
                RawRecord::from_netlink(1100, USER_AUTH_FAILED),
            ],
        );
        for event in &converted.events {
            let json = serde_json::to_string(event).map_err(ctx("SecurityEvent kodiert"))?;
            for needle in ["root", "alice", "sshd", "sudo", "192.0.2.10", "pts", "PAM"] {
                assert!(!json.contains(needle), "Ereignis trägt {needle:?}: {json}");
            }
        }
        Ok(())
    }

    #[test]
    fn test_convert_records_detects_audit_switched_off() {
        let converted = convert_records(
            &sensor(),
            &[
                RawRecord::from_netlink(CONFIG_CHANGE, AUDIT_OFF),
                RawRecord::new(format!("type=CONFIG_CHANGE msg={AUDIT_OFF}")),
            ],
        );
        assert_eq!(converted.stats.audit_disabled, 2);
        assert_eq!(converted.stats.skipped, 2);
        assert_eq!(converted.stats.malformed, 0);
        assert!(converted.events.is_empty());
    }

    #[test]
    fn test_convert_records_ignores_other_config_changes_and_spoofed_text() {
        let converted = convert_records(
            &sensor(),
            &[
                // Eingeschaltet, nicht ausgeschaltet.
                RawRecord::from_netlink(
                    CONFIG_CHANGE,
                    "audit(1700000000.000:501): op=set audit_enabled=1 old=0 auid=0 ses=3 subj=unconfined res=1",
                ),
                // Gesperrtes Audit lehnt das Abschalten ab.
                RawRecord::from_netlink(
                    CONFIG_CHANGE,
                    "audit(1700000000.000:502): op=set audit_enabled=0 old=2 auid=0 ses=3 subj=unconfined res=0",
                ),
                // Anderer Schalter mit ähnlichem Namen.
                RawRecord::from_netlink(
                    CONFIG_CHANGE,
                    "audit(1700000000.000:503): op=set audit_enabled_x=0 old=1 auid=0 ses=3 res=1",
                ),
                // Userspace-Text eines `USER_CMD` kann das Token tragen,
                // aber nie den Typ 1305.
                RawRecord::from_netlink(
                    1123,
                    "audit(1700000000.000:504): pid=1 uid=1000 auid=1000 ses=2 msg='cmd=6C73 audit_enabled=0 res=1 terminal=pts/0 res=success'",
                ),
            ],
        );
        assert_eq!(converted.stats.audit_disabled, 0);
        assert_eq!(converted.stats.skipped, 4);
    }

    #[test]
    fn test_run_once_sends_every_login_event_in_order() -> TestResult {
        let source = ScriptedSource::new(vec![Ok(vec![
            RawRecord::from_netlink(1100, USER_AUTH_FAILED),
            RawRecord::from_netlink(1123, USER_CMD),
            RawRecord::from_netlink(1112, USER_LOGIN),
        ])]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let stats = run_once(&source, &sensor(), &sink, &mut state, now()?)
            .map_err(ctx("Durchlauf gelingt"))?;

        assert_eq!(stats.sent, 2);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.rate_limited, 0);
        assert_eq!(stats.degraded_reports, 0);
        assert_eq!(
            auth_outcomes(&sink.sent()?),
            vec![AuthOutcome::Failure, AuthOutcome::Success]
        );
        assert!(degraded_events(&sink.sent()?).is_empty());
        Ok(())
    }

    #[test]
    fn test_run_once_empty_read_sends_nothing() -> TestResult {
        let source = ScriptedSource::new(Vec::new());
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let stats = run_once(&source, &sensor(), &sink, &mut state, now()?)
            .map_err(ctx("Durchlauf gelingt"))?;
        assert_eq!(stats, CycleStats::default());
        assert!(sink.sent()?.is_empty());
        Ok(())
    }

    #[test]
    fn test_run_once_failure_flood_is_capped_and_reported_once() -> TestResult {
        // Eine SSH-Brute-Force: 1000 gescheiterte USER_AUTH in einem
        // Fenster ergeben höchstens das Budget an Ereignissen und genau ein
        // SensorDegraded.
        let source = ScriptedSource::new(vec![
            Ok(repeated(1100, USER_AUTH_FAILED, 600)),
            Ok(repeated(1100, USER_AUTH_FAILED, 400)),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let total = run_at(&source, &sink, &mut state, &[later(0)?, later(1)?])?;

        let sent = sink.sent()?;
        let outcomes = auth_outcomes(&sent);
        // Höchstens das Gesamtbudget von 32, hier nur Fehlschläge.
        assert!(outcomes.len() <= 32);
        assert_eq!(
            outcomes.len(),
            usize::try_from(MAX_AUTH_FAILURES_PER_WINDOW).map_err(ctx("Budget passt in usize"))?
        );
        assert_eq!(total.sent, u64::from(MAX_AUTH_FAILURES_PER_WINDOW));
        assert_eq!(
            total.rate_limited,
            1000_u64.saturating_sub(u64::from(MAX_AUTH_FAILURES_PER_WINDOW))
        );
        assert_eq!(total.degraded_reports, 1);
        let degraded = degraded_events(&sent);
        assert_eq!(degraded.len(), 1);
        let event = degraded.first().ok_or_else(missing("SensorDegraded"))?;
        assert_eq!(event.kind, EventKind::SensorDegraded { sensor: sensor() });
        assert_eq!(event.observed_at, later(0)?);
        // Das SensorDegraded folgt den gesendeten Anmeldeereignissen.
        assert!(matches!(
            sent.last().map(|event| &event.kind),
            Some(EventKind::SensorDegraded { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_run_once_next_window_sends_again() -> TestResult {
        let window = AUTH_WINDOW.as_secs();
        let source = ScriptedSource::new(vec![
            Ok(repeated(1100, USER_AUTH_FAILED, 100)),
            Ok(repeated(1100, USER_AUTH_FAILED, 100)),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let total = run_at(&source, &sink, &mut state, &[later(0)?, later(window)?])?;

        let per_window = u64::from(MAX_AUTH_FAILURES_PER_WINDOW);
        assert_eq!(total.sent, per_window.saturating_mul(2));
        assert_eq!(
            total.rate_limited,
            200_u64.saturating_sub(per_window.saturating_mul(2))
        );
        // Beide Fenster liegen in einem Meldeabstand: eine Meldung.
        assert_eq!(total.degraded_reports, 1);
        assert_eq!(degraded_events(&sink.sent()?).len(), 1);
        Ok(())
    }

    #[test]
    fn test_run_once_failure_flood_does_not_hide_a_success() -> TestResult {
        // Die Flut erschöpft nur das Fehlschlagsbudget; die gelungene
        // Anmeldung am Ende desselben Fensters geht durch.
        let mut records = repeated(1100, USER_AUTH_FAILED, 500);
        records.push(RawRecord::from_netlink(1100, USER_AUTH_SUCCESS));
        let source = ScriptedSource::new(vec![Ok(records)]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let total = run_at(&source, &sink, &mut state, &[now()?])?;

        let outcomes = auth_outcomes(&sink.sent()?);
        assert_eq!(outcomes.last(), Some(&AuthOutcome::Success));
        assert_eq!(
            total.sent,
            u64::from(MAX_AUTH_FAILURES_PER_WINDOW).saturating_add(1)
        );
        Ok(())
    }

    #[test]
    fn test_auth_budget_limits_each_outcome_per_window() -> TestResult {
        let mut budget = AuthBudget::default();
        let at = now()?;
        let admitted_failures = (0..100)
            .filter(|_| budget.admit(AuthOutcome::Failure, at))
            .count();
        let admitted_successes = (0..100)
            .filter(|_| budget.admit(AuthOutcome::Success, at))
            .count();
        assert_eq!(
            admitted_failures,
            usize::try_from(MAX_AUTH_FAILURES_PER_WINDOW).map_err(ctx("Budget passt in usize"))?
        );
        assert_eq!(
            admitted_successes,
            usize::try_from(MAX_AUTH_SUCCESSES_PER_WINDOW).map_err(ctx("Budget passt in usize"))?
        );
        // Zusammen das dokumentierte Gesamtbudget je Fenster.
        assert_eq!(
            MAX_AUTH_FAILURES_PER_WINDOW.saturating_add(MAX_AUTH_SUCCESSES_PER_WINDOW),
            32
        );

        // Kurz vor Fensterende weiter erschöpft, ab Fensterende frei.
        let window = AUTH_WINDOW.as_secs();
        assert!(!budget.admit(AuthOutcome::Failure, later(window.saturating_sub(1))?));
        assert!(budget.admit(AuthOutcome::Failure, later(window)?));

        // Die Uhr springt zurück: ein neues Fenster statt eines bis zur
        // alten Zeit erschöpften Budgets.
        let mut budget = AuthBudget::default();
        for _ in 0..MAX_AUTH_FAILURES_PER_WINDOW {
            assert!(budget.admit(AuthOutcome::Failure, later(3_600)?));
        }
        assert!(!budget.admit(AuthOutcome::Failure, later(3_600)?));
        assert!(budget.admit(AuthOutcome::Failure, later(0)?));
        Ok(())
    }

    #[test]
    fn test_run_once_malformed_login_record_is_reported_once_per_interval() -> TestResult {
        let malformed = RawRecord::from_netlink(1112, "audit(1699999999.123:459): auid=abc");
        let source = ScriptedSource::new(vec![Ok(vec![malformed.clone()]), Ok(vec![malformed])]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let first = run_at(&source, &sink, &mut state, &[later(0)?])?;
        assert_eq!(first.malformed, 1);
        assert_eq!(first.degraded_reports, 1);
        assert_eq!(degraded_events(&sink.sent()?).len(), 1);

        let second = run_at(&source, &sink, &mut state, &[later(10)?])?;
        assert_eq!(second.malformed, 1);
        assert_eq!(second.degraded_reports, 0);
        assert_eq!(degraded_events(&sink.sent()?).len(), 1);
        Ok(())
    }

    #[test]
    fn test_run_once_overrun_reports_sensor_degraded_once_within_the_interval() -> TestResult {
        let source = ScriptedSource::new(vec![
            Err(NetlinkError::Overrun),
            Err(NetlinkError::Overrun),
            Ok(vec![RawRecord::from_netlink(1112, USER_LOGIN)]),
            Err(NetlinkError::Overrun),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let at = now()?;

        let total = run_at(&source, &sink, &mut state, &[at, at, at, at])?;

        assert_eq!(total.overruns, 3);
        assert_eq!(total.degraded_reports, 1);
        assert_eq!(total.sent, 1);
        let sent = sink.sent()?;
        let degraded = degraded_events(&sent);
        assert_eq!(degraded.len(), 1);
        for event in degraded {
            assert_eq!(event.observed_at, at);
            assert_eq!(event.actor, None);
            assert_eq!(event.kind, EventKind::SensorDegraded { sensor: sensor() });
        }
        Ok(())
    }

    #[test]
    fn test_run_once_sustained_overload_alternating_with_records_reports_once() -> TestResult {
        // Dauerlast: der Kernel wechselt zwischen ENOBUFS und einer
        // geleerten, wieder lesbaren Warteschlange. Innerhalb eines
        // Meldeabstands darf das genau ein SensorDegraded ergeben — gezählt
        // wird trotzdem jeder Überlauf.
        let mut script = Vec::new();
        for _ in 0..20 {
            script.push(Err(NetlinkError::Overrun));
            script.push(Ok(vec![RawRecord::from_netlink(1123, USER_CMD)]));
        }
        let source = ScriptedSource::new(script);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        // Je Durchlauf eine Sekunde: alle 40 liegen in einem Abstand.
        let times = (0..40).map(later).collect::<TestResult<Vec<_>>>()?;
        let total = run_at(&source, &sink, &mut state, &times)?;

        assert_eq!(total.overruns, 20);
        assert_eq!(total.skipped, 20);
        assert_eq!(total.degraded_reports, 1);
        assert_eq!(degraded_events(&sink.sent()?).len(), 1);
        Ok(())
    }

    #[test]
    fn test_run_once_reports_again_after_the_interval() -> TestResult {
        let source = ScriptedSource::new(vec![
            Err(NetlinkError::Overrun),
            Err(NetlinkError::Overrun),
            Err(NetlinkError::Overrun),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let interval = DEGRADED_REPORT_INTERVAL.as_secs();

        let total = run_at(
            &source,
            &sink,
            &mut state,
            &[
                later(0)?,
                later(interval.saturating_sub(1))?,
                later(interval)?,
            ],
        )?;

        assert_eq!(total.overruns, 3);
        assert_eq!(total.degraded_reports, 2);
        let observed: Vec<Timestamp> = degraded_events(&sink.sent()?)
            .into_iter()
            .map(|event| event.observed_at)
            .collect();
        assert_eq!(observed, vec![later(0)?, later(interval)?]);
        Ok(())
    }

    #[test]
    fn test_run_once_truncated_datagram_is_counted_and_reported_like_an_overrun() -> TestResult {
        let source = ScriptedSource::new(vec![
            Err(NetlinkError::Truncated),
            Err(NetlinkError::Overrun),
            Err(NetlinkError::Truncated),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let at = now()?;

        let total = run_at(&source, &sink, &mut state, &[at, at, at])?;

        assert_eq!(total.truncated, 2);
        assert_eq!(total.overruns, 1);
        // Eine gemeinsame Drossel für beide Verlustarten.
        assert_eq!(total.degraded_reports, 1);
        let sent = sink.sent()?;
        let degraded = degraded_events(&sent);
        let event = degraded.first().ok_or_else(missing("SensorDegraded"))?;
        assert_eq!(event.kind, EventKind::SensorDegraded { sensor: sensor() });
        assert_eq!(event.observed_at, at);
        Ok(())
    }

    #[test]
    fn test_run_once_audit_switched_off_is_reported_despite_a_recent_loss_report() -> TestResult {
        // Erst eine Flut (Verlustmeldung), Sekunden später `auditctl -e 0`:
        // die Abschaltung hat ihre eigene Drossel und wird trotzdem gemeldet.
        let source = ScriptedSource::new(vec![
            Ok(repeated(1100, USER_AUTH_FAILED, 100)),
            Ok(vec![RawRecord::from_netlink(CONFIG_CHANGE, AUDIT_OFF)]),
            Ok(vec![RawRecord::from_netlink(CONFIG_CHANGE, AUDIT_OFF)]),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let total = run_at(
            &source,
            &sink,
            &mut state,
            &[later(0)?, later(2)?, later(3)?],
        )?;

        assert_eq!(total.audit_disabled, 2);
        // Flut und erste Abschaltung je eine Meldung; die zweite Abschaltung
        // liegt im Abstand der ersten.
        assert_eq!(total.degraded_reports, 2);
        let observed: Vec<Timestamp> = degraded_events(&sink.sent()?)
            .into_iter()
            .map(|event| event.observed_at)
            .collect();
        assert_eq!(observed, vec![later(0)?, later(2)?]);
        Ok(())
    }

    #[test]
    fn test_run_once_reports_a_silent_stream_once_per_interval() -> TestResult {
        // Auditing war nie an: kein einziger Record.
        let source = ScriptedSource::new(Vec::new());
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let silence = AUDIT_SILENCE_INTERVAL.as_secs();

        let total = run_at(
            &source,
            &sink,
            &mut state,
            &[
                later(0)?,
                later(silence.saturating_sub(1))?,
                later(silence)?,
                later(silence.saturating_add(1))?,
                later(silence.saturating_mul(2))?,
            ],
        )?;

        assert_eq!(total.silence_reports, 2);
        assert_eq!(total.degraded_reports, 2);
        let observed: Vec<Timestamp> = degraded_events(&sink.sent()?)
            .into_iter()
            .map(|event| event.observed_at)
            .collect();
        assert_eq!(
            observed,
            vec![later(silence)?, later(silence.saturating_mul(2))?]
        );
        Ok(())
    }

    #[test]
    fn test_run_once_any_record_or_loss_keeps_the_stream_alive() -> TestResult {
        let silence = AUDIT_SILENCE_INTERVAL.as_secs();
        let half = silence.saturating_div(2);
        let source = ScriptedSource::new(vec![
            Ok(Vec::new()),
            // Keine Anmeldung, aber ein Lebenszeichen.
            Ok(vec![RawRecord::from_netlink(1123, USER_CMD)]),
            Ok(Vec::new()),
            Err(NetlinkError::Overrun),
            Ok(Vec::new()),
        ]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();

        let total = run_at(
            &source,
            &sink,
            &mut state,
            &[
                later(0)?,
                later(half)?,
                later(silence)?,
                later(half.saturating_add(silence))?,
                later(silence.saturating_mul(2))?,
            ],
        )?;

        assert_eq!(total.silence_reports, 0);
        // Nur der Überlauf selbst wurde gemeldet.
        assert_eq!(total.degraded_reports, 1);
        Ok(())
    }

    #[test]
    fn test_silence_watch_clock_set_back_restarts_instead_of_reporting() -> TestResult {
        let silence = AUDIT_SILENCE_INTERVAL.as_secs();
        let mut watch = SilenceWatch::default();
        assert!(!watch.observe(false, later(silence)?));
        // Die Uhr springt zurück: keine Meldung, die Messung beginnt neu.
        assert!(!watch.observe(false, later(0)?));
        assert!(!watch.observe(false, later(silence.saturating_sub(1))?));
        assert!(watch.observe(false, later(silence)?));
        Ok(())
    }

    #[test]
    fn test_run_once_degraded_send_failure_is_returned() -> TestResult {
        let source = ScriptedSource::new(vec![Err(NetlinkError::Truncated)]);
        let mut state = LoopState::default();
        let Err(err) = run_once(&source, &sensor(), &BrokenSink, &mut state, now()?) else {
            return Err(TestError::Unexpected("SentinelSendFailed erwartet".into()));
        };
        assert!(matches!(err, ProbeError::SentinelSendFailed));
        Ok(())
    }

    #[test]
    fn test_run_once_other_read_errors_end_the_probe() -> TestResult {
        let source = ScriptedSource::new(vec![Err(NetlinkError::from(
            SensorError::SourceUnavailable,
        ))]);
        let sink = RecordingSink::default();
        let mut state = LoopState::default();
        let Err(err) = run_once(&source, &sensor(), &sink, &mut state, now()?) else {
            return Err(TestError::Unexpected("AuditRead erwartet".into()));
        };
        assert!(matches!(err, ProbeError::AuditRead(_)));
        Ok(())
    }

    #[test]
    fn test_run_once_send_failure_is_returned() -> TestResult {
        let source = ScriptedSource::new(vec![Ok(vec![RawRecord::from_netlink(1112, USER_LOGIN)])]);
        let mut state = LoopState::default();
        let Err(err) = run_once(&source, &sensor(), &BrokenSink, &mut state, now()?) else {
            return Err(TestError::Unexpected("SentinelSendFailed erwartet".into()));
        };
        assert!(matches!(err, ProbeError::SentinelSendFailed));
        Ok(())
    }

    #[test]
    fn test_overrun_state_reports_at_most_once_per_interval() -> TestResult {
        let interval = DEGRADED_REPORT_INTERVAL.as_secs();
        let mut state = OverrunState::default();
        assert!(state.should_report(later(0)?));
        assert!(!state.should_report(later(0)?));
        assert!(!state.should_report(later(interval.saturating_sub(1))?));
        assert!(state.should_report(later(interval)?));
        // Gemessen ab der letzten Meldung, nicht ab der ersten.
        assert!(!state.should_report(later(interval.saturating_add(1))?));
        assert!(state.should_report(later(interval.saturating_mul(2))?));
        Ok(())
    }

    #[test]
    fn test_overrun_state_clock_set_back_does_not_silence_reports() -> TestResult {
        let mut state = OverrunState::default();
        assert!(state.should_report(later(3_600)?));
        // Die Uhr springt eine Stunde zurück: die nächste Meldung ist fällig,
        // statt bis zum Wiedererreichen der alten Zeit zu schweigen.
        assert!(state.should_report(later(0)?));
        assert!(!state.should_report(later(1)?));
        Ok(())
    }

    #[test]
    fn test_cycle_stats_absorb_saturates() {
        let mut total = CycleStats {
            records: u64::MAX,
            ..CycleStats::default()
        };
        total.absorb(CycleStats {
            records: 1,
            sent: 2,
            truncated: 3,
            rate_limited: 4,
            audit_disabled: 5,
            silence_reports: 6,
            ..CycleStats::default()
        });
        assert_eq!(total.records, u64::MAX);
        assert_eq!(total.sent, 2);
        assert_eq!(total.truncated, 3);
        assert_eq!(total.rate_limited, 4);
        assert_eq!(total.audit_disabled, 5);
        assert_eq!(total.silence_reports, 6);
    }
}
