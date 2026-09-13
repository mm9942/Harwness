//! Ereignisstrom mit Sequenznummer für Server-Sent Events (SSE).
//!
//! # Verantwortungsbereich
//! `harw-web` streamt Lebenszyklus-Ereignisse (z. B. abgeschlossene
//! Operationsaufrufe) über einen begrenzten, gepufferten Broadcast-Kanal.
//! Jedes ausgelieferte Ereignis trägt eine **monoton steigende
//! Sequenznummer**. Ein Client, dessen Verbindung abreißt und neu verbindet,
//! sieht an einer Lücke in der Sequenz sofort, dass er Ereignisse verpasst
//! hat — ein Strom ohne Nummern wäre von einem lückenhaften nicht zu
//! unterscheiden (dieselbe Anforderung, die `xtask::gates::GateReport`
//! mit seinem `checked`-Feld für Gate-Läufe durchsetzt: „nichts verletzt"
//! muss von „nichts geprüft" unterscheidbar bleiben; hier: „keine Lücke"
//! muss von „keine Sequenznummer, also nicht prüfbar" unterscheidbar
//! bleiben).
//!
//! # Vorbild
//! Strukturell identisch zu `harw_mcp_server::events::McpEventBus`
//! (bounded `tokio::sync::broadcast`, `sequence: u64`,
//! `RecvError::Lagged` als Lückenindikator) — hier ohne
//! Session-Aufteilung, weil `harw-web` (Stand dieses Knotens) einen
//! einzigen, prozessweiten Ereignisstrom exponiert.
//!
//! # Keine Systemuhr
//! Ein [`WebEvent`] trägt bewusst **keinen** Zeitstempel aus
//! Bibliothekscode heraus — Zeitstempel, falls ein Aufrufer sie in
//! [`WebEventKind`] mitgeben will, sind Sache des Aufrufers, nicht dieser
//! Datei.
//!
//! # Nebenläufigkeit
//! [`WebEventBus`] ist `Clone + Send + Sync`; alle Klone teilen denselben
//! `Arc`-Zustand. `publish`/`subscribe` sind async-frei und sperren nur für
//! die Dauer eines internen `Mutex`-Zugriffs.
//!
//! # Fehlertypen
//! [`crate::error::WebError::InvalidEventCapacity`] bei Kapazität `0`;
//! [`WebEventReceiveError`] für Empfangsfehler (Lag/geschlossen).
//!
//! # Woher die `TrustClass` kommt — und wo sie verloren geht
//! AW4-01 trennt gerenderten Kontext in zwei Blöcke nach
//! [`harw_context::TrustClass`]. Ein Bediener, der ein [`WebEvent`] liest,
//! muss unterscheiden können, ob der Text vor ihm eine Anweisung, ein
//! belegtes Beweisstück oder angreiferkontrollierte Daten ist.
//!
//! Das Feld `trust` auf [`WebEventKind::OperationCompleted`] reicht diese Klasse additiv
//! durch — **aber die eigentliche Quelle trägt sie nicht.**
//! [`harw_operations::operation::OpOutput`] besteht ausschließlich aus
//! `text: String`; es gibt dort kein Klassenfeld, aus dem sich etwas
//! herleiten ließe. Genau an dieser Stelle geht die Klasse verloren, nicht
//! in dieser Datei. Diese Crate erfindet deshalb keine Klasse für
//! `OpOutput`-Inhalte — sie setzt die einzige Klasse, die ohne bekannte
//! Herkunft zulässig ist.
//!
//! # Warum die Vorgabe die niedrigste Klasse ist
//! Ein Ausgabewert ohne bekannte Herkunft ist [`harw_context::TrustClass::Data`],
//! nie [`harw_context::TrustClass::Instruction`] — alles andere wäre eine
//! stille Eskalation: eine Oberfläche, die einen unklassifizierten Wert als
//! vertrauenswürdig anzeigt, obwohl niemand das geprüft hat. `Data` ist die
//! einzige Klasse, die eine fehlende Aussage korrekt wiedergibt.
//! [`default_trust_class`] liefert diesen Wert für `#[serde(default)]`, weil
//! [`harw_context::TrustClass`] selbst bewusst kein `Default` implementiert
//! (fremder Typ, fremdes Trait — kein `impl` in dieser Crate möglich, und
//! keins gewollt: die Vorgabe ist eine Entscheidung dieser Oberfläche, keine
//! Eigenschaft der Klasse selbst).
//!
//! # Was die Oberfläche zusätzlich tun muss
//! Damit die Trennung für den Bediener sichtbar wird, muss die UI (nicht
//! diese Crate) `trust` auswerten und den Text entsprechend in den
//! Instruktions- oder den Datenblock rendern — mit den bereitliegenden
//! CSS-Token. Solange in der Praxis fast jede `EvidenceRef`-Konstruktion
//! keinen Digest setzt (siehe K54), wird nahezu jeder Wert `Data` tragen.
//! Das ist keine Schwäche dieser Änderung, sondern ihre Rechtfertigung: der
//! Bediener sieht die Wahrheit statt einer erfundenen Sicherheit.

use std::fmt;
use std::sync::Mutex;

use harw_context::TrustClass;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::WebError;

/// Vorgabewert für das Feld `trust` auf [`WebEventKind::OperationCompleted`],
/// wenn ein Bestandsdatensatz das Feld nicht trägt oder die Quelle keine
/// Klasse liefert.
///
/// # Description
/// Siehe Moduldokumentation „Warum die Vorgabe die niedrigste Klasse ist":
/// ein Wert ohne bekannte Herkunft ist [`TrustClass::Data`], nie
/// [`TrustClass::Instruction`] — alles andere wäre eine stille Eskalation.
///
/// # Returns
/// [`TrustClass::Data`].
fn default_trust_class() -> TrustClass {
    TrustClass::Data
}

/// Fachliche Nutzlast eines [`WebEvent`].
///
/// # Description
/// Bewusst minimal gehalten: `harw-web` reicht nur weiter, was eine Route
/// bereits über [`harw_operations::operation::OpOutput`] geliefert hat, plus
/// dem Namen der aufgerufenen Operation. Kein Markdown, kein aktiver Link —
/// das ist UI-01s Sache. `trust` ist additiv (siehe Moduldokumentation
/// „Woher die `TrustClass` kommt"); ein Bestandsdatensatz ohne dieses Feld
/// bleibt über `#[serde(default)]` lesbar und erhält [`TrustClass::Data`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum WebEventKind {
    /// Eine über `harw-web` aufgerufene Operation ist abgeschlossen.
    OperationCompleted {
        /// Maschinenlesbarer Name der Operation (`OperationMeta::name`).
        operation: String,
        /// `true`, wenn die Ausführung mit `Ok(..)` endete.
        ok: bool,
        /// Vertrauensklasse des Textes in `OpOutput`. `OpOutput` selbst
        /// trägt keine Klasse (siehe Moduldokumentation) — dieses Feld
        /// wird deshalb in der Praxis fast immer [`TrustClass::Data`]
        /// sein, bis eine Quelle mit bekannter Herkunft existiert.
        #[serde(default = "default_trust_class")]
        trust: TrustClass,
    },
    /// Betriebssignal, damit ein Client eine offene SSE-Verbindung von einer
    /// stillen, aber toten Verbindung unterscheiden kann.
    Heartbeat,
}

/// Ein ausgeliefertes Ereignis mit monoton steigender Sequenznummer.
///
/// # Description
/// `sequence` beginnt bei `1` und steigt mit jedem [`WebEventBus::publish`]-Aufruf
/// um genau `1`. Ein Abonnent, der eine [`WebEventReceiveError::Lagged`]
/// erhält, kennt aus `skipped` sogar, wie viele Ereignisse zwischen dem
/// zuletzt gesehenen und dem nächsten fehlen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebEvent {
    /// Monoton steigend, beginnend bei `1`.
    pub sequence: u64,
    /// Die fachliche Nutzlast.
    pub kind: WebEventKind,
}

impl WebEvent {
    /// Kodiert dieses Ereignis als eine einzelne SSE-`data:`-Zeile
    /// (`data: <json>\n\n`), abgeschlossen mit einer Leerzeile gemäß dem
    /// SSE-Protokoll.
    ///
    /// # Returns
    /// Den vollständigen SSE-Rahmen als `String`.
    ///
    /// # Errors
    /// [`WebError::EventEncode`], wenn dieses Ereignis sich nicht als JSON
    /// kodieren lässt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::events::{WebEvent, WebEventKind};
    ///
    /// let event = WebEvent { sequence: 1, kind: WebEventKind::Heartbeat };
    /// let frame = event.to_sse_frame().unwrap();
    /// assert!(frame.starts_with("data: "));
    /// assert!(frame.ends_with("\n\n"));
    /// ```
    pub fn to_sse_frame(&self) -> Result<String, WebError> {
        let json = serde_json::to_string(self)?;
        Ok(format!("data: {json}\n\n"))
    }
}

/// Empfangsfehler eines [`WebEventSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebEventReceiveError {
    /// Der Abonnent war zu langsam; `skipped` Ereignisse wurden verworfen,
    /// bevor er sie lesen konnte. Die nächste erfolgreich gelesene
    /// [`WebEvent::sequence`] liegt entsprechend höher als erwartet —
    /// genau die Lücke, die ein Client erkennen muss.
    Lagged {
        /// Anzahl der übersprungenen Ereignisse.
        skipped: u64,
    },
    /// Der Ereignisbus wurde geschlossen (keine Sender mehr vorhanden).
    Closed,
}

impl fmt::Display for WebEventReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lagged { skipped } => {
                write!(f, "Web-Ereignis-Abonnent hat {skipped} Ereignisse verpasst")
            }
            Self::Closed => f.write_str("Web-Ereignisbus wurde geschlossen"),
        }
    }
}

impl std::error::Error for WebEventReceiveError {}

struct EventBusInner {
    sender: broadcast::Sender<WebEvent>,
    next_sequence: u64,
}

/// Prozessweiter, gepufferter Ereignis-Broadcast mit Sequenznummer.
///
/// # Description
/// Genau ein `tokio::sync::broadcast`-Kanal für den gesamten Prozess. Ein
/// zu langsamer Abonnent verliert die ältesten ungelesenen Ereignisse
/// (begrenzter Puffer) und erhält beim nächsten `recv()` einen
/// [`WebEventReceiveError::Lagged`] mit der genauen Anzahl übersprungener
/// Ereignisse.
///
/// # Concurrency
/// `Clone + Send + Sync`; alle Klone teilen denselben `Arc`-Zustand.
#[derive(Clone)]
pub struct WebEventBus {
    inner: std::sync::Arc<Mutex<EventBusInner>>,
    sender: broadcast::Sender<WebEvent>,
}

impl WebEventBus {
    /// Baut einen neuen Ereignisbus mit gegebener Puffergröße.
    ///
    /// # Arguments
    /// - `capacity` (`usize`): maximale Anzahl ungelesener Ereignisse je
    ///   Abonnent, bevor der älteste Eintrag verdrängt wird.
    ///
    /// # Returns
    /// Ein neuer `WebEventBus`.
    ///
    /// # Errors
    /// [`WebError::InvalidEventCapacity`], wenn `capacity == 0`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::events::WebEventBus;
    ///
    /// let bus = WebEventBus::new(16).unwrap();
    /// assert_eq!(bus.subscriber_count(), 0);
    /// ```
    pub fn new(capacity: usize) -> Result<Self, WebError> {
        if capacity == 0 {
            return Err(WebError::InvalidEventCapacity);
        }
        let (sender, _receiver) = broadcast::channel(capacity);
        Ok(Self {
            inner: std::sync::Arc::new(Mutex::new(EventBusInner {
                sender: sender.clone(),
                next_sequence: 1,
            })),
            sender,
        })
    }

    /// Abonniert den Ereignisstrom.
    ///
    /// # Returns
    /// Eine [`WebEventSubscription`], bereit für [`WebEventSubscription::recv`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::events::WebEventBus;
    ///
    /// let bus = WebEventBus::new(4).unwrap();
    /// let _subscription = bus.subscribe();
    /// assert_eq!(bus.subscriber_count(), 1);
    /// ```
    #[must_use]
    pub fn subscribe(&self) -> WebEventSubscription {
        WebEventSubscription {
            receiver: self.sender.subscribe(),
        }
    }

    /// Veröffentlicht ein Ereignis und weist ihm die nächste Sequenznummer zu.
    ///
    /// # Arguments
    /// - `kind` (`WebEventKind`): die fachliche Nutzlast.
    ///
    /// # Returns
    /// Das vollständige [`WebEvent`] inklusive der zugewiesenen Sequenznummer.
    /// `Ok` auch dann, wenn aktuell kein Abonnent lauscht (ein
    /// `broadcast`-Kanal ohne Empfänger verwirft die Nachricht stillschweigend;
    /// die Sequenznummer steigt trotzdem weiter, damit ein später
    /// abonnierender Client die Lücke an der ersten erhaltenen Sequenznummer
    /// erkennt).
    ///
    /// # Concurrency
    /// Sperrt intern nur für die Dauer der Sequenznummer-Vergabe.
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::events::{WebEventBus, WebEventKind};
    ///
    /// let bus = WebEventBus::new(4).unwrap();
    /// let event = bus.publish(WebEventKind::Heartbeat);
    /// assert_eq!(event.sequence, 1);
    /// ```
    pub fn publish(&self, kind: WebEventKind) -> WebEvent {
        // Poisoned nur nach einem Panic während eines Locks — dieser Bus
        // führt selbst keine Logik aus, die während des Locks paniken kann;
        // ein vergifteter Lock wird defensiv wie ein frischer behandelt statt
        // den Aufrufer mit einem zweiten Fehlerpfad zu belasten.
        let mut inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        let event = WebEvent {
            sequence: inner.next_sequence,
            kind,
        };
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        let _ = inner.sender.send(event.clone());
        event
    }

    /// Anzahl aktuell aktiver Abonnenten.
    ///
    /// # Returns
    /// `usize` — Anzahl noch nicht verworfener [`WebEventSubscription`]en.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

/// Ein aktives Abonnement des [`WebEventBus`].
pub struct WebEventSubscription {
    receiver: broadcast::Receiver<WebEvent>,
}

impl WebEventSubscription {
    /// Wartet auf das nächste Ereignis.
    ///
    /// # Returns
    /// Das nächste [`WebEvent`] in Sequenzreihenfolge.
    ///
    /// # Errors
    /// - [`WebEventReceiveError::Lagged`]: der Puffer wurde überschrieben,
    ///   bevor dieses Abonnement lesen konnte; `skipped` nennt die genaue
    ///   Anzahl übersprungener Ereignisse.
    /// - [`WebEventReceiveError::Closed`]: der Bus hat keine Sender mehr.
    ///
    /// # Concurrency
    /// `async fn`; sicher parallel zu anderen Abonnements desselben Busses.
    pub async fn recv(&mut self) -> Result<WebEvent, WebEventReceiveError> {
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(skipped) => {
                WebEventReceiveError::Lagged { skipped }
            }
            broadcast::error::RecvError::Closed => WebEventReceiveError::Closed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{WebEvent, WebEventBus, WebEventKind, WebEventReceiveError};
    use crate::error::WebError;
    use harw_context::TrustClass;

    fn completed(name: &str) -> WebEventKind {
        WebEventKind::OperationCompleted {
            operation: name.to_owned(),
            ok: true,
            trust: TrustClass::Data,
        }
    }

    /// Der wichtigste Test dieser Änderung: ein Ausgabewert ohne bekannte
    /// Herkunft trägt `Data`, nicht irgendeine andere Klasse — siehe
    /// Moduldokumentation „Warum die Vorgabe die niedrigste Klasse ist".
    #[test]
    fn test_operation_completed_without_known_origin_defaults_to_data() {
        let json = serde_json::json!({
            "type": "operation_completed",
            "operation": "noop",
            "ok": true,
        });
        let kind: WebEventKind = serde_json::from_value(json).unwrap();
        match kind {
            WebEventKind::OperationCompleted { trust, .. } => {
                assert_eq!(trust, TrustClass::Data);
            }
            other => panic!("Erwartet OperationCompleted, war: {other:?}"),
        }
    }

    /// Eine deklarierte Klasse kommt unverändert durch — die Durchreichung
    /// ist additiv, keine Ersetzung.
    #[test]
    fn test_operation_completed_declared_trust_class_round_trips() {
        let kind = WebEventKind::OperationCompleted {
            operation: "noop".to_owned(),
            ok: true,
            trust: TrustClass::Evidence,
        };
        let json = serde_json::to_value(&kind).unwrap();
        let decoded: WebEventKind = serde_json::from_value(json).unwrap();
        assert_eq!(kind, decoded);
    }

    /// Ein Bestandsdatensatz ganz ohne das neue Feld bleibt lesbar
    /// (`#[serde(default)]`), unabhängig davon, welche anderen Felder
    /// gesetzt sind.
    #[test]
    fn test_legacy_record_without_trust_field_remains_readable() {
        let json = serde_json::json!({
            "sequence": 7,
            "kind": {
                "type": "operation_completed",
                "operation": "legacy",
                "ok": false,
            },
        });
        let event: WebEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.sequence, 7);
        match event.kind {
            WebEventKind::OperationCompleted { trust, ok, .. } => {
                assert_eq!(trust, TrustClass::Data);
                assert!(!ok);
            }
            other => panic!("Erwartet OperationCompleted, war: {other:?}"),
        }
    }

    /// Ein unbekanntes Feld im Wire-Typ wird abgewiesen
    /// (`#[serde(deny_unknown_fields)]`).
    #[test]
    fn test_unknown_field_in_operation_completed_is_rejected() {
        let json = serde_json::json!({
            "type": "operation_completed",
            "operation": "noop",
            "ok": true,
            "unexpected_field": "sollte abgewiesen werden",
        });
        let result: Result<WebEventKind, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_new_with_zero_capacity_is_rejected() {
        assert!(matches!(
            WebEventBus::new(0),
            Err(WebError::InvalidEventCapacity)
        ));
    }

    #[test]
    fn test_publish_sequence_starts_at_one_and_increments() {
        let bus = WebEventBus::new(8).unwrap();
        let first = bus.publish(completed("a"));
        let second = bus.publish(completed("b"));
        let third = bus.publish(completed("c"));
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(third.sequence, 3);
    }

    #[tokio::test]
    async fn test_subscriber_receives_events_in_sequence_order() {
        let bus = WebEventBus::new(8).unwrap();
        let mut subscription = bus.subscribe();
        bus.publish(completed("a"));
        bus.publish(completed("b"));
        let first = subscription.recv().await.unwrap();
        let second = subscription.recv().await.unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
    }

    #[tokio::test]
    async fn test_slow_subscriber_sees_a_visible_gap_via_lagged() {
        // Kapazität 2: ein drittes Ereignis vor dem ersten `recv()` verdrängt
        // das älteste — der Abonnent MUSS das an `Lagged` erkennen können.
        let bus = WebEventBus::new(2).unwrap();
        let mut subscription = bus.subscribe();
        bus.publish(completed("a"));
        bus.publish(completed("b"));
        bus.publish(completed("c"));

        match subscription.recv().await {
            Err(WebEventReceiveError::Lagged { skipped }) => assert_eq!(skipped, 1),
            other => panic!("Erwartet Lagged, war: {other:?}"),
        }
        let next = subscription.recv().await.unwrap();
        // Die Lücke ist sichtbar: die nächste erhaltene Sequenznummer ist 2,
        // nicht 1 — der Client kann daraus ableiten, dass er das Ereignis
        // mit sequence == 1 nie gesehen hat.
        assert_eq!(next.sequence, 2);
    }

    #[test]
    fn test_subscriber_count_reflects_active_and_dropped_subscriptions() {
        let bus = WebEventBus::new(4).unwrap();
        assert_eq!(bus.subscriber_count(), 0);
        let subscription = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(subscription);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn test_publish_without_subscribers_still_advances_sequence() {
        let bus = WebEventBus::new(4).unwrap();
        let first = bus.publish(completed("a"));
        let second = bus.publish(completed("b"));
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
    }

    #[test]
    fn test_web_event_to_sse_frame_has_data_prefix_and_blank_line() {
        let event = WebEvent {
            sequence: 7,
            kind: WebEventKind::Heartbeat,
        };
        let frame = event.to_sse_frame().unwrap();
        assert!(frame.starts_with("data: "));
        assert!(frame.ends_with("\n\n"));
        assert!(frame.contains("\"sequence\":7"));
    }

    #[test]
    fn test_web_event_bus_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WebEventBus>();
    }
}
