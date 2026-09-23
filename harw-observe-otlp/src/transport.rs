//! Zustellabstraktion für OTLP/JSON-Stapel — kapselt den Netzzugriff hinter
//! einem eigenen Trait.
//!
//! # Verantwortungsbereich
//! Trägt [`OtlpTransport`] und [`RecordingTransport`]. Der echte,
//! netzwerkfähige Implementierer ([`crate::HttpTransport`], HTTP-`POST`
//! ohne TLS) lebt in `crate::http_transport` — siehe dessen Moduldoc für die
//! Begründung der HTTP-Bibliothek, das fehlende TLS und das
//! Fehlschlagverhalten. [`RecordingTransport`] bleibt daneben bestehen: sie
//! zeichnet jeden zugestellten Stapel im Speicher auf, ohne je einen Socket
//! zu öffnen, und macht [`crate::OtlpSink`] damit vollständig ohne Netz
//! testbar (siehe Crate-Doc, Abschnitt „Puffern und Zustellen").
//!
//! # Warum die Trait-Signatur nur `&[u8]` trägt
//! [`OtlpTransport::send_batch`] nimmt rohe, bereits serialisierte Bytes
//! entgegen — keinen Endpunkt, keine Header, keinen HTTP-Status.
//! [`crate::HttpTransport`] bindet Endpunkt und Header bereits bei ihrer
//! eigenen Konstruktion ([`crate::HttpTransport::new`]) und meldet einen
//! Fehler ausschließlich über [`crate::OtlpError::Send`] mit einer
//! Klartext-`reason` — nie über den fremden Fehlertyp der HTTP-Bibliothek
//! selbst. Andernfalls zöge diese Implementierung zwangsläufig einen
//! Pre-1.0-Fremdcrate-Typ (`hyper`/`hyper-util`) in diese Trait-Signatur und
//! damit in eine öffentliche Signatur dieser Crate — genau das, wovor die
//! OTel-Adapter-Isolation (Crate-Doc) schützt, auch für eine Abhängigkeit,
//! die nicht aus der OpenTelemetry-Familie stammt.
//!
//! # Nebenläufigkeit
//! [`OtlpTransport`] verlangt `Send + Sync + Debug`, damit eine
//! Implementierung über `Arc<dyn OtlpTransport>` geteilt und aus jedem
//! Thread aufgerufen werden kann — [`crate::OtlpSink::flush`] kann
//! aus beliebigen Threads gleichzeitig aufgerufen werden (Vertrag A.3).
//! [`RecordingTransport`] hält ihren gesamten Zustand hinter einem
//! `std::sync::Mutex`; [`crate::HttpTransport`]s Nebenläufigkeit steht in
//! dessen eigenem Moduldoc.
//!
//! # Fehler
//! [`crate::OtlpError::Send`] — von [`RecordingTransport`] nur
//! erzeugt, wenn ein Test das über
//! [`RecordingTransport::fail_next_send`] ausdrücklich verlangt; von
//! [`crate::HttpTransport`] bei jedem tatsächlichen Zustellfehlschlag (siehe
//! dessen Moduldoc, Abschnitt „Fehlschlag").
//!
//! # Examples
//! ```
//! use harw_observe_otlp::{OtlpTransport, RecordingTransport};
//!
//! let transport = RecordingTransport::new();
//! transport.send_batch(b"{}").unwrap();
//! assert_eq!(transport.sent_batches(), vec![b"{}".to_vec()]);
//! ```

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::OtlpError;

/// Zustellt einen bereits serialisierten OTLP/JSON-Stapel.
///
/// Siehe Moduldoc für die Begründung der schmalen `&[u8]`-Signatur und dafür,
/// warum diese Crate keine echte, netzwerkfähige Implementierung mitbringt.
pub trait OtlpTransport: Send + Sync + fmt::Debug {
    /// Versucht, `payload` (ein serialisiertes
    /// `ExportMetricsServiceRequest`, siehe `crate::schema`) zuzustellen.
    ///
    /// # Arguments
    /// - `payload` (`&[u8]`): die rohen OTLP/JSON-Bytes eines Stapels.
    ///
    /// # Returns
    /// `Ok(())`, wenn der Stapel als zugestellt gilt.
    ///
    /// # Errors
    /// - [`crate::OtlpError::Send`]: die Zustellung ist
    ///   fehlgeschlagen; [`crate::OtlpSink::flush`] zählt diesen Fall,
    ///   statt ihn zu propagieren (Vertrag A.3, zweite Festlegung), und der
    ///   betroffene Stapel gilt als verworfen — kein erneuter
    ///   Zustellversuch.
    ///
    /// # Panics
    /// Implementierungen sollen niemals paniken.
    ///
    /// # Concurrency
    /// Wird aus beliebigen Threads gleichzeitig aufgerufen, ohne dass der
    /// Aufrufer eine Sperre hält.
    fn send_batch(&self, payload: &[u8]) -> Result<(), OtlpError>;
}

/// Eine aufzeichnende Testimplementierung von [`OtlpTransport`].
///
/// Öffnet nie einen Socket: jeder zugestellte Stapel landet unverändert in
/// einem internen `Vec`, abrufbar über [`RecordingTransport::sent_batches`].
/// Macht [`crate::OtlpSink`] vollständig ohne Netz testbar (siehe
/// Crate-Doc).
#[derive(Debug, Default)]
pub struct RecordingTransport {
    sent: Mutex<Vec<Vec<u8>>>,
    fail_next: AtomicBool,
}

impl RecordingTransport {
    /// Baut eine leere aufzeichnende Testimplementierung.
    ///
    /// # Returns
    /// Eine [`RecordingTransport`] ohne bisher aufgezeichnete Stapel.
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::RecordingTransport;
    ///
    /// let transport = RecordingTransport::new();
    /// assert!(transport.sent_batches().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Alle bisher zugestellten Stapel, in Zustellreihenfolge.
    ///
    /// # Returns
    /// Eine Kopie jedes bisher über [`OtlpTransport::send_batch`]
    /// entgegengenommenen Rumpfs.
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::{OtlpTransport, RecordingTransport};
    ///
    /// let transport = RecordingTransport::new();
    /// transport.send_batch(b"a").unwrap();
    /// transport.send_batch(b"b").unwrap();
    /// assert_eq!(transport.sent_batches(), vec![b"a".to_vec(), b"b".to_vec()]);
    /// ```
    #[must_use]
    pub fn sent_batches(&self) -> Vec<Vec<u8>> {
        self.lock_sent().clone()
    }

    /// Lässt den nächsten [`OtlpTransport::send_batch`]-Aufruf einmalig
    /// fehlschlagen, um das Fehlerzählungsverhalten von
    /// [`crate::OtlpSink::flush`] deterministisch zu testen.
    ///
    /// # Arguments
    /// - `fail` (`bool`): `true` lässt genau den nächsten Aufruf mit
    ///   [`crate::OtlpError::Send`] fehlschlagen; danach setzt sich
    ///   das Flag automatisch zurück.
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::{OtlpTransport, RecordingTransport};
    ///
    /// let transport = RecordingTransport::new();
    /// transport.fail_next_send(true);
    /// assert!(transport.send_batch(b"x").is_err());
    /// assert!(transport.send_batch(b"y").is_ok(), "the flag resets after one use");
    /// ```
    pub fn fail_next_send(&self, fail: bool) {
        self.fail_next.store(fail, Ordering::Relaxed);
    }

    // Nimmt die interne Sperre; bei Vergiftung (ein anderer Thread ist unter
    // Halten der Sperre panisch geworden) wird der zuletzt bekannte Zustand
    // trotzdem übernommen (Muster: `harw-observe-file::FileSink::lock_state`).
    fn lock_sent(&self) -> std::sync::MutexGuard<'_, Vec<Vec<u8>>> {
        match self.sent.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl OtlpTransport for RecordingTransport {
    fn send_batch(&self, payload: &[u8]) -> Result<(), OtlpError> {
        if self.fail_next.swap(false, Ordering::Relaxed) {
            return Err(OtlpError::Send {
                reason: "RecordingTransport: simulated failure for tests".to_owned(),
            });
        }
        self.lock_sent().push(payload.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_recording_transport_starts_empty() {
        let transport = RecordingTransport::new();
        assert!(transport.sent_batches().is_empty());
    }

    #[test]
    fn test_recording_transport_records_batches_in_order() -> TestResult {
        let transport = RecordingTransport::new();
        transport
            .send_batch(b"first")
            .map_err(ctx("erster send_batch"))?;
        transport
            .send_batch(b"second")
            .map_err(ctx("zweiter send_batch"))?;
        assert_eq!(
            transport.sent_batches(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        Ok(())
    }

    #[test]
    fn test_recording_transport_fail_next_send_fails_exactly_once() {
        let transport = RecordingTransport::new();
        transport.fail_next_send(true);

        let first = transport.send_batch(b"x");
        assert!(matches!(first, Err(OtlpError::Send { .. })));

        let second = transport.send_batch(b"y");
        assert!(second.is_ok());
        assert_eq!(transport.sent_batches(), vec![b"y".to_vec()]);
    }

    #[test]
    fn test_recording_transport_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + fmt::Debug>() {}
        assert_bounds::<RecordingTransport>();
    }

    #[test]
    fn test_recording_transport_usable_through_arc_dyn_otlp_transport() -> TestResult {
        use std::sync::Arc;
        let transport: Arc<dyn OtlpTransport> = Arc::new(RecordingTransport::new());
        transport
            .send_batch(b"payload")
            .map_err(ctx("send_batch über Arc<dyn OtlpTransport>"))?;
        Ok(())
    }
}
