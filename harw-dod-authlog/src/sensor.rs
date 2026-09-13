//! `AuthlogSensor`: die Brücke zwischen [`crate::backend::AuthBackend`] und
//! `harw_dod_signals::Sensor`.
//!
//! # Warum diese Datei existiert
//! `harw-dod-sentinel` sammelt ausschließlich `Box<dyn harw_dod_signals::Sensor>`
//! (bzw. `Arc<dyn Sensor>`, siehe dortige Moduldoku). Eine Crate, die nur
//! [`crate::backend::AuthBackend`] implementiert, wäre von dieser Sammelstelle
//! nie erreichbar — gebaut, getestet, aber nie abgerufen. [`AuthlogSensor`]
//! schließt genau diese Lücke: er hält ein `Box<dyn AuthBackend>` und
//! implementiert `Sensor` darüber, sodass ein Sentinel ihn wie jeden anderen
//! Sensor pollen kann, ohne `AuthBackend` selbst zu kennen.
//!
//! Die Ausnahme aus der Crate-Dokumentation bleibt dadurch exakt: die
//! Fähigkeit kommt vom Backend (`backend.capability()`), nie vom Sensor
//! selbst — [`AuthlogSensor`] erfindet keine dritte, eigene Fähigkeit.
//!
//! # Die Frage, die diesen Sensor rechtfertigt: woher kommt `since`?
//! `Sensor::poll` bekommt nur `now` injiziert — `&self`, kein veränderlicher
//! Zustand über den Abruf hinaus (siehe `harw_dod_signals::sensor`-Moduldoku:
//! ein Sensor, der sich einen Fortschrittszeiger merkt, ist gegen kein
//! Fixture mehr deterministisch prüfbar, weil zwei Polls mit demselben `now`
//! dann unterschiedliche Ergebnisse liefern könnten, je nachdem, was vorher
//! geschah). [`AuthBackend::read_events`] verlangt aber ein `since`.
//!
//! Die Lösung hier: ein **festes Rückschaufenster** (`lookback`,
//! [`std::time::Duration`]), als Konstruktorparameter
//! ([`AuthlogSensor::with_lookback`]). Jeder Poll berechnet
//! `since = now.saturating_sub(lookback)` neu — zustandslos, deterministisch,
//! fixture-prüfbar: zwei Polls mit demselben `now` fragen exakt dasselbe
//! Fenster ab und liefern exakt dasselbe Ergebnis.
//!
//! Das feste Fenster hat aber eine Kehrseite, und das ist die eigentlich
//! interessante Frage dieses Sensors — abhängig davon, wie `lookback` zum
//! tatsächlichen Poll-Abstand eines Betreibers steht:
//!
//! - **`lookback` ≥ Poll-Abstand (mit Sicherheitsspanne): Überlappung.**
//!   Ein Anmeldeereignis, das kurz vor dem letzten Poll lag, erscheint im
//!   Fenster des nächsten Polls erneut. Der Verbraucher (Regelwerk, Sentinel)
//!   sieht **Dubletten** — dasselbe [`crate::record::AuthRecord`] als zwei
//!   `SecurityEvent`s mit identischem `observed_at`/`actor`/`outcome`.
//! - **`lookback` < Poll-Abstand: Lücke.** Ereignisse, die zwischen dem Ende
//!   des vorherigen Fensters und dem Beginn des aktuellen liegen, werden von
//!   **keinem** Poll erfasst — **verlorene Ereignisse**.
//!
//! Diese Crate entscheidet sich bewusst für die überlappende Seite:
//! `lookback` soll **großzügiger** bemessen sein als der erwartete
//! Poll-Abstand, mit Sicherheitsspanne für Jitter (siehe
//! [`DEFAULT_LOOKBACK`] für den mitgelieferten, bewusst konservativen
//! Vorgabewert und seine Grenzen). Ein verpasstes Anmeldeereignis ist für
//! einen Sicherheitssensor der teurere Fehler als eine Dublette: eine
//! Dublette lässt sich verwerfen, ein nie gesehenes Ereignis nicht mehr
//! rekonstruieren. **Der Verbraucher muss deshalb mit Dubletten rechnen** und
//! nach Inhalt entdoppeln (z. B. über `(sensor, observed_at, actor, outcome)`
//! oder einen Digest darüber) — [`AuthlogSensor`] liefert *mindestens einmal*,
//! nicht *genau einmal*.
//!
//! `harw-dod-sentinel` bestimmt den tatsächlichen Poll-Abstand extern (die
//! Sammelschleife ist aufruferseitig getaktet, siehe dortige Moduldoku); es
//! gibt in diesem Workspace keinen festen, programmweiten Poll-Abstand, aus
//! dem sich ein Vorgabewert zwingend ableiten ließe. [`DEFAULT_LOOKBACK`] ist
//! deshalb ausdrücklich ein **Platzhalter für einen typischen, häufigen
//! Poll-Abstand** — ein Betreiber mit selteneren Polls **muss**
//! [`AuthlogSensor::with_lookback`] mit einem größeren Fenster verwenden,
//! sonst entstehen stille Lücken.
//!
//! # Die Fähigkeits-Invarianz: Griff und Backend müssen zusammenpassen
//! [`AuthlogSensor::handle`] liefert die Fähigkeit über
//! `harw_dod_cap::SensorHandle::capability`, aber **gepollt** wird über
//! [`crate::backend::AuthBackend::capability`] des gehaltenen Backends — ein
//! Sentinel, der Berechtigungen anhand von [`Sensor::handle`] prüft, aber mit
//! einem Backend anderer Fähigkeit pollt, würde die Rechtematrix aus
//! Contract-Master Abschnitt F unterlaufen, ohne dass eine Typprüfung das
//! verhindern könnte (`handle` und `backend` sind zwei unabhängige
//! Konstruktorargumente, kein gemeinsamer Typ). [`AuthlogSensor::with_lookback`]
//! erzwingt deshalb `debug_assert_eq!(handle.capability(), backend.capability())`
//! — ein Fehlbau bricht in Tests und Debug-Builds sofort, statt sich als
//! stille Fehlkonfiguration bis in ein laufendes System zu schleppen; in
//! Release-Builds entfällt die Prüfung (siehe `debug_assert_eq!`-Dokumentation),
//! weshalb ein Aufrufer zusätzlich **dokumentiert** dafür verantwortlich
//! bleibt, `handle` mit derselben Fähigkeit zu bauen wie `backend`.
//!
//! # `harw_dod_fixtures::sensor_suite!` greift hier absichtlich nicht
//! `sensor_suite!` verlangt `From<harw_dod_cap::SensorHandle<harw_dod_cap::Bound>>`
//! — ein gebundener Griff hinein, eine Sensor-Instanz heraus. Das lässt sich
//! für [`AuthlogSensor`] nicht sinnvoll implementieren: die Konstruktion
//! braucht zusätzlich ein `Box<dyn AuthBackend>`, das aus einem bloßen Griff
//! nicht ableitbar ist (derselbe Fall wie ein Drift-Sensor, der zusätzlich
//! eine Baseline braucht). Selbst ein direkter Aufruf der zugrunde liegenden
//! `harw_dod_fixtures::harness::assert_*`-Funktionen — die generisch über
//! `F: Fn(SensorHandle<Bound>) -> S` sind, nicht hart an `From` gebunden —
//! würde hier nichts Sinnvolles prüfen: dieses Fixture-Modell kopiert einen
//! `tree/`-Verzeichnisbaum, bindet ihn als `ReadScope` und schreibt Kanarien
//! in seine Dateien. [`AuthlogSensor`]/[`crate::audit_backend::AuditBackend`]
//! lesen nie über `handle.scope()`/`ReadScope` — ihre Quelle ist
//! ausschließlich das injizierte `Box<dyn AuthBackend>`. Jede
//! `assert_*`-Prüfung dieses Fixture-Modells würde deshalb trivial bestehen,
//! ohne die eigentliche Record-Parsing- oder Inhaltsfreiheits-Logik dieser
//! Crate je auszuführen. Die sieben Prüfungen im Abschlussbericht dieses
//! Knotens ([`crate::audit_backend`], [`crate::fixture_backend`]) sind der
//! inhaltlich richtige Ersatz.
//!
//! # Der Inhalt bleibt heikel
//! `SensorReading::samples` bleibt leer — Anmeldungen sind Ereignisse, keine
//! Messwerte (siehe `harw_dod_signals::sample`-Moduldoku für die Trennung).
//! Jedes erzeugte `SecurityEvent` trägt `actor: Some(...)`, nie einen
//! Benutzer-, Host- oder Terminalnamen (siehe Crate-Dokumentation).
//!
//! # Exportierte Typen
//! [`AuthlogSensor`], [`DEFAULT_LOOKBACK`].
//!
//! # Nebenläufigkeit
//! `AuthlogSensor: Send + Sync + Debug` (Anforderung von
//! `harw_dod_signals::Sensor`): `SensorHandle<Bound>` und `Box<dyn AuthBackend>`
//! sind beide `Send + Sync`, `std::time::Duration` ist es trivial.
//! [`AuthlogSensor::poll`] nimmt `&self` und führt keine innere
//! Veränderlichkeit — konkurrierende Polls auf demselben Sensor sind sicher.
//!
//! # Fehler
//! `harw_dod_cap::SensorError`, wie vom `Sensor`-Trait verlangt — ein
//! [`crate::error::AuthlogError`] aus dem gehaltenen Backend wird auf die
//! innere `SensorError`-Variante entpackt (diese Crate kennt nur die eine).
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuthlogSensor, AuthRecord, FixtureAuthBackend};
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_signals::{Actor, AuthOutcome, Sensor};
//! use harw_types::SensorId;
//! use jiff::Timestamp;
//!
//! let handle = SensorHandle::new(SensorId::from_str("authlog-0"), Capability::ReadAuditNetlink)
//!     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
//! let backend = FixtureAuthBackend::new(
//!     [AuthRecord {
//!         observed_at: Timestamp::UNIX_EPOCH,
//!         actor: Actor { uid: 0, auid: Some(1000), cgroup: None },
//!         outcome: AuthOutcome::Success,
//!     }],
//!     Capability::ReadAuditNetlink,
//! );
//! let sensor = AuthlogSensor::new(handle, Box::new(backend));
//!
//! let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("Fixture scheitert nie");
//! assert!(reading.samples.is_empty());
//! assert_eq!(reading.events.len(), 1);
//! ```

use std::time::Duration;

use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
use jiff::Timestamp;

use crate::backend::AuthBackend;
use crate::error::AuthlogError;

/// Vorgabe-Rückschaufenster: **fünf Minuten**.
///
/// # Beschreibung
/// Ausdrücklich ein Platzhalter, kein aus einem festen Poll-Abstand
/// abgeleiteter Wert — dieser Workspace legt keinen programmweiten
/// Poll-Abstand fest (siehe Moduldokumentation). Ein Betreiber mit einem
/// selteneren Poll-Abstand als fünf Minuten **muss**
/// [`AuthlogSensor::with_lookback`] mit einem größeren Fenster verwenden,
/// sonst entstehen stille Lücken (siehe Moduldokumentation, Abschnitt
/// „woher kommt `since`?").
pub const DEFAULT_LOOKBACK: Duration = Duration::from_secs(300);

/// Ein Sensor für Anmeldeereignisse über ein injiziertes [`AuthBackend`].
///
/// # Description
/// Hält einen gebundenen Griff und ein Backend als zwei unabhängige, bei der
/// Konstruktion übergebene Werte — siehe Moduldokumentation für die
/// Fähigkeits-Invarianz zwischen beiden und für die Herleitung von `since`
/// aus `lookback`.
pub struct AuthlogSensor {
    handle: SensorHandle<Bound>,
    backend: Box<dyn AuthBackend>,
    lookback: Duration,
}

impl AuthlogSensor {
    /// Baut einen Sensor mit [`DEFAULT_LOOKBACK`].
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors.
    /// - `backend` (`Box<dyn AuthBackend>`): die Quelle für Anmeldeereignisse.
    ///
    /// # Returns
    /// Einen `AuthlogSensor`, dessen [`Sensor::poll`] `backend` mit
    /// `since = now.saturating_sub(`[`DEFAULT_LOOKBACK`]`)` abruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != backend.capability()`
    /// (siehe Moduldokumentation, Abschnitt „Fähigkeits-Invarianz"). In
    /// Release-Builds keine Prüfung — der Aufrufer bleibt dokumentiert dafür
    /// verantwortlich.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_authlog::{AuthlogSensor, AuthRecord, FixtureAuthBackend};
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_types::SensorId;
    ///
    /// let handle = SensorHandle::new(SensorId::from_str("authlog-0"), Capability::ReadJournal)
    ///     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
    /// let backend = FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadJournal);
    /// let _sensor = AuthlogSensor::new(handle, Box::new(backend));
    /// ```
    #[must_use]
    pub fn new(handle: SensorHandle<Bound>, backend: Box<dyn AuthBackend>) -> Self {
        Self::with_lookback(handle, backend, DEFAULT_LOOKBACK)
    }

    /// Baut einen Sensor mit einem eigenen Rückschaufenster.
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors.
    /// - `backend` (`Box<dyn AuthBackend>`): die Quelle für Anmeldeereignisse.
    /// - `lookback` (`std::time::Duration`): wie weit
    ///   [`Sensor::poll`] bei jedem Aufruf zurückfragt. Siehe
    ///   Moduldokumentation für die Überlappung/Lücke-Abwägung — großzügig
    ///   bemessen erzeugt Dubletten beim Verbraucher, knapp bemessen
    ///   verliert Ereignisse.
    ///
    /// # Returns
    /// Einen `AuthlogSensor`, dessen [`Sensor::poll`] `backend` mit
    /// `since = now.saturating_sub(lookback)` abruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != backend.capability()`.
    /// In Release-Builds keine Prüfung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_authlog::{AuthlogSensor, AuthRecord, FixtureAuthBackend};
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_types::SensorId;
    /// use std::time::Duration;
    ///
    /// let handle =
    ///     SensorHandle::new(SensorId::from_str("authlog-0"), Capability::ReadAuditNetlink)
    ///         .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
    /// let backend =
    ///     FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadAuditNetlink);
    /// let _sensor =
    ///     AuthlogSensor::with_lookback(handle, Box::new(backend), Duration::from_secs(3600));
    /// ```
    #[must_use]
    pub fn with_lookback(
        handle: SensorHandle<Bound>,
        backend: Box<dyn AuthBackend>,
        lookback: Duration,
    ) -> Self {
        debug_assert_eq!(
            handle.capability(),
            backend.capability(),
            "AuthlogSensor: Griff und Backend müssen dieselbe Capability tragen"
        );
        Self {
            handle,
            backend,
            lookback,
        }
    }
}

impl std::fmt::Debug for AuthlogSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Zeigt Griff und Fenster — das Backend ist ein Trait-Objekt ohne
        // `Debug`-Zusage und könnte ohnehin Recordinhalt puffern.
        f.debug_struct("AuthlogSensor")
            .field("handle", &self.handle)
            .field("lookback", &self.lookback)
            .finish_non_exhaustive()
    }
}

impl Sensor for AuthlogSensor {
    /// Der gebundene Griff dieses Sensors.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Fragt das gehaltene Backend über `[now - lookback, ..]` ab und bildet
    /// jedes [`crate::record::AuthRecord`] auf ein `SecurityEvent` mit
    /// `EventKind::AuthEvent { outcome }` ab.
    ///
    /// # Errors
    /// `harw_dod_cap::SensorError`, entpackt aus [`AuthlogError`], wenn das
    /// Backend nicht lesbar ist oder eine unerwartete Form liefert.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        // `jiff::Timestamp::saturating_sub` liefert entgegen seinem Namen ein
        // `Result`: es scheitert, wenn die Negation der Dauer überläuft
        // (gegen die gepinnte Quelle geprüft, `jiff-0.2/src/timestamp.rs`).
        //
        // Keine der vier `SensorError`-Varianten passt darauf --
        // `MalformedSource` wäre eine Falschetikettierung, denn die Quelle ist
        // in Ordnung, die eigene Fensterlänge ist es nicht. Stattdessen wird
        // auf `Timestamp::MIN` geklemmt: für einen Sicherheitssensor ist "lies
        // ab Anfang der Zeit" die konservative Richtung -- er liest mehr, nie
        // weniger, und kann damit kein Ereignis verpassen. Der Fall tritt nur
        // bei einer absurd großen `lookback` ein, und dort ist genau das die
        // richtige Auslegung.
        let since = now
            .saturating_sub(self.lookback)
            .unwrap_or(jiff::Timestamp::MIN);
        let records = self
            .backend
            .read_events(since)
            .map_err(into_sensor_error)?;

        let events = records
            .into_iter()
            .map(|record| SecurityEvent {
                sensor: self.handle.id().clone(),
                observed_at: record.observed_at,
                actor: Some(record.actor),
                kind: EventKind::AuthEvent {
                    outcome: record.outcome,
                },
            })
            .collect();

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

/// Entpackt die eine `SensorError`-Variante aus [`AuthlogError`].
///
/// # Description
/// [`AuthlogError`] trägt ausschließlich `harw_dod_cap::SensorError`
/// (siehe `crate::error`-Moduldoku); `Sensor::poll` verlangt aber genau
/// diesen inneren Typ als Fehler, nicht `AuthlogError` selbst.
fn into_sensor_error(err: AuthlogError) -> SensorError {
    match err {
        AuthlogError::Sensor(inner) => inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_backend::FixtureAuthBackend;
    use crate::record::AuthRecord;
    use harw_dod_cap::{Capability, ReadScope};
    use harw_dod_signals::{Actor, AuthOutcome};
    use harw_types::SensorId;

    fn handle_with(capability: Capability) -> SensorHandle<Bound> {
        SensorHandle::new(SensorId::from_str("authlog-test"), capability)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()))
    }

    fn auth_record(second: i64, uid: u32, auid: Option<u32>, outcome: AuthOutcome) -> AuthRecord {
        AuthRecord {
            observed_at: Timestamp::new(second, 0).expect("gültiger Zeitstempel"),
            actor: Actor {
                uid,
                auid,
                cgroup: None,
            },
            outcome,
        }
    }

    #[test]
    fn test_handle_returns_bound_handle() {
        let sensor = AuthlogSensor::new(
            handle_with(Capability::ReadJournal),
            Box::new(FixtureAuthBackend::new(
                Vec::<AuthRecord>::new(),
                Capability::ReadJournal,
            )),
        );
        assert_eq!(sensor.handle().capability(), Capability::ReadJournal);
    }

    #[test]
    fn test_poll_maps_auth_record_to_auth_event_with_empty_samples() {
        let sensor = AuthlogSensor::new(
            handle_with(Capability::ReadAuditNetlink),
            Box::new(FixtureAuthBackend::new(
                vec![auth_record(1, 0, Some(1000), AuthOutcome::Success)],
                Capability::ReadAuditNetlink,
            )),
        );

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert!(reading.samples.is_empty());
        assert_eq!(reading.events.len(), 1);

        let event = &reading.events[0];
        assert!(matches!(
            event.kind,
            EventKind::AuthEvent {
                outcome: AuthOutcome::Success
            }
        ));
        let actor = event.actor.as_ref().expect("actor muss gesetzt sein");
        assert_eq!(actor.uid, 0);
        assert_eq!(actor.auid, Some(1000));
    }

    #[test]
    fn test_poll_computes_since_from_now_minus_lookback() {
        // Ein Record knapp außerhalb des Rückschaufensters darf nicht
        // erscheinen; einer knapp innerhalb schon.
        let sensor = AuthlogSensor::with_lookback(
            handle_with(Capability::ReadAuditNetlink),
            Box::new(FixtureAuthBackend::new(
                vec![
                    auth_record(1_000, 1000, Some(1000), AuthOutcome::Success),
                    auth_record(1_101, 1000, Some(1000), AuthOutcome::Success),
                ],
                Capability::ReadAuditNetlink,
            )),
            Duration::from_secs(100),
        );

        let now = Timestamp::new(1_200, 0).expect("gültiger Zeitstempel");
        let reading = sensor.poll(now).expect("Fixture scheitert nie");
        // since = now - 100 = 1100: der erste Record (1000) fällt heraus,
        // der zweite (1101) bleibt.
        assert_eq!(reading.events.len(), 1);
    }

    #[test]
    fn test_poll_two_calls_with_same_now_are_deterministic() {
        let sensor = AuthlogSensor::new(
            handle_with(Capability::ReadJournal),
            Box::new(FixtureAuthBackend::new(
                vec![auth_record(1, 0, None, AuthOutcome::Failure)],
                Capability::ReadJournal,
            )),
        );

        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("erster Poll");
        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("zweiter Poll");
        assert_eq!(first, second);
    }

    #[test]
    fn test_poll_overlapping_windows_yield_duplicate_events_by_design() {
        // Dokumentiert die Kehrseite eines großzügigen Rückschaufensters:
        // zwei Polls, deren Fenster sich überlappen, sehen denselben Record
        // zweimal — der Verbraucher muss entdoppeln.
        let backend = FixtureAuthBackend::new(
            vec![auth_record(1_000, 1000, Some(1000), AuthOutcome::Success)],
            Capability::ReadAuditNetlink,
        );
        let sensor = AuthlogSensor::with_lookback(
            handle_with(Capability::ReadAuditNetlink),
            Box::new(backend),
            Duration::from_secs(500),
        );

        let first_poll = sensor
            .poll(Timestamp::new(1_050, 0).expect("gültiger Zeitstempel"))
            .expect("erster Poll");
        let second_poll = sensor
            .poll(Timestamp::new(1_100, 0).expect("gültiger Zeitstempel"))
            .expect("zweiter Poll");

        assert_eq!(first_poll.events.len(), 1);
        assert_eq!(second_poll.events.len(), 1);
        assert_eq!(first_poll.events[0], second_poll.events[0]);
    }
}
