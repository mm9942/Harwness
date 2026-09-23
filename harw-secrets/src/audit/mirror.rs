//! Der Audit-Spiegel: den Kettenzustand aus [`crate::audit::chain`] auf einem
//! zweiten, hostexternen Weg sichtbar machen (Knoten AW7-04).
//!
//! # Warum ein Audit auf dem eigenen Host nicht genügt
//! [`chain::verify`](crate::audit::chain::AuditLog::verify) stellt einen
//! Kettenbruch zuverlässig fest — aber nur für den, der es aufruft, auf dem
//! Host, der auch das Protokoll führt. Wer diesen Host übernimmt, kann sowohl
//! den Log-Inhalt als auch das Ergebnis von `verify` fälschen oder das
//! Aufrufen ganz unterdrücken. Ein Audit, dessen einziger Zeuge der
//! möglicherweise kompromittierte Host selbst ist, beweist gegenüber diesem
//! Fall nichts. Ein Spiegel, der (a) fortlaufend Lebenszeichen nach außen
//! trägt und (b) einen erkannten Bruch über einen zweiten Weg meldet, macht
//! Stille nach einer Übernahme selbst beobachtbar.
//!
//! # Produktionsstatus von `chain::verify`
//! Vor diesem Knoten wurde [`crate::audit::chain::AuditLog::verify`] **nur**
//! aus einem Test heraus aufgerufen (`harw-secrets/src/store.rs`,
//! `store.audit_log().verify().is_ok()`). Kein Produktionsaufrufer existierte.
//! Ein Nullzähler auf diesem Pfad wäre also blind gewesen: er hätte dauerhaft
//! Null gezeigt, weil ihn nie ein echter Aufruf erreicht — nicht, weil nie
//! ein Bruch vorkam. Diese Datei behebt genau das, indem sie
//! [`ChainAuditMirror::verify_and_mirror`] als produktionsförmigen Aufrufer
//! von `verify` bereitstellt: er nimmt ein `AuditLog` und einen
//! hereingereichten Zeitpunkt entgegen, ruft `verify` auf und spiegelt sowohl
//! den laufenden Zustand als auch einen erkannten Bruch nach außen.
//!
//! **Was diese Datei nicht tut:** sie plant `verify_and_mirror` nicht
//! periodisch ein. Ein Scheduler gehört in den tatsächlichen Host-Prozess
//! (`harw-cli` / `harw-job-runtime`), beides außerhalb des Schreibbereichs
//! dieses Knotens (`harw-secrets`, `harw-channel*`). Bis ein solcher
//! Scheduler `verify_and_mirror` tatsächlich aufruft, bleibt der Nullzähler
//! [`ChainAuditMirror::chain_break_count`] technisch scharf (er zählt jeden
//! erreichten Aufruf korrekt), aber praktisch ungenutzt, solange niemand ihn
//! aufruft — das ist im Abschlussbericht dieses Knotens ausdrücklich
//! festgehalten, nicht verschwiegen.
//!
//! # Was „out of band" hier bedeutet — und wogegen es nicht schützt
//! [`MirrorTransport`] ist bewusst als Trait mit einer aufzeichnenden
//! Testimplementierung ([`RecordingMirrorTransport`]) gehalten: die einzige
//! vorgesehene reale Anbindung ist ein zweiter Netzwerkkanal (Telegram, siehe
//! `harw-channel-telegram-transport::mirror`), der über einen anderen Prozess
//! (Telegrams Server) und ein anderes Zugangsdatum (Bot-Token statt
//! Log-Datei-Zugriff) läuft als das, was den Host lokal kompromittiert. Das
//! schützt gegen: lokale Manipulation des Audit-Logs, eine unterdrückte
//! Log-Rotation, einen Prozess, der `verify` schlicht nie aufruft (dann bleibt
//! auch das Lebenszeichen aus — selbst das ist beobachtbar), oder einen
//! Angreifer, der nur Dateisystem- oder Prozesszugriff hat, aber nicht auch
//! die ausgehende Netzwerkverbindung kontrolliert.
//!
//! Es schützt **nicht** gegen einen Angreifer, der den gesamten Host
//! übernimmt **einschließlich** seiner Netzwerk-Uplink-Kontrolle (z. B.
//! DNS-Hijack, Firewall-Regeln, die den Bot-Traffic umleiten oder
//! unterdrücken) — dann bleibt zwar weiterhin *aus*, dass Lebenszeichen
//! ausbleiben, beobachtbar, aber nur für den, der aktiv das Ausbleiben von
//! Lebenszeichen überwacht (ein "Totmann-Schalter", den dieser Knoten nicht
//! selbst implementiert — er liefert nur den Sender, nicht den externen
//! Beobachter). Es schützt ebenfalls nicht gegen eine Kompromittierung des
//! Telegram-Bot-Tokens selbst oder des Empfänger-Chats.
//!
//! # Was übertragen wird — und was ausdrücklich nicht
//! Ein [`crate::audit::event::AuditEvent`] trägt `actor`, `action` und
//! `subjects` (z. B. `CgroupId`/`FindingId`-artige Bezüge in verwandten
//! Audit-Modellen) — das verrät die Struktur des überwachten Systems. Der
//! Spiegel überträgt **nichts davon**:
//!
//! - [`MirrorEntry`] (fortlaufendes Lebenszeichen): nur `event_count`, der
//!   `chain_head`-Hash (eine SHA-256-Prüfsumme, aus der sich kein Ereignis
//!   rekonstruieren lässt) und der hereingereichte Prüfzeitpunkt. Das genügt,
//!   damit ein externer Beobachter erkennt, dass die Kette weiterläuft (oder
//!   eben nicht mehr meldet) und dass sich `event_count` monoton fortsetzt —
//!   mehr braucht die reine Lebendigkeits-/Fortschrittsbeobachtung nicht.
//! - [`ChainBreakAlert`] (Bruchmeldung): nur `broken_at_index`,
//!   `total_events` und der Erkennungszeitpunkt. **Bewusst ohne** die
//!   erwarteten/gefundenen Hash-Werte aus
//!   [`crate::error::AuditError::ChainBroken`] — auch wenn ein SHA-256-Hash
//!   für sich genommen kein Protokollinhalt ist, könnte ein Angreifer, der
//!   den Bruch selbst verursacht hat, aus einer Hash-Bestätigung indirekt
//!   lernen, ob eine bestimmte Fälschung "angekommen" ist. Die Meldung sagt
//!   *dass* und *wo* (Index, Gesamtzahl, Zeitpunkt) gebrochen wurde — genug,
//!   um zu handeln —, nie *was* im betroffenen Ereignis stand.
//!
//! # Nebenläufigkeit
//! [`MirrorTransport`] verlangt nur `&self`; Implementierungen sorgen selbst
//! für innere Synchronisation. [`ChainAuditMirror`] hält seine Zähler in
//! `AtomicU64` und ist `Send + Sync`, sofern sein Transport es ist.
//!
//! # Fehler
//! Transportfehler sind [`crate::error::MirrorError`]. Ein fehlgeschlagener
//! Versand blockiert `verify_and_mirror` nicht und wird nicht still
//! verschluckt: er erhöht [`ChainAuditMirror::delivery_failure_count`].
//!
//! # Examples
//! ```
//! use harw_secrets::audit::chain::AuditLog;
//! use harw_secrets::audit::mirror::{ChainAuditMirror, RecordingMirrorTransport};
//! use harw_secrets::audit::event::Actor;
//! use jiff::Timestamp;
//!
//! let mut log = AuditLog::new();
//! log.append(Actor::System, "secret.access", Vec::new());
//!
//! let mirror = ChainAuditMirror::new(RecordingMirrorTransport::new());
//! mirror
//!     .verify_and_mirror(&log, Timestamp::now(), &harw_observe::NullSink)
//!     .expect("intact chain verifies");
//! assert_eq!(mirror.chain_break_count(), 0);
//! ```
//!
//! # Der Nullzähler `audit_chain_break`
//! [`ChainAuditMirror::chain_break_count`] ist ein internes `AtomicU64` —
//! beobachtbar nur, wer eine `&ChainAuditMirror`-Referenz hält.
//! [`ChainAuditMirror::verify_and_mirror`] meldet dieselbe Erhöhung
//! zusätzlich an den hereingereichten
//! [`harw_observe::TelemetrySink`] unter dem Namen
//! [`crate::audit::telemetry::AUDIT_CHAIN_BREAK`] — additiv, siehe dessen
//! Moduldoku für die Invariante, den erwarteten Wert null und die ehrliche
//! Einschränkung, was dieser Zähler nicht beobachten kann.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_observe::TelemetrySink;
use jiff::Timestamp;

use crate::audit::chain::AuditLog;
use crate::audit::telemetry::AUDIT_CHAIN_BREAK;
use crate::error::{AuditError, MirrorError, MirrorResult};

/// Ein fortlaufendes Lebenszeichen des Spiegels (§ "Was übertragen wird").
///
/// # Description
/// Trägt bewusst nur, was zur Fortschritts-/Lebendigkeitsbeobachtung nötig
/// ist: den aktuellen Kettenzähler, den aktuellen Kopf-Hash (eine reine
/// Prüfsumme) und den hereingereichten Prüfzeitpunkt. Kein `actor`, keine
/// `action`, keine `subjects` — diese Felder verlassen den Host nie über
/// diesen Pfad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorEntry {
    /// Anzahl der Ereignisse in der Kette zum Prüfzeitpunkt.
    pub event_count: u64,
    /// SHA-256-Kopf-Hash der Kette zum Prüfzeitpunkt.
    pub chain_head: [u8; 32],
    /// Zeitpunkt der Prüfung (hereingereicht, niemals `Timestamp::now()`).
    pub checked_at: Timestamp,
}

impl MirrorEntry {
    /// Rendert das Lebenszeichen als inhaltsfreie, für Menschen lesbare Zeile.
    ///
    /// # Returns
    /// Eine einzeilige Zeichenkette mit `event_count`, dem hex-kodierten
    /// `chain_head` und `checked_at` — nichts, was aus einem konkreten
    /// [`crate::audit::event::AuditEvent`] stammt.
    ///
    /// # Examples
    /// ```
    /// use harw_secrets::audit::mirror::MirrorEntry;
    /// use jiff::Timestamp;
    ///
    /// let entry = MirrorEntry {
    ///     event_count: 3,
    ///     chain_head: [0u8; 32],
    ///     checked_at: Timestamp::now(),
    /// };
    /// assert!(entry.to_report_line().starts_with("audit-mirror heartbeat"));
    /// ```
    #[must_use]
    pub fn to_report_line(&self) -> String {
        format!(
            "audit-mirror heartbeat: event_count={} chain_head={} checked_at={}",
            self.event_count,
            hex_encode(&self.chain_head),
            self.checked_at
        )
    }
}

/// Eine Kettenbruch-Meldung (§ "Was übertragen wird").
///
/// # Description
/// Trägt genug, um zu handeln (wann, welcher Bereich der Kette), aber keine
/// Hash-Werte und keinen Ereignisinhalt — siehe Moduldoku für die Begründung,
/// warum selbst die erwarteten/gefundenen Hashes aus
/// [`crate::error::AuditError::ChainBroken`] hier bewusst fehlen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainBreakAlert {
    /// Index des ersten Ereignisses, dessen `prev_hash` nicht passte.
    pub broken_at_index: u64,
    /// Gesamtzahl der Ereignisse in der Kette zum Erkennungszeitpunkt.
    pub total_events: u64,
    /// Zeitpunkt, zu dem der Bruch erkannt wurde (hereingereicht).
    pub detected_at: Timestamp,
}

impl ChainBreakAlert {
    /// Rendert die Bruchmeldung als inhaltsfreie, für Menschen lesbare Zeile.
    ///
    /// # Returns
    /// Eine einzeilige Zeichenkette mit `broken_at_index`, `total_events` und
    /// `detected_at` — ohne Hash-Werte oder Ereignisinhalt.
    ///
    /// # Examples
    /// ```
    /// use harw_secrets::audit::mirror::ChainBreakAlert;
    /// use jiff::Timestamp;
    ///
    /// let alert = ChainBreakAlert {
    ///     broken_at_index: 2,
    ///     total_events: 5,
    ///     detected_at: Timestamp::now(),
    /// };
    /// assert!(alert.to_report_line().contains("AUDIT CHAIN BREAK"));
    /// ```
    #[must_use]
    pub fn to_report_line(&self) -> String {
        format!(
            "AUDIT CHAIN BREAK detected at index {} (total_events={}) at {}",
            self.broken_at_index, self.total_events, self.detected_at
        )
    }
}

/// Hex-kodiert `bytes` ohne fremde Crate-Abhängigkeit (nur für Anzeigezwecke).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Ein zweiter, hostexterner Übertragungsweg für Lebenszeichen und
/// Bruchmeldungen (§ "Was `out of band` hier bedeutet").
///
/// # Description
/// Bewusst minimal und synchron gehalten, analog zu
/// [`harw_channel::ChannelAdapter`]: Implementierungen entscheiden selbst
/// über Transport, Wiederholung und Rate-Begrenzung. Ein Fehlschlag darf den
/// Host **nicht** blockieren — Aufrufer wie [`ChainAuditMirror`] behandeln
/// jeden `Err` als nicht-fatal, zählen ihn aber statt ihn stillschweigend zu
/// verwerfen.
pub trait MirrorTransport: Send + Sync {
    /// Sendet ein fortlaufendes Lebenszeichen.
    ///
    /// # Arguments
    /// - `entry` (`&MirrorEntry`): das zu sendende, inhaltsfreie Lebenszeichen.
    ///
    /// # Errors
    /// - [`MirrorError::EntryRejected`]: wenn der Transport das Lebenszeichen
    ///   nicht zustellen konnte.
    fn send_entry(&self, entry: &MirrorEntry) -> MirrorResult<()>;

    /// Sendet eine Kettenbruch-Meldung.
    ///
    /// # Arguments
    /// - `alert` (`&ChainBreakAlert`): die zu sendende, inhaltsfreie Meldung.
    ///
    /// # Errors
    /// - [`MirrorError::AlertRejected`]: wenn der Transport die Meldung nicht
    ///   zustellen konnte.
    fn send_break_alert(&self, alert: &ChainBreakAlert) -> MirrorResult<()>;
}

/// Aufzeichnende Testimplementierung von [`MirrorTransport`].
///
/// # Description
/// Sammelt jeden Aufruf in Aufrufreihenfolge hinter einem `Mutex`. Kein
/// echter Netzwerkzugriff — bestimmt für Tests dieses Crates und für
/// Konsumenten, die [`MirrorTransport`] gegen eine reale Anbindung
/// implementieren (z. B. `harw-channel-telegram-transport`), es aber ohne
/// Netzwerk testen wollen. Ein Konstruktor mit voreingestelltem Fehlschlag
/// ([`RecordingMirrorTransport::always_failing`]) simuliert einen
/// ausgefallenen Transport, ohne eine echte Verbindung herzustellen.
#[derive(Debug, Default)]
pub struct RecordingMirrorTransport {
    entries: Mutex<Vec<MirrorEntry>>,
    alerts: Mutex<Vec<ChainBreakAlert>>,
    fail: bool,
}

impl RecordingMirrorTransport {
    /// Eine leere Aufzeichnung, die jeden Versand entgegennimmt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Eine Aufzeichnung, die jeden Versand mit einem Fehler ablehnt — für
    /// Tests eines ausgefallenen Transports, ohne echtes Netzwerk.
    #[must_use]
    pub fn always_failing() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            alerts: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    /// Alle bisher aufgezeichneten Lebenszeichen, in Aufrufreihenfolge.
    ///
    /// # Panics
    /// Wenn der interne Mutex vergiftet ist (ein vorheriger Aufruf ist über
    /// einen Panic ausgestiegen) — für eine reine Testimplementierung
    /// akzeptabel.
    #[must_use]
    pub fn entries(&self) -> Vec<MirrorEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Alle bisher aufgezeichneten Bruchmeldungen, in Aufrufreihenfolge.
    ///
    /// # Panics
    /// Wenn der interne Mutex vergiftet ist (siehe [`Self::entries`]).
    #[must_use]
    pub fn alerts(&self) -> Vec<ChainBreakAlert> {
        self.alerts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl MirrorTransport for RecordingMirrorTransport {
    fn send_entry(&self, entry: &MirrorEntry) -> MirrorResult<()> {
        if self.fail {
            return Err(MirrorError::EntryRejected {
                reason: "test transport configured to always fail".to_owned(),
            });
        }
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(*entry);
        Ok(())
    }

    fn send_break_alert(&self, alert: &ChainBreakAlert) -> MirrorResult<()> {
        if self.fail {
            return Err(MirrorError::AlertRejected {
                reason: "test transport configured to always fail".to_owned(),
            });
        }
        self.alerts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(*alert);
        Ok(())
    }
}

/// Der produktionsförmige Aufrufer von [`AuditLog::verify`], der jedes
/// Ergebnis zusätzlich über einen [`MirrorTransport`] nach außen trägt.
///
/// # Description
/// Siehe Moduldoku für den Produktionsstatus: diese Struktur macht
/// `chain::verify` aufrufbar und beobachtbar, wird aber (Stand dieses
/// Knotens) noch von keinem Scheduler in `harw-cli`/`harw-job-runtime`
/// tatsächlich periodisch aufgerufen.
#[derive(Debug, Default)]
pub struct ChainAuditMirror<T> {
    transport: T,
    chain_break_count: AtomicU64,
    delivery_failure_count: AtomicU64,
}

impl<T: MirrorTransport> ChainAuditMirror<T> {
    /// Baut einen Spiegel um `transport`, beide Zähler bei Null.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            chain_break_count: AtomicU64::new(0),
            delivery_failure_count: AtomicU64::new(0),
        }
    }

    /// Der Nullzähler: wie oft `verify` seit dem Bau dieses Spiegels einen
    /// Kettenbruch festgestellt hat. Erwarteter Wert: 0.
    ///
    /// # Returns
    /// Die Anzahl bisher über diesen Spiegel erkannter Kettenbrüche.
    #[must_use]
    pub fn chain_break_count(&self) -> u64 {
        self.chain_break_count.load(Ordering::SeqCst)
    }

    /// Wie oft der Versand eines Lebenszeichens oder einer Bruchmeldung am
    /// Transport gescheitert ist. Ein Fehlschlag hier blockiert
    /// `verify_and_mirror` nicht, wird aber gezählt statt still verworfen.
    ///
    /// # Returns
    /// Die Anzahl bisher gezählter Zustellfehler.
    #[must_use]
    pub fn delivery_failure_count(&self) -> u64 {
        self.delivery_failure_count.load(Ordering::SeqCst)
    }

    /// Prüft `log` und spiegelt Zustand und Ergebnis nach außen (§4.3 Schritt 1
    /// plus Out-of-Band-Meldung).
    ///
    /// # Description
    /// Sendet zuerst ein [`MirrorEntry`]-Lebenszeichen (unabhängig vom
    /// Prüfergebnis), ruft dann [`AuditLog::verify`] auf. Bei einem
    /// festgestellten [`AuditError::ChainBroken`] erhöht sich
    /// [`Self::chain_break_count`], meldet
    /// [`crate::audit::telemetry::AUDIT_CHAIN_BREAK`] dieselbe Erhöhung an
    /// `sink` (§ Moduldoku „Der Nullzähler `audit_chain_break`") und eine
    /// [`ChainBreakAlert`] wird zusätzlich gesendet. Ein fehlschlagender
    /// Versand ist nicht fatal: er erhöht [`Self::delivery_failure_count`]
    /// und blockiert weder diesen Aufruf noch den Host.
    ///
    /// # Arguments
    /// - `log` (`&AuditLog`): die zu prüfende Kette.
    /// - `checked_at` (`Timestamp`): hereingereichter Prüfzeitpunkt (keine
    ///   Systemuhr).
    /// - `sink` (`&dyn TelemetrySink`): Ziel für die
    ///   [`crate::audit::telemetry::AUDIT_CHAIN_BREAK`]-Meldung bei einem
    ///   festgestellten Bruch; unbenutzt, solange die Kette intakt ist.
    ///
    /// # Returns
    /// `Ok(())`, wenn die Kette intakt ist.
    ///
    /// # Errors
    /// - [`AuditError::ChainBroken`]: wenn `verify` einen Bruch feststellt.
    ///   Der Fehler wird trotz gespiegelter Meldung an den Aufrufer
    ///   zurückgegeben, damit lokale Fehlerbehandlung unverändert greift.
    ///
    /// # Concurrency
    /// [`crate::audit::telemetry::AUDIT_CHAIN_BREAK`] ist ein `'static`
    /// [`harw_observe::NullCounter`] und aus beliebigen Threads gleichzeitig
    /// erhöhbar; `sink.record` muss laut [`TelemetrySink`]-Vertrag ebenfalls
    /// nebenläufig sicher sein.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    pub fn verify_and_mirror(
        &self,
        log: &AuditLog,
        checked_at: Timestamp,
        sink: &dyn TelemetrySink,
    ) -> Result<(), AuditError> {
        let entry = MirrorEntry {
            event_count: log.len() as u64,
            chain_head: log.chain_head(),
            checked_at,
        };
        if self.transport.send_entry(&entry).is_err() {
            self.delivery_failure_count.fetch_add(1, Ordering::SeqCst);
        }

        let result = log.verify();
        if let Err(AuditError::ChainBroken { index, .. }) = &result {
            self.chain_break_count.fetch_add(1, Ordering::SeqCst);
            AUDIT_CHAIN_BREAK.violated(sink, &[]);
            let alert = ChainBreakAlert {
                broken_at_index: *index,
                total_events: log.len() as u64,
                detected_at: checked_at,
            };
            if self.transport.send_break_alert(&alert).is_err() {
                self.delivery_failure_count.fetch_add(1, Ordering::SeqCst);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::event::{Actor, SubjectRef};
    use crate::audit::telemetry::AUDIT_COUNTER_LOCK;
    use crate::test_support::{TestResult, ctx};
    use harw_observe::NullSink;

    fn ts(seconds: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(seconds).map_err(ctx("valid test timestamp"))
    }

    /// Builds a two-event chain and then breaks it by tampering the second
    /// event's `prev_hash`, reassembling the log via the crate-internal
    /// test-only constructor so production code never exposes mutable
    /// internals.
    fn broken_log() -> AuditLog {
        let mut log = AuditLog::new();
        log.append(Actor::System, "secret.access", Vec::new());
        log.append(
            Actor::Operator("operator-jane".to_owned()),
            "secret.rotate",
            vec![SubjectRef::new("secret", "prod-token")],
        );

        let good_first = log.events()[0].clone();
        let mut tampered_second = log.events()[1].clone();
        tampered_second.prev_hash[0] ^= 0xFF;

        AuditLog::from_raw_events_for_test(vec![good_first, tampered_second])
    }

    #[test]
    fn intact_chain_reports_zero_breaks_and_records_one_heartbeat() -> TestResult {
        let mut log = AuditLog::new();
        log.append(Actor::System, "secret.access", Vec::new());
        let transport = RecordingMirrorTransport::new();
        let mirror = ChainAuditMirror::new(transport);

        let outcome = mirror.verify_and_mirror(&log, ts(100)?, &NullSink);

        assert!(outcome.is_ok());
        assert_eq!(mirror.chain_break_count(), 0);
        assert_eq!(mirror.transport.entries().len(), 1);
        assert!(mirror.transport.alerts().is_empty());
        Ok(())
    }

    #[test]
    fn broken_chain_is_detected_and_alert_is_sent_and_counter_moves_off_zero() -> TestResult {
        let _guard = AUDIT_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let log = broken_log();
        let transport = RecordingMirrorTransport::new();
        let mirror = ChainAuditMirror::new(transport);
        let before = AUDIT_CHAIN_BREAK.count();

        let outcome = mirror.verify_and_mirror(&log, ts(200)?, &NullSink);

        assert!(matches!(
            outcome,
            Err(AuditError::ChainBroken { index: 1, .. })
        ));
        assert_eq!(mirror.chain_break_count(), 1);
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before + 1);
        let alerts = mirror.transport.alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].broken_at_index, 1);
        assert_eq!(alerts[0].total_events, 2);
        Ok(())
    }

    #[test]
    fn break_alert_never_carries_actor_action_or_subject_content() -> TestResult {
        let _guard = AUDIT_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let log = broken_log();
        let transport = RecordingMirrorTransport::new();
        let mirror = ChainAuditMirror::new(transport);

        let _ = mirror.verify_and_mirror(&log, ts(300)?, &NullSink);

        let alerts = mirror.transport.alerts();
        let rendered = alerts[0].to_report_line();
        assert!(!rendered.contains("operator-jane"));
        assert!(!rendered.contains("secret.rotate"));
        assert!(!rendered.contains("prod-token"));
        assert!(rendered.contains("AUDIT CHAIN BREAK"));
        assert!(rendered.contains("index 1"));
        Ok(())
    }

    #[test]
    fn heartbeat_never_carries_actor_action_or_subject_content() -> TestResult {
        let mut log = AuditLog::new();
        log.append(
            Actor::Operator("operator-jane".to_owned()),
            "secret.rotate",
            vec![SubjectRef::new("secret", "prod-token")],
        );
        let transport = RecordingMirrorTransport::new();
        let mirror = ChainAuditMirror::new(transport);

        mirror.verify_and_mirror(&log, ts(400)?, &NullSink)?;

        let entries = mirror.transport.entries();
        let rendered = entries[0].to_report_line();
        assert!(!rendered.contains("operator-jane"));
        assert!(!rendered.contains("secret.rotate"));
        assert!(!rendered.contains("prod-token"));
        assert!(rendered.contains("event_count=1"));
        Ok(())
    }

    #[test]
    fn failed_transport_does_not_hang_and_is_not_silently_dropped() -> TestResult {
        let mut log = AuditLog::new();
        log.append(Actor::System, "secret.access", Vec::new());
        let mirror = ChainAuditMirror::new(RecordingMirrorTransport::always_failing());

        let outcome = mirror.verify_and_mirror(&log, ts(500)?, &NullSink);

        assert!(outcome.is_ok());
        assert_eq!(mirror.delivery_failure_count(), 1);
        Ok(())
    }

    #[test]
    fn failed_transport_on_a_broken_chain_still_reports_the_break_locally() -> TestResult {
        let _guard = AUDIT_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let log = broken_log();
        let mirror = ChainAuditMirror::new(RecordingMirrorTransport::always_failing());
        let before = AUDIT_CHAIN_BREAK.count();

        let outcome = mirror.verify_and_mirror(&log, ts(600)?, &NullSink);

        assert!(matches!(outcome, Err(AuditError::ChainBroken { .. })));
        assert_eq!(mirror.chain_break_count(), 1);
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before + 1);
        // One failure for the heartbeat, one for the alert.
        assert_eq!(mirror.delivery_failure_count(), 2);
        Ok(())
    }
}
