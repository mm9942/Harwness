//! Ringpuffer für die jüngsten Samples und Events (Knoten AW2-18).
//!
//! # Zwei getrennte Ringe, nicht einer
//! [`EvidenceBuffer`] hält **zwei unabhängige** Ringe — einen für
//! [`HostSample`], einen für [`SecurityEvent`] — statt eines einzigen
//! gemeinsamen Rings über beide Ströme. Das ist keine Bequemlichkeit,
//! sondern die Konsequenz aus der Größenbegründung unten: Samples sind
//! „langweilig und häufig", Events sind „selten und wichtig" (siehe
//! `harw_dod_signals::sample`-Moduldoku für die volle Begründung dieser
//! Trennung auf Typebene). Ein gemeinsamer Ring fester Größe ließe jeden
//! Sample-Zyklus proportional viele Plätze belegen — bei genügend Samples
//! zwischen zwei Events würde reine, bedeutungslose Telemetrie ein seltenes,
//! sicherheitsrelevantes Ereignis aus dem Puffer verdrängen, bevor es
//! eingefroren werden kann. Zwei getrennte Ringe machen diese Verdrängung
//! strukturell unmöglich: eine Sample-Flut kann höchstens andere Samples
//! verdrängen, nie ein Event.
//!
//! # Die Puffergröße als Sicherheitsentscheidung
//! Ein Ringpuffer überschreibt beim Erreichen seiner Kapazität den ältesten
//! Eintrag. Das ist der Punkt, an dem Größe zu einer Sicherheitsfrage wird,
//! nicht nur einer Speicherfrage:
//!
//! - **Zu klein**, und ein Angreifer — oder auch nur gewöhnliches
//!   Betriebsrauschen — verdrängt die Spuren eines Vorfalls, bevor
//!   [`EvidenceBuffer::freeze`] sie als [`harw_dod_signals::SecurityEvidence`]
//!   zitierfähig gemacht hat. Für den Event-Ring ist das der wörtliche
//!   Angriff, den diese Crate verhindern soll: ein Vorfall, der real
//!   stattfand, aber aus dem Puffer gefallen ist, bevor ihn irgendjemand
//!   ausgelesen hat, ist ununterscheidbar von einem Vorfall, der nie
//!   stattfand.
//! - **Zu groß**, und der Speicherbedarf ist unbegrenzt — bei elf Sensoren,
//!   die kontinuierlich abgerufen werden, ein realer Betriebsfaktor, nicht
//!   nur ein theoretisches Risiko.
//!
//! Diese Crate löst die Abwägung, indem sie die Größe **konfigurierbar**
//! macht ([`EvidenceBuffer::new`]) statt sie fest zu verdrahten — die
//! tatsächlich richtige Größe hängt von der Abrufkadenz ab, die das Binary
//! (Knoten AW2-19) wählt, und die kennt diese Crate nicht. Die
//! Voreinstellungen ([`EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY`],
//! [`EvidenceBuffer::DEFAULT_EVENT_CAPACITY`]) sind für Entwicklung und Tests
//! gedacht, nicht als abschließende Produktionsentscheidung:
//!
//! - `DEFAULT_EVENT_CAPACITY = 4096`: Events sind der Strom, den ein
//!   Angreifer am ehesten verdrängen möchte (siehe oben) und gleichzeitig der
//!   seltenere der beiden — eine vierstellige Kapazität gibt großzügigen
//!   Vorlauf gegen Verdrängung, bei vernachlässigbarem Speicherbedarf ( ein
//!   [`SecurityEvent`] ist klein: einige Dutzend bis wenige hundert Bytes).
//! - `DEFAULT_SAMPLE_CAPACITY = 2048`: Samples sind häufiger, aber für sich
//!   genommen weniger einzeln beweiskräftig als ein Event — eine kleinere,
//!   aber immer noch großzügige Kapazität hält den Speicherbedarf im
//!   Rahmen, ohne den Ring bei normaler Abrufkadenz binnen Sekunden zu
//!   durchlaufen.
//!
//! # Delta-Auswertung: Zähler und Marken (PL-100 WP0.1)
//! Der Ring wird nie geleert — er bleibt der **Kontextspeicher**, aus dem
//! [`EvidenceBuffer::freeze`] jederzeit den vollen, zitierfähigen Inhalt
//! einfriert. Wer die Regeln aber Zyklus für Zyklus über den *ganzen* Ring
//! laufen lässt, meldet dasselbe Ereignis in jedem Zyklus erneut, solange es
//! im Ring liegt (die „Befund-Flut" aus PL-100). Deshalb zählt jeder Ring
//! zusätzlich monoton, wie viele Einträge er je aufgenommen hat:
//!
//! - Jeder aufgenommene Eintrag erhält implizit eine Folgenummer
//!   (`0, 1, 2, …`); gehalten werden stets die Folgenummern
//!   `[pushed - len, pushed)`.
//! - [`EvidenceBuffer::mark`] liefert eine [`BufferMark`]: den Zählerstand
//!   beider Ringe zu einem Zeitpunkt.
//! - [`EvidenceBuffer::samples_since`]/[`EvidenceBuffer::events_since`]
//!   liefern nur die noch gehaltenen Einträge, die **nach** einer Marke
//!   ankamen; [`EvidenceBuffer::freeze_since`] friert genau dieses Delta als
//!   eigenes [`SecurityEvidence`] ein ([`EvidenceDelta`]) und nennt die
//!   Marke, ab der der nächste Zyklus weitermacht.
//! - Kamen seit der Marke mehr Einträge an, als der Ring fasst, sind die
//!   ältesten davon schon verdrängt, bevor sie je ausgewertet wurden.
//!   [`EvidenceBuffer::samples_lost_since`]/[`EvidenceBuffer::events_lost_since`]
//!   zählen sie, statt sie still zu verschweigen.
//!
//! Ein Ring der Kapazität `0` nimmt nie einen Eintrag auf und zählt deshalb
//! auch keinen (siehe [`EvidenceBuffer::new`]). Die Zähler sind `u64` und
//! sättigen statt überzulaufen; bei einer Million Einträgen je Sekunde ist
//! die Grenze erst nach über 500 000 Jahren erreicht.
//!
//! # Exportierte Typen
//! [`EvidenceBuffer`], [`BufferMark`], [`EvidenceDelta`].
//!
//! # Nebenläufigkeit
//! Reiner, unveränderlicher Zustand hinter `&mut self` für Schreibzugriffe
//! ([`EvidenceBuffer::push_sample`], [`EvidenceBuffer::push_event`]) und
//! `&self` für Lesezugriffe — kein Interior Mutability, kein internes
//! Locking. [`crate::Sentinel`] hält genau eine `EvidenceBuffer`-Instanz und
//! greift aus einem einzigen Sammelthread darauf zu (siehe `lib.rs`-Moduldoku,
//! Abschnitt "Nebenläufigkeit").
//!
//! # Fehler
//! [`crate::error::SentinelError`] — ausschließlich über
//! [`EvidenceBuffer::freeze`], das an
//! [`harw_dod_signals::SecurityEvidence::capture`] delegiert.
//!
//! # Examples
//! ```rust
//! use harw_dod_sentinel::buffer::EvidenceBuffer;
//! use harw_dod_signals::HostSample;
//! use harw_types::SensorId;
//!
//! let mut buffer = EvidenceBuffer::new(2, 2);
//! buffer.push_sample(HostSample {
//!     sensor: SensorId::from_str("thermal-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     metric: std::borrow::Cow::Borrowed("temperature_celsius"),
//!     value: 42.5,
//! });
//! assert_eq!(buffer.sample_len(), 1);
//!
//! let evidence = buffer
//!     .freeze(jiff::Timestamp::UNIX_EPOCH)
//!     .expect("evidence capture is infallible for these field types");
//! assert_eq!(evidence.samples.len(), 1);
//! ```

use std::collections::VecDeque;

use harw_dod_signals::{HostSample, SecurityEvent, SecurityEvidence};
use jiff::Timestamp;

use crate::error::{SentinelError, SentinelResult};

/// Zwei getrennte Ringpuffer — Samples und Events — mit fester,
/// konfigurierbarer Kapazität.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung der Trennung und der
/// Größenwahl. Beide Ringe überschreiben beim Erreichen ihrer Kapazität den
/// jeweils ältesten Eintrag (FIFO-Verdrängung); die relative Reihenfolge der
/// verbleibenden Einträge bleibt dabei erhalten.
#[derive(Debug)]
pub struct EvidenceBuffer {
    /// Der Sample-Ring, ältester Eintrag vorn.
    samples: VecDeque<HostSample>,
    /// Feste Kapazität des Sample-Rings.
    sample_capacity: usize,
    /// Monotone Anzahl je in den Sample-Ring aufgenommener Einträge
    /// (Folgenummer des nächsten Eintrags, siehe Moduldoku).
    samples_pushed: u64,
    /// Der Event-Ring, ältester Eintrag vorn.
    events: VecDeque<SecurityEvent>,
    /// Feste Kapazität des Event-Rings.
    event_capacity: usize,
    /// Monotone Anzahl je in den Event-Ring aufgenommener Einträge.
    events_pushed: u64,
}

/// Zählerstand beider Ringe eines [`EvidenceBuffer`] zu einem Zeitpunkt.
///
/// # Description
/// Eine Marke trennt „schon ausgewertet" von „neu angekommen" (siehe
/// Moduldoku, Abschnitt „Delta-Auswertung: Zähler und Marken"). Sie ist nur
/// gegenüber dem Puffer aussagekräftig, der sie mit [`EvidenceBuffer::mark`]
/// geliefert hat; eine Marke „aus der Zukunft" (größer als der aktuelle
/// Zählerstand) liefert schlicht ein leeres Delta.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_dod_sentinel::buffer::{BufferMark, EvidenceBuffer};
///
/// let buffer = EvidenceBuffer::new(4, 4);
/// assert_eq!(buffer.mark(), BufferMark::START);
/// assert_eq!(BufferMark::START.events(), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BufferMark {
    /// Zählerstand des Sample-Rings.
    samples: u64,
    /// Zählerstand des Event-Rings.
    events: u64,
}

impl BufferMark {
    /// Die Marke vor dem allerersten Eintrag: ein Delta ab ihr umfasst
    /// alles, was der Ring noch hält.
    pub const START: Self = Self {
        samples: 0,
        events: 0,
    };

    /// Zählerstand des Sample-Rings in dieser Marke.
    #[must_use]
    pub const fn samples(self) -> u64 {
        self.samples
    }

    /// Zählerstand des Event-Rings in dieser Marke.
    #[must_use]
    pub const fn events(self) -> u64 {
        self.events
    }
}

/// Das seit einer [`BufferMark`] angekommene Delta beider Ringe, eingefroren.
///
/// # Description
/// Ergebnis von [`EvidenceBuffer::freeze_since`]. `evidence` enthält nur
/// die nach der Marke angekommenen und noch gehaltenen Einträge, in
/// Ringreihenfolge; sein Digest deckt genau dieses Delta ab, nicht den
/// ganzen Ring.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceDelta {
    /// Der eingefrorene Beleg über das Delta.
    pub evidence: SecurityEvidence,
    /// Der Zählerstand am Ende dieses Deltas — die Marke, die der nächste
    /// Zyklus als `since` übergibt.
    pub until: BufferMark,
    /// Samples, die nach der Marke ankamen, aber schon verdrängt waren,
    /// bevor dieses Delta gebildet wurde (nie ausgewertet).
    pub samples_lost: u64,
    /// Events, die nach der Marke ankamen, aber schon verdrängt waren,
    /// bevor dieses Delta gebildet wurde (nie ausgewertet).
    pub events_lost: u64,
}

impl EvidenceBuffer {
    /// Voreingestellte Kapazität des Sample-Rings. Siehe Moduldoku,
    /// Abschnitt "Die Puffergröße als Sicherheitsentscheidung", für die
    /// Begründung.
    pub const DEFAULT_SAMPLE_CAPACITY: usize = 2048;
    /// Voreingestellte Kapazität des Event-Rings. Siehe Moduldoku, Abschnitt
    /// "Die Puffergröße als Sicherheitsentscheidung", für die Begründung.
    pub const DEFAULT_EVENT_CAPACITY: usize = 4096;

    /// Baut einen leeren Puffer mit den gegebenen Kapazitäten.
    ///
    /// # Arguments
    /// - `sample_capacity` (`usize`): maximale Anzahl gleichzeitig
    ///   gehaltener [`HostSample`]-Einträge. `0` ist zulässig und bedeutet:
    ///   dieser Ring hält nie einen Eintrag ([`Self::push_sample`] wird zum
    ///   No-op).
    /// - `event_capacity` (`usize`): maximale Anzahl gleichzeitig gehaltener
    ///   [`SecurityEvent`]-Einträge, mit derselben `0`-Bedeutung.
    ///
    /// # Returns
    /// Einen `EvidenceBuffer` ohne Einträge.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::EvidenceBuffer;
    ///
    /// let buffer = EvidenceBuffer::new(1024, 1024);
    /// assert_eq!(buffer.sample_len(), 0);
    /// assert_eq!(buffer.event_len(), 0);
    /// ```
    #[must_use]
    pub fn new(sample_capacity: usize, event_capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(sample_capacity),
            sample_capacity,
            samples_pushed: 0,
            events: VecDeque::with_capacity(event_capacity),
            event_capacity,
            events_pushed: 0,
        }
    }

    /// Fügt einen Messwert hinzu und verdrängt bei Bedarf den ältesten.
    ///
    /// # Description
    /// Ist der Sample-Ring bereits an seiner Kapazität, wird zuerst der
    /// älteste Eintrag entfernt, bevor `sample` angehängt wird — der Ring
    /// wächst nie über [`Self::sample_capacity`] hinaus. Jeder aufgenommene
    /// Eintrag erhöht [`Self::samples_pushed`] um eins (siehe Moduldoku,
    /// Abschnitt „Delta-Auswertung: Zähler und Marken").
    ///
    /// # Arguments
    /// - `sample` (`HostSample`): der einzufügende Messwert.
    ///
    /// # Concurrency
    /// Erfordert `&mut self`; kein interner Zustand, der aus mehreren
    /// Threads gleichzeitig sicher wäre.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::EvidenceBuffer;
    /// use harw_dod_signals::HostSample;
    /// use harw_types::SensorId;
    ///
    /// let mut buffer = EvidenceBuffer::new(1, 1);
    /// let sample = |v: f64| HostSample {
    ///     sensor: SensorId::from_str("thermal-0"),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     metric: std::borrow::Cow::Borrowed("temperature_celsius"),
    ///     value: v,
    /// };
    /// buffer.push_sample(sample(1.0));
    /// buffer.push_sample(sample(2.0));
    /// assert_eq!(buffer.sample_len(), 1);
    /// ```
    pub fn push_sample(&mut self, sample: HostSample) {
        if self.sample_capacity == 0 {
            return;
        }
        if self.samples.len() >= self.sample_capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.samples_pushed = self.samples_pushed.saturating_add(1);
    }

    /// Fügt ein Ereignis hinzu und verdrängt bei Bedarf das älteste.
    ///
    /// # Description
    /// Analog zu [`Self::push_sample`], für den unabhängigen Event-Ring
    /// (siehe Moduldoku, Abschnitt "Zwei getrennte Ringe, nicht einer");
    /// jeder aufgenommene Eintrag erhöht [`Self::events_pushed`] um eins.
    ///
    /// # Arguments
    /// - `event` (`SecurityEvent`): das einzufügende Ereignis.
    ///
    /// # Concurrency
    /// Erfordert `&mut self`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::EvidenceBuffer;
    /// use harw_dod_signals::{EventKind, SecurityEvent};
    /// use harw_types::SensorId;
    ///
    /// let mut buffer = EvidenceBuffer::new(1, 1);
    /// let event = |port: u16| SecurityEvent {
    ///     sensor: SensorId::from_str("netmon-0"),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     actor: None,
    ///     kind: EventKind::ListenerOpened { port },
    /// };
    /// buffer.push_event(event(8080));
    /// buffer.push_event(event(9090));
    /// assert_eq!(buffer.event_len(), 1);
    /// ```
    pub fn push_event(&mut self, event: SecurityEvent) {
        if self.event_capacity == 0 {
            return;
        }
        if self.events.len() >= self.event_capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.events_pushed = self.events_pushed.saturating_add(1);
    }

    /// Konfigurierte Kapazität des Sample-Rings.
    #[must_use]
    pub const fn sample_capacity(&self) -> usize {
        self.sample_capacity
    }

    /// Konfigurierte Kapazität des Event-Rings.
    #[must_use]
    pub const fn event_capacity(&self) -> usize {
        self.event_capacity
    }

    /// Aktuelle Anzahl gehaltener Samples.
    #[must_use]
    pub fn sample_len(&self) -> usize {
        self.samples.len()
    }

    /// Aktuelle Anzahl gehaltener Events.
    #[must_use]
    pub fn event_len(&self) -> usize {
        self.events.len()
    }

    /// Füllstand des Sample-Rings, zwischen `0.0` (leer) und `1.0` (voll).
    ///
    /// # Returns
    /// `0.0`, wenn [`Self::sample_capacity`] `0` ist (ein Ring ohne Plätze
    /// hat keinen sinnvollen Füllstand); sonst `sample_len() /
    /// sample_capacity()`.
    #[must_use]
    pub fn sample_utilization(&self) -> f64 {
        utilization(self.samples.len(), self.sample_capacity)
    }

    /// Füllstand des Event-Rings, zwischen `0.0` (leer) und `1.0` (voll).
    ///
    /// Siehe [`Self::sample_utilization`] für die `0`-Kapazitäts-Regel.
    #[must_use]
    pub fn event_utilization(&self) -> f64 {
        utilization(self.events.len(), self.event_capacity)
    }

    /// Die gehaltenen Samples, von ältestem zu jüngstem.
    pub fn samples(&self) -> impl Iterator<Item = &HostSample> {
        self.samples.iter()
    }

    /// Die gehaltenen Events, von ältestem zu jüngstem.
    pub fn events(&self) -> impl Iterator<Item = &SecurityEvent> {
        self.events.iter()
    }

    /// Wie viele Samples dieser Ring seit seiner Erzeugung aufgenommen hat.
    ///
    /// # Description
    /// Monoton steigend (sättigend), unabhängig von späterer Verdrängung —
    /// siehe Moduldoku, Abschnitt „Delta-Auswertung: Zähler und Marken".
    #[must_use]
    pub const fn samples_pushed(&self) -> u64 {
        self.samples_pushed
    }

    /// Wie viele Events dieser Ring seit seiner Erzeugung aufgenommen hat.
    ///
    /// Siehe [`Self::samples_pushed`].
    #[must_use]
    pub const fn events_pushed(&self) -> u64 {
        self.events_pushed
    }

    /// Der aktuelle Zählerstand beider Ringe.
    ///
    /// # Returns
    /// Eine [`BufferMark`]; ein späteres [`Self::events_since`] mit ihr
    /// liefert genau die danach aufgenommenen Events.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::EvidenceBuffer;
    /// use harw_dod_signals::{EventKind, SecurityEvent};
    /// use harw_types::SensorId;
    ///
    /// let mut buffer = EvidenceBuffer::new(8, 8);
    /// let event = |port: u16| SecurityEvent {
    ///     sensor: SensorId::from_str("netmon-0"),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     actor: None,
    ///     kind: EventKind::ListenerOpened { port },
    /// };
    /// buffer.push_event(event(80));
    /// let mark = buffer.mark();
    /// buffer.push_event(event(443));
    /// assert_eq!(buffer.events_since(mark).count(), 1);
    /// assert_eq!(buffer.event_len(), 2, "the ring keeps both as context");
    /// ```
    #[must_use]
    pub const fn mark(&self) -> BufferMark {
        BufferMark {
            samples: self.samples_pushed,
            events: self.events_pushed,
        }
    }

    /// Die noch gehaltenen Samples, die nach `mark` aufgenommen wurden, von
    /// ältestem zu jüngstem.
    ///
    /// # Arguments
    /// - `mark` (`BufferMark`): eine frühere [`Self::mark`] dieses Puffers.
    ///
    /// # Returns
    /// Leer, wenn seit `mark` nichts ankam (oder `mark` aus der Zukunft
    /// stammt). Bereits verdrängte Einträge fehlen; ihre Anzahl liefert
    /// [`Self::samples_lost_since`].
    pub fn samples_since(&self, mark: BufferMark) -> impl Iterator<Item = &HostSample> {
        let skip = held_before(mark.samples, self.samples_pushed, self.samples.len());
        self.samples.iter().skip(skip)
    }

    /// Die noch gehaltenen Events, die nach `mark` aufgenommen wurden, von
    /// ältestem zu jüngstem.
    ///
    /// Siehe [`Self::samples_since`]; verdrängte Einträge zählt
    /// [`Self::events_lost_since`].
    pub fn events_since(&self, mark: BufferMark) -> impl Iterator<Item = &SecurityEvent> {
        let skip = held_before(mark.events, self.events_pushed, self.events.len());
        self.events.iter().skip(skip)
    }

    /// Wie viele Samples nach `mark` aufgenommen, aber schon wieder
    /// verdrängt wurden — Einträge, die ein Delta ab `mark` nie sieht.
    #[must_use]
    pub fn samples_lost_since(&self, mark: BufferMark) -> u64 {
        evicted_since(mark.samples, self.samples_pushed, self.samples.len())
    }

    /// Wie viele Events nach `mark` aufgenommen, aber schon wieder verdrängt
    /// wurden. Siehe [`Self::samples_lost_since`].
    #[must_use]
    pub fn events_lost_since(&self, mark: BufferMark) -> u64 {
        evicted_since(mark.events, self.events_pushed, self.events.len())
    }

    /// Friert nur das seit `since` angekommene Delta als Beleg ein.
    ///
    /// # Description
    /// Wie [`Self::freeze`], aber über [`Self::samples_since`] und
    /// [`Self::events_since`] statt über den ganzen Ring. Der Ring selbst
    /// bleibt unverändert (er ist weiterhin der Kontextspeicher für
    /// [`Self::freeze`]). Wer Zyklus für Zyklus auswertet, übergibt beim
    /// nächsten Aufruf [`EvidenceDelta::until`] als `since` und sieht so
    /// jeden Eintrag genau einmal (PL-100 WP0.1).
    ///
    /// # Arguments
    /// - `since` (`BufferMark`): Marke des vorigen Zyklus;
    ///   [`BufferMark::START`] für „alles, was der Ring noch hält".
    /// - `now` (`jiff::Timestamp`): injizierter Einfrierzeitpunkt.
    ///
    /// # Returns
    /// Ein [`EvidenceDelta`] mit dem Beleg, der Folgemarke und den Zählern
    /// verdrängter, nie ausgewerteter Einträge.
    ///
    /// # Errors
    /// - [`SentinelError::Evidence`]: wie bei [`Self::freeze`].
    ///
    /// # Concurrency
    /// Reine Lesefunktion, siehe [`Self::freeze`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::{BufferMark, EvidenceBuffer};
    /// use harw_dod_signals::{EventKind, SecurityEvent};
    /// use harw_types::SensorId;
    ///
    /// let mut buffer = EvidenceBuffer::new(8, 8);
    /// buffer.push_event(SecurityEvent {
    ///     sensor: SensorId::from_str("netmon-0"),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     actor: None,
    ///     kind: EventKind::ListenerOpened { port: 8080 },
    /// });
    /// let first = buffer.freeze_since(BufferMark::START, jiff::Timestamp::UNIX_EPOCH)?;
    /// assert_eq!(first.evidence.events.len(), 1);
    /// let second = buffer.freeze_since(first.until, jiff::Timestamp::UNIX_EPOCH)?;
    /// assert!(second.evidence.events.is_empty(), "nothing new since the first delta");
    /// # Ok::<(), harw_dod_sentinel::SentinelError>(())
    /// ```
    pub fn freeze_since(&self, since: BufferMark, now: Timestamp) -> SentinelResult<EvidenceDelta> {
        let samples: Vec<HostSample> = self.samples_since(since).cloned().collect();
        let events: Vec<SecurityEvent> = self.events_since(since).cloned().collect();
        let evidence =
            SecurityEvidence::capture(samples, events, now).map_err(SentinelError::from)?;
        Ok(EvidenceDelta {
            evidence,
            until: self.mark(),
            samples_lost: self.samples_lost_since(since),
            events_lost: self.events_lost_since(since),
        })
    }

    /// Friert den aktuellen Pufferinhalt als zitierfähigen Beleg ein.
    ///
    /// # Description
    /// Übergibt Samples und Events in ihrer aktuellen Ringreihenfolge
    /// (ältestes zuerst) unverändert an
    /// [`harw_dod_signals::SecurityEvidence::capture`], das den Digest bildet
    /// — diese Crate berechnet ihn nicht selbst (siehe dortige Moduldoku für
    /// die Bildungsregel). Zwei Aufrufe ohne dazwischenliegende
    /// [`Self::push_sample`]/[`Self::push_event`]-Aufrufe liefern denselben
    /// Digest, weil dieselben Elemente in derselben Reihenfolge erneut
    /// serialisiert werden.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierter Einfrierzeitpunkt.
    ///
    /// # Returns
    /// Ein [`SecurityEvidence`] mit dem aktuellen Pufferinhalt.
    ///
    /// # Errors
    /// - [`SentinelError::Evidence`]: die kanonische Kodierung von Samples
    ///   und Events ist fehlgeschlagen (siehe dortige Doku: praktisch
    ///   unerreichbar für die hier gehaltenen Feldtypen, aber nicht
    ///   ausgeschlossen — deshalb `Result`-basiert statt eines verbotenen
    ///   `unwrap()`/`expect()`).
    ///
    /// # Concurrency
    /// Reine Lesefunktion; sicher aus jedem Thread aufrufbar, solange kein
    /// anderer Thread gleichzeitig `&mut self`-Zugriff hält.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::buffer::EvidenceBuffer;
    ///
    /// let buffer = EvidenceBuffer::new(8, 8);
    /// let a = buffer
    ///     .freeze(jiff::Timestamp::UNIX_EPOCH)
    ///     .expect("empty buffer always encodes");
    /// let b = buffer
    ///     .freeze(jiff::Timestamp::UNIX_EPOCH)
    ///     .expect("empty buffer always encodes");
    /// assert_eq!(a.digest, b.digest);
    /// ```
    pub fn freeze(&self, now: Timestamp) -> SentinelResult<SecurityEvidence> {
        let samples: Vec<HostSample> = self.samples.iter().cloned().collect();
        let events: Vec<SecurityEvent> = self.events.iter().cloned().collect();
        SecurityEvidence::capture(samples, events, now).map_err(SentinelError::from)
    }
}

impl Default for EvidenceBuffer {
    /// Siehe Typ-Doku für [`EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY`] und
    /// [`EvidenceBuffer::DEFAULT_EVENT_CAPACITY`].
    fn default() -> Self {
        Self::new(Self::DEFAULT_SAMPLE_CAPACITY, Self::DEFAULT_EVENT_CAPACITY)
    }
}

/// Folgenummer des ältesten noch gehaltenen Eintrags eines Rings mit
/// `pushed` aufgenommenen und `len` gehaltenen Einträgen.
fn oldest_held(pushed: u64, len: usize) -> u64 {
    pushed.saturating_sub(u64::try_from(len).unwrap_or(u64::MAX))
}

/// Wie viele der `len` gehaltenen Einträge vor `mark` aufgenommen wurden
/// und deshalb in einem Delta ab `mark` übersprungen werden (höchstens
/// `len`, auch für eine Marke aus der Zukunft).
fn held_before(mark: u64, pushed: u64, len: usize) -> usize {
    let before = mark.saturating_sub(oldest_held(pushed, len));
    usize::try_from(before).map_or(len, |before| before.min(len))
}

/// Wie viele Einträge nach `mark` aufgenommen, aber schon verdrängt wurden.
fn evicted_since(mark: u64, pushed: u64, len: usize) -> u64 {
    oldest_held(pushed, len).saturating_sub(mark)
}

/// Reiner Hilfsfunktion für den Füllstand eines Rings; siehe
/// [`EvidenceBuffer::sample_utilization`] und
/// [`EvidenceBuffer::event_utilization`].
fn utilization(len: usize, capacity: usize) -> f64 {
    if capacity == 0 {
        0.0
    } else {
        len as f64 / capacity as f64
    }
}

#[cfg(test)]
mod tests {
    use super::{BufferMark, EvidenceBuffer};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::{EventKind, HostSample, SecurityEvent};
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn sample(value: f64) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("thermal-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed("temperature_celsius"),
            value,
        }
    }

    fn event(port: u16) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("netmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ListenerOpened { port },
        }
    }

    #[test]
    fn test_new_buffer_is_empty() {
        let buffer = EvidenceBuffer::new(4, 4);
        assert_eq!(buffer.sample_len(), 0);
        assert_eq!(buffer.event_len(), 0);
    }

    #[test]
    fn test_push_sample_overwrites_oldest_when_capacity_exceeded() {
        let mut buffer = EvidenceBuffer::new(3, 3);
        for i in 0..4 {
            buffer.push_sample(sample(f64::from(i)));
        }
        let values: Vec<f64> = buffer.samples().map(|s| s.value).collect();
        // Kapazität 3, 4 Einträge eingefügt: der erste (0.0) ist verdrängt.
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_push_event_overwrites_oldest_when_capacity_exceeded() -> TestResult {
        let mut buffer = EvidenceBuffer::new(2, 2);
        buffer.push_event(event(1));
        buffer.push_event(event(2));
        buffer.push_event(event(3));
        let ports: Vec<u16> = buffer
            .events()
            .map(|e| match e.kind {
                EventKind::ListenerOpened { port } => Ok(port),
                _ => Err(TestError::Unexpected(
                    "test fixture only produces ListenerOpened".into(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(ports, vec![2, 3]);
        Ok(())
    }

    #[test]
    fn test_zero_capacity_sample_ring_never_holds_an_entry() {
        let mut buffer = EvidenceBuffer::new(0, 4);
        buffer.push_sample(sample(1.0));
        assert_eq!(buffer.sample_len(), 0);
    }

    #[test]
    fn test_sample_flood_does_not_evict_events() {
        // Der Kernpunkt der Trennung in zwei Ringe: beliebig viele Samples
        // dürfen niemals ein Event verdrängen.
        let mut buffer = EvidenceBuffer::new(2, 8);
        buffer.push_event(event(1));
        for i in 0..100 {
            buffer.push_sample(sample(f64::from(i)));
        }
        assert_eq!(buffer.event_len(), 1);
    }

    #[test]
    fn test_utilization_reflects_fill_level() {
        let mut buffer = EvidenceBuffer::new(4, 4);
        assert_eq!(buffer.sample_utilization(), 0.0);
        buffer.push_sample(sample(1.0));
        buffer.push_sample(sample(2.0));
        assert_eq!(buffer.sample_utilization(), 0.5);
    }

    #[test]
    fn test_utilization_is_zero_for_zero_capacity_ring() {
        let buffer = EvidenceBuffer::new(0, 0);
        assert_eq!(buffer.sample_utilization(), 0.0);
        assert_eq!(buffer.event_utilization(), 0.0);
    }

    #[test]
    fn test_freeze_digest_is_stable_across_calls_without_new_entries() -> TestResult {
        let mut buffer = EvidenceBuffer::new(4, 4);
        buffer.push_sample(sample(1.0));
        buffer.push_event(event(80));

        let first = buffer
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        let second = buffer
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;

        assert_eq!(first.digest, second.digest);
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn test_freeze_preserves_ring_order() -> TestResult {
        let mut buffer = EvidenceBuffer::new(4, 4);
        buffer.push_sample(sample(1.0));
        buffer.push_sample(sample(2.0));

        let evidence = buffer
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(evidence.samples, vec![sample(1.0), sample(2.0)]);
        Ok(())
    }

    fn ports_of<'a>(events: impl Iterator<Item = &'a SecurityEvent>) -> TestResult<Vec<u16>> {
        events
            .map(|e| match e.kind {
                EventKind::ListenerOpened { port } => Ok(port),
                _ => Err(TestError::Unexpected(
                    "test fixture only produces ListenerOpened".into(),
                )),
            })
            .collect()
    }

    #[test]
    fn test_new_buffer_starts_at_the_start_mark() {
        let buffer = EvidenceBuffer::new(4, 4);
        assert_eq!(buffer.mark(), BufferMark::START);
        assert_eq!(buffer.samples_pushed(), 0);
        assert_eq!(buffer.events_pushed(), 0);
    }

    #[test]
    fn test_push_counters_are_monotonic_across_eviction() {
        let mut buffer = EvidenceBuffer::new(2, 2);
        for i in 0_u8..5 {
            buffer.push_sample(sample(f64::from(i)));
            buffer.push_event(event(u16::from(i)));
        }
        assert_eq!(buffer.sample_len(), 2);
        assert_eq!(buffer.samples_pushed(), 5);
        assert_eq!(buffer.events_pushed(), 5);
        assert_eq!(buffer.mark().samples(), 5);
        assert_eq!(buffer.mark().events(), 5);
    }

    #[test]
    fn test_zero_capacity_ring_counts_nothing() {
        let mut buffer = EvidenceBuffer::new(0, 0);
        buffer.push_sample(sample(1.0));
        buffer.push_event(event(1));
        assert_eq!(buffer.mark(), BufferMark::START);
        assert_eq!(buffer.events_lost_since(BufferMark::START), 0);
    }

    #[test]
    fn test_events_since_yields_only_entries_after_the_mark() -> TestResult {
        let mut buffer = EvidenceBuffer::new(8, 8);
        buffer.push_event(event(1));
        buffer.push_event(event(2));
        let mark = buffer.mark();
        buffer.push_event(event(3));

        assert_eq!(ports_of(buffer.events_since(mark))?, vec![3]);
        assert_eq!(
            ports_of(buffer.events_since(BufferMark::START))?,
            vec![1, 2, 3]
        );
        assert_eq!(buffer.events_since(buffer.mark()).count(), 0);
        assert_eq!(
            buffer.event_len(),
            3,
            "the ring keeps every entry as context"
        );
        Ok(())
    }

    #[test]
    fn test_samples_since_yields_only_entries_after_the_mark() {
        let mut buffer = EvidenceBuffer::new(8, 8);
        buffer.push_sample(sample(1.0));
        let mark = buffer.mark();
        buffer.push_sample(sample(2.0));
        buffer.push_sample(sample(3.0));

        let values: Vec<f64> = buffer.samples_since(mark).map(|s| s.value).collect();
        assert_eq!(values, vec![2.0, 3.0]);
        assert_eq!(buffer.samples_lost_since(mark), 0);
    }

    #[test]
    fn test_entries_evicted_before_evaluation_are_counted_as_lost() -> TestResult {
        let mut buffer = EvidenceBuffer::new(2, 2);
        let mark = buffer.mark();
        for port in 1..=5 {
            buffer.push_event(event(port));
        }
        // Kapazität 2: von fünf neuen Events sind nur 4 und 5 noch da.
        assert_eq!(ports_of(buffer.events_since(mark))?, vec![4, 5]);
        assert_eq!(buffer.events_lost_since(mark), 3);

        let later = buffer.mark();
        buffer.push_event(event(6));
        assert_eq!(ports_of(buffer.events_since(later))?, vec![6]);
        assert_eq!(buffer.events_lost_since(later), 0);
        Ok(())
    }

    #[test]
    fn test_a_mark_from_the_future_yields_an_empty_delta() {
        let mut buffer = EvidenceBuffer::new(4, 4);
        buffer.push_event(event(1));
        let mut other = EvidenceBuffer::new(4, 4);
        for port in 0..10 {
            other.push_event(event(port));
        }
        assert_eq!(buffer.events_since(other.mark()).count(), 0);
        assert_eq!(buffer.events_lost_since(other.mark()), 0);
    }

    #[test]
    fn test_freeze_since_sees_each_entry_exactly_once_over_many_cycles() -> TestResult {
        let mut buffer = EvidenceBuffer::new(8, 8);
        buffer.push_event(event(80));
        buffer.push_sample(sample(1.0));

        let mut since = BufferMark::START;
        let mut seen_events = 0_usize;
        let mut seen_samples = 0_usize;
        for _ in 0..10 {
            let delta = buffer
                .freeze_since(since, Timestamp::UNIX_EPOCH)
                .map_err(ctx("well-formed buffer content always encodes"))?;
            seen_events = seen_events
                .checked_add(delta.evidence.events.len())
                .ok_or(TestError::Missing("event count overflow"))?;
            seen_samples = seen_samples
                .checked_add(delta.evidence.samples.len())
                .ok_or(TestError::Missing("sample count overflow"))?;
            assert_eq!(delta.events_lost, 0);
            assert_eq!(delta.samples_lost, 0);
            since = delta.until;
        }
        assert_eq!(seen_events, 1, "a constant ring yields its event once");
        assert_eq!(seen_samples, 1, "a constant ring yields its sample once");
        assert_eq!(buffer.event_len(), 1, "freeze_since never drains the ring");

        buffer.push_event(event(443));
        let delta = buffer
            .freeze_since(since, Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(ports_of(delta.evidence.events.iter())?, vec![443]);
        assert_eq!(delta.until, buffer.mark());
        Ok(())
    }

    #[test]
    fn test_freeze_since_start_equals_freeze_of_the_whole_ring() -> TestResult {
        let mut buffer = EvidenceBuffer::new(4, 4);
        buffer.push_event(event(80));
        buffer.push_sample(sample(1.0));
        let whole = buffer
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        let delta = buffer
            .freeze_since(BufferMark::START, Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(delta.evidence, whole);
        Ok(())
    }

    #[test]
    fn test_default_buffer_uses_documented_capacities() {
        let buffer = EvidenceBuffer::default();
        assert_eq!(
            buffer.sample_capacity(),
            EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY
        );
        assert_eq!(
            buffer.event_capacity(),
            EvidenceBuffer::DEFAULT_EVENT_CAPACITY
        );
    }
}
