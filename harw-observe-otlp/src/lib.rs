//! OTLP-Brücke über HTTP/JSON: puffert `harw_observe`-Messwerte und liefert
//! sie stapelweise als OTLP/JSON an einen externen Collector.
//!
//! # Verantwortungsbereich
//! Der dritte echte Implementierer von [`harw_observe::TelemetrySink`] nach
//! `harw-observe-file::FileSink` und `harw-observe-prom::PromSink` (Vertrag
//! A.3/A.6, `docs/design/build-history.md`). Besitzt [`OtlpSink`] (Pufferung,
//! Stapelbildung, die `security.*`/`warden.*`-Sperre), [`OtlpConfig`] +
//! [`HeaderEntry`] (Konfiguration), [`OtlpClock`] + [`FixedClock`]
//! (injizierte Zeitquelle), [`OtlpTransport`] + [`RecordingTransport`]
//! (Zustellabstraktion) und [`OtlpError`] (Vertrag §H.1). Der interne
//! OTLP/JSON-Rumpfaufbau (`ExportMetricsServiceRequest` und alles darunter)
//! ist vollständig `pub(crate)` — siehe „Die OTel-Adapter-Isolation" unten.
//!
//! # Die OTel-Adapter-Isolation — und warum es sie gibt
//! Der Workspace kennt Doktrin **D5**: keine Pre-1.0-Abhängigkeit in einer
//! öffentlichen Signatur. Ihre Begründung nennt ausdrücklich die
//! OpenTelemetry-Crate-Familie (`opentelemetry`, `opentelemetry-otlp`,
//! `opentelemetry_sdk`, …) als den Fall, den D5 verhindern soll:
//! versionsgleichgeschaltet über mehrere Crates hinweg, mit Brüchen in
//! Minor-Versionen, und mit großer Typfläche, die sich in jede Signatur
//! einschleicht, die auch nur einen ihrer Typen entgegennimmt oder
//! zurückgibt. Deshalb gilt ab diesem Knoten (AW7) ein eigenes CI-Gate
//! „OTel-Adapter-Isolation": **kein OTel-Typ, kein `opentelemetry*`-Typ,
//! kein Typ irgendeiner Fremdcrate** erscheint in einer öffentlichen
//! Signatur dieser Crate. Diese Crate ist eine **Mauer**, keine Brücke im
//! Sinne von „beide Seiten sichtbar" — deshalb sind sowohl der interne
//! OTLP/JSON-Rumpf (`crate::schema`) als auch das Zustellergebnis
//! ([`OtlpTransport::send_batch`]) ausschließlich über eigene Typen
//! (`Vec<u8>`, [`OtlpError`]) sichtbar, nie über einen fremden Typ.
//! Konsequent zu Ende gedacht heißt das: diese Crate zieht **überhaupt
//! keine** OpenTelemetry-Crate als Abhängigkeit — nicht einmal
//! `opentelemetry-otlp` intern hinter der Mauer —, weil sonst jede künftige
//! Bruchversion dieser Familie diese Crate treffen könnte, auch ohne dass
//! ein einziger ihrer Typen je eine öffentliche Signatur erreicht.
//!
//! # Die Kodierungsentscheidung: OTLP/JSON, nicht gRPC, nicht `prost`
//! Drei realistische Wege standen zur Wahl (Begründung im Detail:
//! `crate::schema`-Moduldoc):
//! - **`opentelemetry-otlp` + `opentelemetry_sdk`** (gRPC oder HTTP): die
//!   volle Familie — verboten unter der OTel-Adapter-Isolation oben.
//! - **`prost` + `prost-types`** mit generierten OTLP-Protobuf-Typen:
//!   mittlere Fläche, aber `prost-build` braucht `protoc` als externes
//!   Werkzeug im Build — ein `build.rs` mit Systemabhängigkeit, verboten
//!   unter **D3**.
//! - **OTLP/JSON**: der OTLP-Standard definiert eine proto3-JSON-Abbildung
//!   desselben `ExportMetricsServiceRequest`. Nur `serde`/`serde_json`
//!   (bereits Workspace-Abhängigkeiten), kein `build.rs`, kein `protoc`.
//!
//! Diese Crate wählt **OTLP/JSON** und erfüllt damit D5 restlos, ohne D3 zu
//! berühren. Der Preis: ein größerer Rumpf auf der Leitung als binäres
//! Protobuf, und manche Collector-Konfigurationen akzeptieren ausschließlich
//! `application/x-protobuf` statt `application/json` — beides eine bewusste
//! Abwägung, keine Unkenntnis. Auch der gRPC-Transport selbst entfällt aus
//! demselben Grund wie die `opentelemetry-otlp`-Familie: gRPC zöge `tonic` +
//! `hyper` + `prost` + `tower` in den Baum, eine weitere große,
//! versionsgleichgeschaltete Fläche für ein Protokoll, das OTLP/HTTP als
//! einfachen `POST` auf `/v1/metrics` mit einem Rumpf löst.
//!
//! Feldnamen und -formen des OTLP/JSON-Rumpfs sind gegen die offizielle
//! `open-telemetry/opentelemetry-proto`-Definition geprüft; die genaue
//! Quellenangabe (mit URLs, abgerufen 2026-09-01) steht im
//! `crate::schema`-Moduldoc.
//!
//! # Metriken zuerst, Spans später
//! Dieser Knoten trägt ausschließlich Metriken. `MetricKey` ist seit AW3-04
//! eingefroren und hat einen Golden-Test (`harw-observe-prom`); diese Crate
//! übernimmt dessen bereits beantwortete Fragen unverändert (Namensableitung,
//! Label-Sortierung, `MetricKind`-Behandlung — siehe `crate::schema`-Moduldoc)
//! statt sie ein zweites Mal, möglicherweise abweichend, zu beantworten: zwei
//! Sinks, die denselben `MetricKey` unterschiedlich benennen, sind eine
//! Fehlerquelle, kein Feature. Spans hängen an `harw_observe::TraceContext`,
//! dessen Erzeugerkette noch unvollständig ist, und bleiben einem späteren
//! Knoten vorbehalten.
//!
//! # Puffern und Zustellen
//! [`OtlpSink::record`] darf den aufrufenden Pfad nicht blockieren — ein
//! Sink, der synchron ein HTTP-`POST` ausführt, hängt den Turn-Loop an einen
//! fremden Collector. `record()` puffert deshalb ausschließlich (hinter
//! einem `std::sync::Mutex`, siehe `crate::sink`-Moduldoc) und rührt nie ein
//! Netzwerk an; erst [`OtlpSink::flush`] — dessen `TelemetrySink`-Vertrag
//! ohnehin „erzwingt das Ausschreiben" verspricht, also blockieren darf —
//! entnimmt dem Puffer Stapel von höchstens `OtlpConfig::batch_size`
//! Datenpunkten und liefert jeden über den injizierten [`OtlpTransport`] aus.
//!
//! **Überlaufverhalten**: der Puffer wächst nie unbegrenzt. Erreicht er
//! `OtlpConfig::max_buffer` Einträge, wird ein neu eingehender Messpunkt
//! verworfen — nie blockierend gewartet, nie unbegrenzt aufgestaut — und
//! gezählt ([`OtlpSink::buffer_overflow_dropped_count`]): eine still
//! verschwindende Metrik ist schlimmer als eine fehlende, sichtbar gezählte.
//! Scheitert die Zustellung eines bereits gebildeten Stapels (ein
//! [`OtlpTransport::send_batch`]-Fehler oder ein Serialisierungsfehler),
//! gilt dieser Stapel ebenfalls als verworfen — kein erneuter Zustellversuch
//! — und wird über [`OtlpSink::send_failure_count`] gezählt. Nicht endliche
//! Gleitkommawerte (`NaN`/`±Inf`, die JSON anders als das
//! Prometheus-Textformat nicht kodieren kann) und Zeitstempel außerhalb von
//! OTLPs `u64`-Nanosekundenbereich werden ebenso je Datenpunkt verworfen und
//! gezählt ([`OtlpSink::non_finite_dropped_count`],
//! [`OtlpSink::timestamp_out_of_range_dropped_count`]), statt den ganzen
//! Stapel ungültig zu machen.
//!
//! # Verhältnis zur `security.*`/`warden.*`-Routingregel (AW5-06)
//! `harw_observe::routing` führt eine Namensraum-Regel: Metriken unter
//! [`harw_observe::PROTECTED_NAMESPACES`] (`security.*`, `warden.*`) gehen
//! ohne ausdrückliche Operator-Bestätigung
//! (`harw_observe::ExportApproval`) ausschließlich an den `default_sink`
//! eines `harw_observe::RoutingSink` — im Betrieb der File-Sink. Diese Regel
//! lebt vollständig in `RoutingSink`, das **vor** jedem Blattsink wie diesem
//! sitzt; ob sie in der Praxis greift, hängt allein davon ab, ob die
//! Kompositionsstelle (typischerweise `harw-cli`) `RoutingSink` tatsächlich
//! zwischen die Emittenten und diesen OTLP-Sink schaltet. Nichts im Typsystem
//! erzwingt das: nichts hindert eine Fehlkonfiguration daran, [`OtlpSink`]
//! direkt (ohne `RoutingSink` davor) oder sogar als `RoutingSink`s eigenen
//! `default_sink` zu verkabeln — beides hebt den Schutz vollständig auf.
//! **`RoutingSink` schützt diesen Sink deshalb nicht automatisch** — sie
//! schützt ihn nur, wenn die Komposition außerhalb dieser Crate es richtig
//! macht, eine Betriebsdisziplin, keine Compile-Zeit-Garantie.
//!
//! Ein OTLP-Sink, der Sicherheitstelemetrie an einen externen Collector
//! schickt, ist genau der Vorfall, den die Regel verhindern soll — ein
//! Angreifer, der den Collector einsehen kann, erführe, ob und wie er bemerkt
//! wurde. Diese Crate verlässt sich deshalb nicht auf die Verkabelung:
//! [`OtlpSink::record`] prüft `key.name` selbst gegen
//! [`harw_observe::PROTECTED_NAMESPACES`] (`crate::sink`-Moduldoc,
//! Begründung der `starts_with`-Grenzprüfung) und verwirft einen Treffer
//! bedingungslos, **bevor** er den internen Puffer erreicht — unabhängig
//! davon, ob eine `RoutingSink`-Freigabe existiert. Das ist bewusst
//! absoluter als `harw_observe::ExportApproval`, das generisch für
//! *irgendein* Exportziel gedacht ist, nicht speziell für Netzwerkegress
//! geprüft: dieser Sink verweigert `security.*`/`warden.*` unbedingt, auch
//! wenn eine `RoutingSink`-Freigabe ausdrücklich genau diesen Sink als
//! Exportziel benennt. Ein Betreiber, der Sicherheitstelemetrie wirklich über
//! OTLP an einen Collector exportieren will, braucht dafür eine bewusste
//! Änderung an dieser Crate, keine Konfigurationszeile. Jeder Treffer wird
//! über den Nullzähler [`OTLP_PROTECTED_NAMESPACE_BLOCKED`] gezählt
//! (dieselbe Mechanik wie `harw_observe::routing::SECURITY_METRIC_LEAKED`) —
//! derselbe Tiefenverteidigungs-Reflex, den `RoutingSink` selbst schon für
//! einen wirkungslosen `route()`-Eintrag zeigt: eine zweite, unabhängige
//! Durchsetzung derselben Regel ist hier Absicht, kein Versehen.
//!
//! # Keine Systemuhr
//! `harw_observe::TelemetrySink::record`s Signatur ist eingefroren (Vertrag
//! A.3) und trägt keinen `timestamp`-Parameter; `harw_observe::MetricValue`
//! trägt ebenfalls keinen. Ein Zeitstempel für einen Datenpunkt kann deshalb
//! nur „hereingereicht" werden: über die bei der Konstruktion injizierte
//! [`OtlpClock`] (siehe `crate::clock`-Moduldoc). Diese Crate ruft an keiner
//! Stelle `jiff::Timestamp::now()` auf, auch nicht als bequeme
//! Voreinstellung — die Kompositionsstelle liefert eine echte,
//! systemuhrbasierte Implementierung. Die Umrechnung des injizierten
//! `jiff::Timestamp` in OTLPs vorzeichenlose 64-Bit-Nanosekunden seit der
//! Epoche (`crate::schema::timestamp_to_unix_nanos`) prüft Über- und
//! Unterlauf explizit über `TryFrom` statt über ein stillschweigend
//! umschlagendes Cast — getestet an der Epoche, weit in der Zukunft
//! (innerhalb und außerhalb des `u64`-Bereichs) und vor der Epoche
//! (negativ).
//!
//! # Was gebaut wurde — AW7-02: der echte Transport
//! [`OtlpTransport`] bleibt das eine Trait, das jede Zustellung hinter einer
//! schmalen `&[u8]`-Signatur kapselt. Neben [`RecordingTransport`] (weiterhin
//! die Test-/Aufzeichnungsimplementierung ohne Netzzugriff) trägt diese Crate
//! jetzt [`HttpTransport`]: liefert einen Stapel per HTTP `POST` an einen
//! externen OTLP-Collector aus, auf derselben `hyper`-Familie wie
//! `harw-mcp-server`/`harw-web` (nur clientseitige Feature-Flags derselben,
//! bereits im Workspace aufgelösten Crate-Versionen — **null neue Crates**
//! im `Cargo.lock`), ohne TLS (nur `http://`, siehe
//! `crate::http_transport`-Moduldoc, Abschnitt „TLS", für die Begründung und
//! die gemeldete Lücke) und ohne eine zweite HTTP-Bibliothek wie `reqwest`
//! zu ziehen. Vollständige Begründung, das Fehlschlagverhalten, die
//! Brücke zwischen synchronem `flush()` und `hyper`s async-Client sowie die
//! Testabdeckung ohne echtes Netz stehen im `crate::http_transport`-Moduldoc.
//! [`OtlpConfig`] trug `endpoint`/`headers` bereits vor diesem Knoten (K19,
//! `deny_unknown_fields`) — [`HttpTransport`] ist der erste Konsument dieser
//! Felder.
//!
//! # Nebenläufigkeit
//! [`OtlpSink`] ist `Send + Sync + Debug` (Vertrag A.3); sein Puffer liegt
//! hinter einem `std::sync::Mutex` (`TelemetrySink::record` bekommt nur
//! `&self`), jeder Zähler in einem eigenen `AtomicU64` — siehe
//! `crate::sink`-Moduldoc. [`OtlpTransport`] und [`OtlpClock`] verlangen
//! `Send + Sync + Debug`, damit Implementierungen über `Arc<dyn _>` geteilt
//! werden können.
//!
//! # Fehler
//! [`OtlpError`] — entsteht ausschließlich intern (Zeitumrechnung,
//! JSON-Serialisierung, Transportfehler); `OtlpSink::record`/`::flush` geben
//! `()` zurück (Vertrag A.3) und zählen jede Ursache statt sie zu
//! propagieren.
//!
//! # Examples
//! ```
//! use std::sync::Arc;
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_otlp::{FixedClock, OtlpConfig, OtlpSink, RecordingTransport};
//!
//! const JOBS_COMPLETED: MetricKey = MetricKey {
//!     name: "jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! let transport = Arc::new(RecordingTransport::new());
//! let config = OtlpConfig {
//!     endpoint: "http://127.0.0.1:4318/v1/metrics".to_owned(),
//!     headers: Vec::new(),
//!     batch_size: 100,
//!     max_buffer: 10_000,
//!     resource_service_name: "harw-sentinel".to_owned(),
//! };
//! let sink = OtlpSink::new(
//!     Arc::clone(&transport) as Arc<dyn harw_observe_otlp::OtlpTransport>,
//!     Arc::new(FixedClock::new(jiff::Timestamp::UNIX_EPOCH)),
//!     config,
//! );
//!
//! sink.record(&JOBS_COMPLETED, MetricValue::Count(1), &[]);
//! sink.flush();
//! assert_eq!(transport.sent_batches().len(), 1);
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entsteht in
//! Knoten **AW7-02**; Ebene **L2** im Zielgraphen.

mod clock;
mod config;
mod error;
mod http_transport;
mod schema;
mod sink;
mod transport;

pub use clock::{FixedClock, OtlpClock};
pub use config::{HeaderEntry, OtlpConfig};
pub use error::{OtlpError, OtlpResult};
pub use http_transport::HttpTransport;
pub use sink::{OTLP_PROTECTED_NAMESPACE_BLOCKED, OtlpSink};
pub use transport::{OtlpTransport, RecordingTransport};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
