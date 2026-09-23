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
//! # Exportierte Typen
//! [`EvidenceBuffer`].
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
    samples: VecDeque<HostSample>,
    sample_capacity: usize,
    events: VecDeque<SecurityEvent>,
    event_capacity: usize,
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
            events: VecDeque::with_capacity(event_capacity),
            event_capacity,
        }
    }

    /// Fügt einen Messwert hinzu und verdrängt bei Bedarf den ältesten.
    ///
    /// # Description
    /// Ist der Sample-Ring bereits an seiner Kapazität, wird zuerst der
    /// älteste Eintrag entfernt, bevor `sample` angehängt wird — der Ring
    /// wächst nie über [`Self::sample_capacity`] hinaus.
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
    }

    /// Fügt ein Ereignis hinzu und verdrängt bei Bedarf das älteste.
    ///
    /// # Description
    /// Analog zu [`Self::push_sample`], für den unabhängigen Event-Ring
    /// (siehe Moduldoku, Abschnitt "Zwei getrennte Ringe, nicht einer").
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
    use super::EvidenceBuffer;
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
