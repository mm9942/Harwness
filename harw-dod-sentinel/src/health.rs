//! Der Degradationsautomat: ein `SensorHealth` je Sensor (Knoten AW2-18).
//!
//! # Der Automat als Zustandsdiagramm (Prosa)
//! Drei Zustände: [`SensorHealth::Bound`] (liefert), [`SensorHealth::Retrying`]
//! (vorübergehender Fehler, ein weiterer Versuch ist vorgesehen) und
//! [`SensorHealth::Degraded`] (aufgegeben, wird nicht mehr abgerufen). Die
//! Übergänge, alle über [`SensorHealth::advance`]:
//!
//! - `Bound` **+ Erfolg →** `Bound`.
//! - `Bound` **+ Fehler mit [`Permanence::Transient`] →** `Retrying { failures: 1, .. }`.
//! - `Bound` **+ Fehler mit [`Permanence::Permanent`] →** `Degraded` — sofort,
//!   ohne einen einzigen Zwischenversuch.
//! - `Retrying` **+ Erfolg →** `Bound` (der Fehlerzähler wird verworfen, nicht
//!   nur zurückgesetzt: ein erfolgreicher Abruf ist ein vollständiger Neustart
//!   der Beobachtung, kein Punktestand, der sich langsam erholt).
//! - `Retrying` **+ Fehler mit `Transient`, Obergrenze noch nicht erreicht →**
//!   `Retrying { failures: failures + 1, .. }` mit einem neuen
//!   `next_attempt`.
//! - `Retrying` **+ Fehler mit `Transient`, Obergrenze erreicht →**
//!   `Degraded { reason: DegradeReason::RetriesExhausted { .. } }`.
//! - `Retrying` **+ Fehler mit `Permanent` →** `Degraded { reason:
//!   DegradeReason::Permanent }` — sofort, unabhängig vom bisherigen
//!   Fehlerzähler.
//! - `Degraded` **+ irgendein Ergebnis →** `Degraded` (unverändert). Terminal
//!   innerhalb eines Prozesslaufs — siehe Abschnitt "Der Rückweg aus
//!   `Degraded`" unten. In der Praxis ruft [`crate::Sentinel::poll_all`]
//!   einen bereits `Degraded`-Sensor gar nicht erst auf; `advance` bleibt
//!   trotzdem für jeden Eingabezustand total, damit der Typ selbst die
//!   Terminal-Eigenschaft erzwingt, statt sich auf eine
//!   Aufrufer-Konvention zu verlassen, die irgendwo vergessen werden könnte.
//!
//! Der Automat bekommt `now` **injiziert** ([`SensorHealth::advance`],
//! [`SensorHealth::is_due`]) und liest nie die Systemuhr selbst — sonst wäre
//! er gegen kein Fixture mehr deterministisch prüfbar (vgl.
//! `harw_dod_signals::sensor`-Moduldoku, dieselbe Regel für [`crate::Sensor::poll`]
//! selbst).
//!
//! # Warum `Permanence::Permanent` jeden Fehlerzähler überspringt
//! [`harw_dod_cap::error::SensorError::permanence`] dokumentiert
//! `Permanent` als „ein erneuter Versuch wird mit an Sicherheit grenzender
//! Wahrscheinlichkeit wieder scheitern". Ein Fehlerzähler existiert nur, um
//! zu entscheiden, *wie viele* aussichtsreiche Versuche man einem Sensor vor
//! der Abmeldung noch gibt — bei einem Fehler, der per Definition aussichtslos
//! ist, gibt es nichts abzuwarten. Zählen und dann doch sofort abmelden wäre
//! nur eine langsamere Art, dasselbe zu tun.
//!
//! # Der Rückweg aus `Degraded`
//! **Es gibt innerhalb eines laufenden Prozesses keinen.** Das ist eine
//! bewusste Entscheidung, keine Lücke:
//!
//! - Ein `Degraded { reason: DegradeReason::Permanent }` entstand aus einem
//!   Fehler, der laut [`harw_dod_cap::error::SensorError::permanence`] eine
//!   Eigenschaft *dieses Hosts* beschreibt (fehlender Kernel-Pfad, Bereich
//!   außerhalb der Konfiguration) — ein Zustand, der sich innerhalb eines
//!   laufenden Sensor-Laufs nicht ändert. Ein zeitgesteuerter erneuter
//!   Versuch würde denselben aussichtslosen Fehler in fester Kadenz
//!   wiederholen und nichts gewinnen außer zusätzlicher Last.
//! - Ein `Degraded { reason: DegradeReason::RetriesExhausted { .. } }`
//!   entstand aus einer Serie von Fehlern, die über die gesamte konfigurierte
//!   Rückversuchsspanne ([`RetryPolicy`]) hinweg nicht abgeklungen ist. Ein
//!   Sensor, der genau an diesem Punkt von selbst wieder zu liefern beginnt,
//!   flackert — und ein Automat, der auf Flackern mit `Degraded → Bound →
//!   Degraded → …` reagiert, erzeugt bei jedem Wechsel ein neues
//!   `SensorDegraded`-Ereignis. Das widerspricht dem Zweck dieses Ereignisses:
//!   es soll eine seltene, ernstzunehmende Meldung sein („dieser Sensor ist
//!   abgemeldet, verlasst euch nicht mehr auf ihn"), keine, die bei jedem
//!   kurzen Wackelkontakt erneut auftaucht und damit ihre eigene Bedeutung
//!   verwässert.
//!
//! Die praktische Konsequenz: **ein Neustart des Sentinel-Prozesses** ist der
//! einzige vorgesehene Rückweg. Ein Neustart baut jeden Sensor mit frischem
//! [`SensorHealth::Bound`] neu auf (siehe [`crate::Sentinel::new`]) und ist
//! damit ein expliziter, im Betrieb sichtbarer Vorgang (Prozess-Exit-Code,
//! Neustart-Metrik der Betriebsumgebung) statt einer stillen internen
//! Zustandsänderung, die niemand bemerkt. Ein Automat ohne Rückweg ist nicht
//! immer richtig — hier ist es die sicherere Wahl, weil die Alternative
//! entweder aussichtslose Arbeit wiederholt oder die eine Meldung entwertet,
//! auf die sich dieser ganze Automat stützt.
//!
//! # Nebenläufigkeit
//! [`SensorHealth`], [`DegradeReason`] und [`RetryPolicy`] sind reine,
//! unveränderliche Werttypen ohne Interior Mutability: `Send + Sync`
//! automatisch. [`SensorHealth::advance`] ist eine reine Funktion (`&self ->
//! Self`, kein geteilter Zustand), sicher aus jedem Thread aufrufbar.
//!
//! # Fehler
//! Keine eigenen — der Automat selbst ist total und ohne fehlbaren Pfad.
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::Permanence;
//! use harw_dod_sentinel::health::{RetryPolicy, SensorHealth};
//!
//! let policy = RetryPolicy::default();
//! let bound = SensorHealth::Bound;
//!
//! // Ein dauerhafter Fehler degradiert sofort, ohne Zwischenversuch.
//! let degraded = bound.advance(Err(Permanence::Permanent), jiff::Timestamp::UNIX_EPOCH, policy);
//! assert!(degraded.is_degraded());
//! ```

use jiff::{SignedDuration, Timestamp};

/// Warum ein Sensor aufgegeben wurde.
///
/// # Description
/// Trägt genug Kontext, um die beiden in [`SensorHealth`] dokumentierten
/// Degradationspfade auseinanderzuhalten, ohne den ursprünglichen
/// [`harw_dod_cap::error::SensorError`] selbst zu speichern (der weder
/// `Clone` noch `PartialEq` ableitet und inhaltsfrei bleiben soll, siehe
/// dortige Moduldoku) — für die Unterscheidung „sofort aussichtslos" versus
/// „Rückversuche erschöpft" reicht diese kleinere, wertgleiche
/// Zusammenfassung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    /// Sofort abgemeldet: der zuletzt beobachtete Fehler hatte
    /// [`harw_dod_cap::Permanence::Permanent`].
    Permanent,
    /// Nach Erreichen der in [`RetryPolicy::max_retries`] konfigurierten
    /// Obergrenze abgemeldet.
    RetriesExhausted {
        /// Anzahl der aufeinanderfolgenden Fehlversuche, die zur Abmeldung
        /// führten (gleich [`RetryPolicy::max_retries`] zum Zeitpunkt der
        /// Abmeldung).
        attempts: u32,
    },
}

impl std::fmt::Display for DegradeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Permanent => f.write_str("permanent sensor error"),
            Self::RetriesExhausted { attempts } => {
                write!(f, "retry budget exhausted after {attempts} attempt(s)")
            }
        }
    }
}

/// Wie oft und in welchem Abstand ein Sensor nach einem vorübergehenden
/// Fehler erneut versucht wird, bevor er als [`SensorHealth::Degraded`]
/// aufgegeben wird.
///
/// # Description
/// Konfigurierbar, weil die richtige Wahl von der tatsächlichen Abrufkadenz
/// abhängt, die dieses Crate nicht kennt (sie gehört dem Binary, Knoten
/// AW2-19). [`Self::default`] liefert eine für Entwicklung und Tests
/// vernünftige Voreinstellung:
///
/// - `max_retries = 5`: genug, um eine kurze Serie vorübergehender Störungen
///   (ein flüchtiger `Io`-Fehler, eine `/proc`-Momentaufnahme mit
///   `MalformedSource` mitten in einer Kernel-internen Aktualisierung) zu
///   überstehen, ohne einen tatsächlich ausgefallenen Sensor unbegrenzt lange
///   im Zustand `Retrying` zu belassen.
/// - `backoff = 30s`: bei ausgeschöpfter Obergrenze vergehen damit rund
///   zweieinhalb Minuten durchgehenden Scheiterns, bevor die Abmeldung
///   erfolgt — kurz genug, dass das `SensorDegraded`-Ereignis zeitnah
///   erscheint, lang genug, dass eine einzelne kurze Störung nicht sofort als
///   Ausfall gewertet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Anzahl aufeinanderfolgender `Transient`-Fehlversuche, ab der ein
    /// Sensor als [`SensorHealth::Degraded`] aufgegeben wird.
    pub max_retries: u32,
    /// Wartezeit zwischen einem Fehlversuch und dem nächsten vorgesehenen
    /// Versuch ([`SensorHealth::Retrying::next_attempt`]).
    pub backoff: SignedDuration,
}

impl RetryPolicy {
    /// Voreingestellte Obergrenze aufeinanderfolgender Fehlversuche.
    pub const DEFAULT_MAX_RETRIES: u32 = 5;
    /// Voreingestellter Abstand zwischen zwei Versuchen, in Sekunden.
    pub const DEFAULT_BACKOFF_SECS: i64 = 30;

    /// Baut eine Rückversuchsrichtlinie aus expliziten Werten.
    ///
    /// # Arguments
    /// - `max_retries` (`u32`): siehe [`Self::max_retries`].
    /// - `backoff` (`jiff::SignedDuration`): siehe [`Self::backoff`].
    ///
    /// # Returns
    /// Eine `RetryPolicy` mit genau diesen Werten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::health::RetryPolicy;
    ///
    /// let policy = RetryPolicy::new(3, jiff::SignedDuration::from_secs(10));
    /// assert_eq!(policy.max_retries, 3);
    /// ```
    #[must_use]
    pub const fn new(max_retries: u32, backoff: SignedDuration) -> Self {
        Self {
            max_retries,
            backoff,
        }
    }
}

impl Default for RetryPolicy {
    /// Siehe Typ-Doku für die Begründung der gewählten Voreinstellung
    /// (`max_retries = 5`, `backoff = 30s`).
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_MAX_RETRIES,
            SignedDuration::from_secs(Self::DEFAULT_BACKOFF_SECS),
        )
    }
}

/// Der Gesundheitszustand eines einzelnen Sensors.
///
/// # Description
/// Siehe Moduldoku für das vollständige Zustandsdiagramm und die Begründung
/// des fehlenden Rückwegs aus [`Self::Degraded`].
#[derive(Debug, Clone, PartialEq)]
pub enum SensorHealth {
    /// Der Sensor liefert.
    Bound,
    /// Ein vorübergehender Fehler; es wird erneut versucht.
    Retrying {
        /// Anzahl aufeinanderfolgender Fehlversuche bislang.
        failures: u32,
        /// Injizierter Zeitpunkt, ab dem der nächste Versuch fällig ist.
        /// [`crate::Sentinel::poll_all`] ruft den Sensor vor diesem
        /// Zeitpunkt nicht erneut auf (siehe [`Self::is_due`]).
        next_attempt: Timestamp,
    },
    /// Aufgegeben. Der Sensor wird nicht mehr abgerufen.
    Degraded {
        /// Warum die Abmeldung erfolgte.
        reason: DegradeReason,
    },
}

impl SensorHealth {
    /// Ob dieser Zustand `Degraded` ist.
    ///
    /// # Returns
    /// `true` genau dann, wenn `self` [`Self::Degraded`] ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::health::SensorHealth;
    ///
    /// assert!(!SensorHealth::Bound.is_degraded());
    /// ```
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded { .. })
    }

    /// Ob dieser Sensor zum Zeitpunkt `now` abgerufen werden darf.
    ///
    /// # Description
    /// `Bound` ist immer fällig; `Degraded` ist nie fällig (terminal, siehe
    /// Moduldoku); `Retrying` ist erst ab seinem `next_attempt` fällig — das
    /// setzt die in [`RetryPolicy::backoff`] konfigurierte Wartezeit
    /// tatsächlich durch, statt sie nur als unbenutztes Feld mitzuführen.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit.
    ///
    /// # Returns
    /// `true`, wenn [`crate::Sentinel::poll_all`] diesen Sensor zu `now`
    /// abrufen soll.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::health::SensorHealth;
    /// use jiff::Timestamp;
    ///
    /// let health = SensorHealth::Retrying {
    ///     failures: 1,
    ///     next_attempt: Timestamp::UNIX_EPOCH
    ///         .checked_add(jiff::SignedDuration::from_secs(30))
    ///         .expect("30s from the epoch is in range"),
    /// };
    /// assert!(!health.is_due(Timestamp::UNIX_EPOCH));
    /// ```
    #[must_use]
    pub fn is_due(&self, now: Timestamp) -> bool {
        match self {
            Self::Bound => true,
            Self::Retrying { next_attempt, .. } => now >= *next_attempt,
            Self::Degraded { .. } => false,
        }
    }

    /// Wertet ein Abrufergebnis aus und liefert den nächsten Zustand.
    ///
    /// # Description
    /// Reine Zustandsübergangsfunktion; siehe Moduldoku für das vollständige
    /// Diagramm. Ist `self` bereits [`Self::Degraded`], ist der Rückgabewert
    /// unabhängig von `outcome` wieder derselbe `Degraded`-Zustand — der Typ
    /// erzwingt die Terminal-Eigenschaft damit selbst, statt sich auf eine
    /// Aufrufer-Konvention zu verlassen (in der Praxis ruft
    /// [`crate::Sentinel::poll_all`] `advance` auf einem bereits
    /// `Degraded`-Sensor gar nicht erst auf).
    ///
    /// # Arguments
    /// - `outcome` (`Result<(), harw_dod_cap::Permanence>`): `Ok(())` für
    ///   einen erfolgreichen Abruf, `Err(permanence)` für einen
    ///   fehlgeschlagenen — mit der [`harw_dod_cap::error::SensorError::permanence`]
    ///   des dabei aufgetretenen Fehlers.
    /// - `now` (`jiff::Timestamp`): injizierte Zeit; bestimmt ein neues
    ///   [`Self::Retrying::next_attempt`] als `now + policy.backoff`.
    /// - `policy` (`RetryPolicy`): Obergrenze und Wartezeit für
    ///   Rückversuche.
    ///
    /// # Returns
    /// Den Folgezustand.
    ///
    /// # Concurrency
    /// Reine Funktion ohne geteilten Zustand.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Permanence;
    /// use harw_dod_sentinel::health::{RetryPolicy, SensorHealth};
    /// use jiff::Timestamp;
    ///
    /// let policy = RetryPolicy::default();
    /// let next = SensorHealth::Bound.advance(Err(Permanence::Transient), Timestamp::UNIX_EPOCH, policy);
    /// assert!(matches!(next, SensorHealth::Retrying { failures: 1, .. }));
    /// ```
    #[must_use]
    pub fn advance(
        &self,
        outcome: Result<(), harw_dod_cap::Permanence>,
        now: Timestamp,
        policy: RetryPolicy,
    ) -> Self {
        if self.is_degraded() {
            return self.clone();
        }

        match outcome {
            Ok(()) => Self::Bound,
            Err(harw_dod_cap::Permanence::Permanent) => Self::Degraded {
                reason: DegradeReason::Permanent,
            },
            Err(harw_dod_cap::Permanence::Transient) => {
                let failures = match self {
                    Self::Retrying { failures, .. } => failures.saturating_add(1),
                    Self::Bound | Self::Degraded { .. } => 1,
                };

                if failures >= policy.max_retries {
                    Self::Degraded {
                        reason: DegradeReason::RetriesExhausted { attempts: failures },
                    }
                } else {
                    // Saturiert statt zu paniken, falls `now` bereits am
                    // äußersten Rand des darstellbaren Zeitraums liegt (siehe
                    // `jiff::Timestamp::MAX`) — ein praktisch nie erreichter
                    // Randfall, der aber nicht zum Absturz führen darf: kein
                    // `unwrap()`/`expect()` außerhalb von Tests (Harte Regeln).
                    let next_attempt = now.checked_add(policy.backoff).unwrap_or(Timestamp::MAX);
                    Self::Retrying {
                        failures,
                        next_attempt,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DegradeReason, RetryPolicy, SensorHealth};
    use harw_dod_cap::Permanence;
    use jiff::{SignedDuration, Timestamp};

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy::new(max_retries, SignedDuration::from_secs(30))
    }

    #[test]
    fn test_bound_stays_bound_on_success() {
        let next = SensorHealth::Bound.advance(Ok(()), Timestamp::UNIX_EPOCH, policy(5));
        assert_eq!(next, SensorHealth::Bound);
    }

    #[test]
    fn test_bound_transitions_to_retrying_on_transient_error() {
        let next = SensorHealth::Bound.advance(
            Err(Permanence::Transient),
            Timestamp::UNIX_EPOCH,
            policy(5),
        );
        assert!(matches!(next, SensorHealth::Retrying { failures: 1, .. }));
    }

    #[test]
    fn test_bound_degrades_immediately_on_permanent_error_without_retry() {
        let next = SensorHealth::Bound.advance(
            Err(Permanence::Permanent),
            Timestamp::UNIX_EPOCH,
            policy(5),
        );
        assert_eq!(
            next,
            SensorHealth::Degraded {
                reason: DegradeReason::Permanent
            }
        );
    }

    #[test]
    fn test_retrying_degrades_immediately_on_permanent_error_regardless_of_failure_count() {
        let retrying = SensorHealth::Retrying {
            failures: 1,
            next_attempt: Timestamp::UNIX_EPOCH,
        };
        let next = retrying.advance(Err(Permanence::Permanent), Timestamp::UNIX_EPOCH, policy(5));
        assert_eq!(
            next,
            SensorHealth::Degraded {
                reason: DegradeReason::Permanent
            }
        );
    }

    #[test]
    fn test_retrying_returns_to_bound_on_success() {
        let retrying = SensorHealth::Retrying {
            failures: 2,
            next_attempt: Timestamp::UNIX_EPOCH,
        };
        let next = retrying.advance(Ok(()), Timestamp::UNIX_EPOCH, policy(5));
        assert_eq!(next, SensorHealth::Bound);
    }

    #[test]
    fn test_retrying_increments_failures_below_ceiling() {
        let retrying = SensorHealth::Retrying {
            failures: 1,
            next_attempt: Timestamp::UNIX_EPOCH,
        };
        let next = retrying.advance(
            Err(Permanence::Transient),
            Timestamp::UNIX_EPOCH,
            policy(5),
        );
        assert!(matches!(next, SensorHealth::Retrying { failures: 2, .. }));
    }

    #[test]
    fn test_retrying_degrades_after_reaching_max_retries() {
        let retrying = SensorHealth::Retrying {
            failures: 4,
            next_attempt: Timestamp::UNIX_EPOCH,
        };
        let next = retrying.advance(
            Err(Permanence::Transient),
            Timestamp::UNIX_EPOCH,
            policy(5),
        );
        assert_eq!(
            next,
            SensorHealth::Degraded {
                reason: DegradeReason::RetriesExhausted { attempts: 5 }
            }
        );
    }

    #[test]
    fn test_degraded_is_terminal_regardless_of_outcome() {
        let degraded = SensorHealth::Degraded {
            reason: DegradeReason::Permanent,
        };
        let after_success = degraded.advance(Ok(()), Timestamp::UNIX_EPOCH, policy(5));
        let after_failure =
            degraded.advance(Err(Permanence::Transient), Timestamp::UNIX_EPOCH, policy(5));
        assert_eq!(after_success, degraded);
        assert_eq!(after_failure, degraded);
    }

    #[test]
    fn test_next_attempt_is_now_plus_backoff() {
        let backoff = SignedDuration::from_secs(30);
        let next = SensorHealth::Bound.advance(
            Err(Permanence::Transient),
            Timestamp::UNIX_EPOCH,
            RetryPolicy::new(5, backoff),
        );
        let SensorHealth::Retrying { next_attempt, .. } = next else {
            panic!("expected Retrying state");
        };
        assert_eq!(
            next_attempt,
            Timestamp::UNIX_EPOCH.checked_add(backoff).expect("in range")
        );
    }

    #[test]
    fn test_is_due_true_for_bound() {
        assert!(SensorHealth::Bound.is_due(Timestamp::UNIX_EPOCH));
    }

    #[test]
    fn test_is_due_false_for_degraded() {
        let degraded = SensorHealth::Degraded {
            reason: DegradeReason::Permanent,
        };
        assert!(!degraded.is_due(Timestamp::UNIX_EPOCH));
    }

    #[test]
    fn test_is_due_false_before_next_attempt_and_true_after() {
        let next_attempt = Timestamp::UNIX_EPOCH
            .checked_add(SignedDuration::from_secs(30))
            .expect("in range");
        let retrying = SensorHealth::Retrying {
            failures: 1,
            next_attempt,
        };
        assert!(!retrying.is_due(Timestamp::UNIX_EPOCH));
        assert!(retrying.is_due(next_attempt));
    }

    #[test]
    fn test_degrade_reason_display_is_human_readable() {
        assert_eq!(DegradeReason::Permanent.to_string(), "permanent sensor error");
        assert_eq!(
            DegradeReason::RetriesExhausted { attempts: 5 }.to_string(),
            "retry budget exhausted after 5 attempt(s)"
        );
    }

    #[test]
    fn test_retry_policy_default_matches_documented_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, RetryPolicy::DEFAULT_MAX_RETRIES);
        assert_eq!(
            policy.backoff,
            SignedDuration::from_secs(RetryPolicy::DEFAULT_BACKOFF_SECS)
        );
    }
}
