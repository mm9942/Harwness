//! Echter Netzwerktransport für [`crate::OtlpTransport`]: liefert einen
//! bereits serialisierten OTLP/JSON-Stapel per HTTP `POST` an einen externen
//! Collector aus.
//!
//! # Verantwortungsbereich
//! Trägt [`HttpTransport`], den ersten echten (netzwerkfähigen) Implementierer
//! von [`crate::OtlpTransport`] dieser Crate, neben der weiterhin bestehenden
//! [`crate::RecordingTransport`] (Test-/Aufzeichnungsimplementierung, siehe
//! `crate::transport`-Moduldoc). Baut Endpunkt, Header und Verbindung einmalig
//! bei [`HttpTransport::new`] auf; [`HttpTransport::send_batch`] eröffnet nie
//! selbst eine neue Verbindung, sondern nutzt den bei der Konstruktion
//! gebauten, poolenden HTTP-Client wieder.
//!
//! # Die HTTP-Bibliothek — und warum genau diese, mit null neuen Crates
//! Der Workspace hat bereits zwei HTTP-Konsumenten: `harw-mcp-server` und
//! `harw-web` (Serverseite, `hyper` + `hyper-util` + `http-body-util` +
//! `bytes` + `tokio`, Feature-Flags `"server"`/`"http1"`) sowie
//! `harw-provider-http` (Clientseite, `reqwest` mit `rustls-tls`, das intern
//! selbst auf `hyper`s Client-Maschinerie aufbaut). Eine dritte, eigene
//! HTTP-Bibliothek für diese Crate wäre genau die Vervielfachung, die dieser
//! Leitfaden verbietet; `reqwest` erneut zu ziehen bringt zusätzlich eine
//! deutlich größere Fläche (eigener Redirect-/Cookie-/Multipart-Unterbau) als
//! nötig für einen einzigen `POST` ohne Redirects.
//!
//! Diese Crate wählt deshalb **dieselbe** `hyper`-Familie wie
//! `harw-mcp-server`/`harw-web`, nur mit den clientseitigen Feature-Flags
//! (`hyper` `"client"`, `hyper-util` `"client"` + `"client-legacy"` +
//! `"tokio"`) statt der serverseitigen (`"server"` + `"http1"`). Cargo löst
//! Features pro Paketversion für den gesamten Baum vereinigt auf: `hyper`
//! 1.10.1, `hyper-util` 0.1.20, `http-body-util` 0.1.4, `bytes` 1.12.1 und
//! `tokio` 1.53.0 stehen bereits exakt in diesen Versionen im
//! Workspace-`Cargo.lock`, ebenso ihre für die Client-Features zusätzlich
//! nötigen transitiven Abhängigkeiten (`want`, `pin-project-lite`,
//! `smallvec`, `socket2`, `futures-util`, `futures-channel`, `tower-service`)
//! — alle bereits vorhanden, weil `reqwest` (`harw-provider-http`) selbst auf
//! `hyper`s Client baut. Das Hinzufügen dieser Feature-Flags fügt dem
//! Cargo.lock deshalb **null neue Crates** hinzu — nur zusätzliche
//! Compile-Zeit-Features auf bereits aufgelösten Paketen. `hyper-util`s
//! `Client` (Feature `"client-legacy"`) übernimmt Verbindungsaufbau,
//! Connection-Pooling und HTTP/1-Framing; diese Crate baut kein eigenes
//! Verbindungsmanagement.
//!
//! # TLS
//! **Nicht gebaut.** Ein OTLP-Collector hinter `https://` bräuchte entweder
//! `rustls` (samt Wurzelzertifikat-Bundle, z. B. `webpki-roots` oder
//! `rustls-native-certs`) oder `native-tls` (das eine Systembibliothek und
//! damit ein `build.rs` voraussetzt, verboten unter Doktrin **D3**). Beides
//! ist eine schwere neue Abhängigkeit, die dieser Knoten nicht rechtfertigt,
//! solange kein Aufrufer sie tatsächlich braucht — der Regelfall ist ein
//! Collector auf `localhost` oder im selben Vertrauensnetz. [`HttpTransport`]
//! akzeptiert deshalb **ausschließlich** `http://`-Endpunkte;
//! [`HttpTransport::new`] verweigert eine `https://`-Konfiguration mit
//! [`crate::OtlpError::Send`] statt eine unsichere Verbindung stillschweigend
//! auf Klartext herunterzustufen oder zu versuchen, sie doch aufzubauen und
//! erst beim Verbindungsversuch zu scheitern. Ein Betreiber mit einem
//! `https://`-Collector braucht entweder einen lokalen TLS-terminierenden
//! Reverse-Proxy (z. B. auf demselben Host über `http://127.0.0.1:<port>`)
//! oder eine künftige bewusste Erweiterung dieser Crate um eine
//! TLS-Bibliothek — das ist die hier gemeldete Lücke, keine übersehene.
//!
//! # Fehlschlag
//! Jeder Fehlschlag — eine ungültige `endpoint`-Konfiguration, ein ungültiger
//! Headername/-wert, ein Verbindungsfehler, eine eigene Zeitüberschreitung
//! (siehe Abschnitt „Zeitüberschreitung und Antwortrumpf-Obergrenze") oder
//! ein nicht-erfolgreicher HTTP-Status (alles außerhalb `2xx`) — wird als
//! [`crate::OtlpError::Send`] gemeldet. [`HttpTransport`] versucht **nie**,
//! einen fehlgeschlagenen Stapel erneut zuzustellen; das ist bereits
//! [`crate::OtlpSink::flush`]s Vertrag (siehe Crate-Doc, Abschnitt „Puffern
//! und Zustellen"): der Aufrufer zählt den Fehlschlag über
//! [`crate::OtlpSink::send_failure_count`], statt ihn zu propagieren.
//! [`HttpTransport::send_batch`] pausiert nie auf eine erneute Verbindung —
//! ein Verbindungsfehler kommt vom zugrunde liegenden Connector zurück und
//! wird sofort als [`crate::OtlpError::Send`] gemeldet.
//!
//! # Zeitüberschreitung und Antwortrumpf-Obergrenze
//! Ohne eigenes Zeitlimit könnte ein Collector, der die TCP-Verbindung
//! zwar annimmt, aber nie antwortet oder einen endlos langen Rumpf
//! ausliefert, [`HttpTransport::send_batch`] (und über
//! `Runtime::block_on`, siehe Abschnitt „Blockierender Aufruf, eigene
//! Laufzeit", auch [`crate::OtlpSink::flush`]) auf unbestimmte Zeit
//! blockieren; der Aufrufer ruft `flush()` beim Herunterfahren synchron
//! auf, ein hängender Collector würde also das Herunterfahren blockieren.
//! [`HttpTransport::new`] setzt deshalb auf dem `HttpConnector` ein
//! Verbindungsaufbau-Zeitlimit (`DEFAULT_CONNECT_TIMEOUT`), und
//! [`HttpTransport::send_batch`] umschließt den gesamten `deliver`-Aufruf
//! (Anfrage senden, Statuszeile und Antwortrumpf lesen) zusätzlich mit
//! `tokio::time::timeout` über `request_timeout` (Standardwert
//! `DEFAULT_REQUEST_TIMEOUT`) — läuft dieses Zeitlimit ab, meldet
//! `send_batch` [`crate::OtlpError::Send`], statt weiter zu warten. Der
//! Antwortrumpf selbst wird nicht mehr unbegrenzt eingelesen, sondern über
//! `http_body_util::Limited` auf `RESPONSE_BODY_LIMIT` begrenzt: ein
//! Collector, der endlos streamt, lässt den Speicher dieses Transports
//! damit nicht unbegrenzt wachsen, sondern das Einlesen bricht spätestens
//! bei `RESPONSE_BODY_LIMIT` mit einem (hier absichtlich verworfenen)
//! Fehler ab — die zugrunde liegende Verbindung wird dann nicht mehr
//! sauber an den Pool zurückgegeben, sondern geschlossen, ein hinnehmbarer
//! Kompromiss gegenüber unbegrenztem Speicherwachstum.
//!
//! # Die OTel-Adapter-Isolation gilt auch hier
//! [`HttpTransport::new`] und [`HttpTransport::send_batch`] tragen nach außen
//! ausschließlich eigene Typen (`&OtlpConfig`, `&[u8]`, [`crate::OtlpError`])
//! — kein `hyper::Uri`, kein `hyper::HeaderName`/`HeaderValue`, kein
//! `hyper_util`-Typ und kein `tokio`-Typ erscheint in einer öffentlichen
//! Signatur dieser Crate (siehe Crate-Doc, Abschnitt „Die
//! OTel-Adapter-Isolation" — dieselbe Mauer gilt für jede Fremdcrate, nicht
//! nur für die OpenTelemetry-Familie). Endpunkt, Header und der interne
//! `tokio`-Laufzeitgriff liegen als private Felder hinter [`HttpTransport`].
//!
//! # Blockierender Aufruf, eigene Laufzeit
//! [`crate::OtlpSink::flush`] ist synchroner Code (siehe Crate-Doc,
//! Abschnitt „Puffern und Zustellen": `flush()` darf blockieren, `record()`
//! nicht). `hyper`s Client ist ausschließlich async. Statt ein zweites
//! Nebenläufigkeitsmodell für diese Crate einzuführen, baut
//! [`HttpTransport::new`] eine **eigene, einzelne** `tokio`-Ein-Faden-Laufzeit
//! (`tokio::runtime::Builder::new_current_thread`) und
//! [`HttpTransport::send_batch`] überbrückt sie ausschließlich über
//! `Runtime::block_on` — das fügt sich in das bestehende synchrone
//! `flush()`-Modell ein, statt einen eigenen Hintergrund-Thread oder eine
//! zweite Laufzeit außerhalb dieser Methode zu verlangen.
//!
//! **Wichtige Einschränkung**: `Runtime::block_on` paniert, wenn es selbst
//! bereits innerhalb einer laufenden `tokio`-Laufzeit aufgerufen wird
//! ("Cannot start a runtime from within a runtime"). Ruft die
//! Kompositionsstelle `OtlpSink::flush` aus async `tokio`-Code auf (z. B.
//! `harw-cli` innerhalb eines `#[tokio::main]`), muss sie das über
//! `tokio::task::spawn_blocking` tun, nicht direkt `.await`en oder synchron
//! aus einem async-Kontext aufrufen — dieselbe Auflage, die jeder blockierende
//! Aufruf innerhalb einer `tokio`-Laufzeit trägt, unabhängig von dieser
//! Crate.
//!
//! # Nebenläufigkeit
//! [`HttpTransport`] ist `Send + Sync + Debug` (Vertrag für
//! [`crate::OtlpTransport`]): der `hyper_util`-Client ist intern klonbar und
//! threadsicher, die `tokio`-Laufzeit ist `Send + Sync`, Endpunkt und Header
//! sind nach der Konstruktion unveränderlich. `Debug` ist handgeschrieben
//! (weder `hyper_util::client::legacy::Client` noch
//! `tokio::runtime::Runtime` implementieren `Debug` durchgängig) und zeigt
//! nur den Endpunkt und die Headerzahl, nie Headerwerte (mögliche
//! Authentifizierungsdaten, siehe [`crate::HeaderEntry`]-Doku).
//!
//! # Tests ohne echtes Netz
//! Jeder Test dieses Moduls, der eine tatsächliche Zustellung prüft, startet
//! einen **eigenen, lokalen** `std::net::TcpListener` auf `127.0.0.1:0` (Port
//! 0 -> vom Betriebssystem zugewiesen) innerhalb des Tests selbst und schließt
//! ihn am Testende — kein Test baut eine Verbindung zu einem externen Host
//! auf. Der Testserver ist absichtlich eine minimale, handgeschriebene
//! HTTP/1.1-Antwort über einen rohen `std::net::TcpStream`-Thread, keine
//! zweite `hyper`-Serverinstanz — das hält den Test unabhängig von der
//! Implementierung, die er prüft.
//!
//! # Examples
//! ```
//! use harw_observe_otlp::{HttpTransport, OtlpConfig};
//!
//! let config = OtlpConfig {
//!     endpoint: "http://127.0.0.1:4318/v1/metrics".to_owned(),
//!     headers: Vec::new(),
//!     batch_size: 100,
//!     max_buffer: 10_000,
//!     resource_service_name: "harw-sentinel".to_owned(),
//! };
//! // `new()` baut nur Client und Laufzeit auf; es öffnet noch keine
//! // Verbindung -- ein nicht erreichbarer Collector scheitert erst beim
//! // ersten `send_batch()`, nicht hier.
//! let transport = HttpTransport::new(&config);
//! assert!(transport.is_ok());
//! ```

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::runtime::Runtime;

use crate::config::OtlpConfig;
use crate::error::OtlpError;
use crate::transport::OtlpTransport;

/// Verbindungsaufbau-Zeitlimit des internen `HttpConnector` (siehe Moduldoc,
/// Abschnitt „Zeitüberschreitung und Antwortrumpf-Obergrenze"). Fest, nicht
/// pro Instanz überschreibbar: dieser Wert fließt bereits beim Bau des
/// `hyper_util`-Clients in [`HttpTransport::new`] ein, danach lässt sich der
/// Connector nicht mehr verändern.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Gesamt-Zeitlimit für einen einzelnen `POST` (Anfrage senden, Statuszeile
/// und Antwortrumpf lesen zusammen); Standardwert für das private Feld
/// `HttpTransport::request_timeout` (siehe Moduldoc, Abschnitt
/// „Zeitüberschreitung und Antwortrumpf-Obergrenze").
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Obergrenze für den eingelesenen Antwortrumpf in Bytes (siehe Moduldoc,
/// Abschnitt „Zeitüberschreitung und Antwortrumpf-Obergrenze"): der Inhalt
/// trägt für diesen Transport ohnehin keine Information, ein endlos
/// streamender Rumpf soll den Speicher trotzdem nicht unbegrenzt wachsen
/// lassen.
const RESPONSE_BODY_LIMIT: usize = 64 * 1024;

/// Ein netzwerkfähiger [`crate::OtlpTransport`], der einen OTLP/JSON-Stapel
/// per HTTP `POST` an [`crate::OtlpConfig::endpoint`] ausliefert.
///
/// Siehe Moduldoc für die Begründung der HTTP-Bibliothek, das fehlende TLS,
/// das Fehlschlagverhalten und die Brücke zwischen synchronem `flush()` und
/// dem async `hyper`-Client.
pub struct HttpTransport {
    runtime: Runtime,
    client: Client<HttpConnector, Full<Bytes>>,
    uri: Uri,
    header_count: usize,
    headers: Vec<(HeaderName, HeaderValue)>,
    // Gesamt-Zeitlimit für `deliver` (siehe Moduldoc, Abschnitt
    // „Zeitüberschreitung und Antwortrumpf-Obergrenze"). Kein öffentlicher
    // Setter -- außerhalb dieser Crate bleibt der Wert fest auf
    // `DEFAULT_REQUEST_TIMEOUT`; die Tests dieses Moduls überschreiben ihn
    // direkt (privates Feld, in Rust auch von Kind-Modulen sichtbar), um
    // eine Zeitüberschreitung ohne einen mehrere Sekunden langen Testlauf
    // zu erzwingen.
    request_timeout: Duration,
}

impl fmt::Debug for HttpTransport {
    // Handgeschrieben statt `#[derive(Debug)]`: weder `Client` noch
    // `Runtime` implementieren `Debug` durchgängig (siehe Moduldoc,
    // Abschnitt „Nebenläufigkeit"), und Headerwerte (mögliche
    // Authentifizierungsdaten) sollen ohnehin nicht in einer Debug-Ausgabe
    // erscheinen.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("uri", &self.uri.to_string())
            .field("header_count", &self.header_count)
            .finish()
    }
}

impl HttpTransport {
    /// Baut einen `HttpTransport` für `config.endpoint`.
    ///
    /// # Description
    /// Baut den `hyper`-Client, die interne `tokio`-Ein-Faden-Laufzeit und
    /// validiert Endpunkt und Header — öffnet dabei aber noch **keine**
    /// Netzwerkverbindung (siehe Moduldoc, Beispiel). Verweigert einen
    /// `https://`-Endpunkt bedingungslos (siehe Moduldoc, Abschnitt „TLS").
    ///
    /// # Arguments
    /// - `config` (`&OtlpConfig`): liefert `endpoint` (muss mit `http://`
    ///   beginnen) und `headers` (jeder Eintrag muss ein gültiger
    ///   HTTP-Headername/-wert sein).
    ///
    /// # Returns
    /// Einen einsatzbereiten `HttpTransport`.
    ///
    /// # Errors
    /// - [`crate::OtlpError::Send`]: `config.endpoint` ist keine gültige URI,
    ///   verwendet nicht das `http`-Schema (insbesondere `https://`, siehe
    ///   Moduldoc „TLS"), ein `config.headers`-Eintrag trägt einen ungültigen
    ///   Headernamen oder -wert, oder die interne `tokio`-Laufzeit ließ sich
    ///   nicht aufbauen (z. B. Betriebssystem-Ressourcenerschöpfung).
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::{HttpTransport, OtlpConfig};
    ///
    /// let mut config = OtlpConfig {
    ///     endpoint: "https://collector.example/v1/metrics".to_owned(),
    ///     headers: Vec::new(),
    ///     batch_size: 100,
    ///     max_buffer: 10_000,
    ///     resource_service_name: "harw-sentinel".to_owned(),
    /// };
    /// assert!(HttpTransport::new(&config).is_err(), "https:// is refused, not silently downgraded");
    ///
    /// config.endpoint = "http://127.0.0.1:4318/v1/metrics".to_owned();
    /// assert!(HttpTransport::new(&config).is_ok());
    /// ```
    pub fn new(config: &OtlpConfig) -> Result<Self, OtlpError> {
        let uri = parse_http_endpoint(&config.endpoint)?;
        let headers = build_headers(&config.headers)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| OtlpError::Send {
                reason: format!(
                    "failed to start the HTTP transport's internal tokio runtime: {error}"
                ),
            })?;
        // Verbindungsaufbau-Zeitlimit setzen, bevor der Client den
        // Connector übernimmt (siehe Moduldoc, Abschnitt
        // „Zeitüberschreitung und Antwortrumpf-Obergrenze"): ohne dieses
        // Limit gäbe es keine Obergrenze für den TCP-Verbindungsaufbau,
        // sondern nur das (viel großzügigere und plattformabhängige)
        // Betriebssystem-Zeitlimit.
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(DEFAULT_CONNECT_TIMEOUT));
        let client = Client::builder(TokioExecutor::new()).build(connector);
        let header_count = headers.len();
        Ok(Self {
            runtime,
            client,
            uri,
            header_count,
            headers,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    // Führt den eigentlichen `POST` aus; von `send_batch` über
    // `Runtime::block_on` überbrückt (siehe Moduldoc, Abschnitt
    // „Blockierender Aufruf, eigene Laufzeit").
    async fn deliver(&self, payload: &[u8]) -> Result<(), OtlpError> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(self.uri.clone())
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (name, value) in &self.headers {
            builder = builder.header(name.clone(), value.clone());
        }
        let request = builder
            .body(Full::new(Bytes::copy_from_slice(payload)))
            .map_err(|error| OtlpError::Send {
                reason: format!("failed to build the OTLP HTTP request: {error}"),
            })?;

        let response = self
            .client
            .request(request)
            .await
            .map_err(|error| OtlpError::Send {
                reason: format!("HTTP request to {} failed: {error}", self.uri),
            })?;
        let status = response.status();
        // Den Rumpf noch abholen, damit die zugrunde liegende Verbindung dem
        // Pool sauber zurückgegeben wird -- der Inhalt selbst trägt für
        // diesen Transport keine Information. `Limited` begrenzt die
        // eingelesene Menge auf `RESPONSE_BODY_LIMIT` (siehe Moduldoc,
        // Abschnitt „Zeitüberschreitung und Antwortrumpf-Obergrenze"): ohne
        // diese Grenze könnte ein Collector, der endlos streamt, den
        // Speicher dieses Transports unbegrenzt wachsen lassen. Der Fehler
        // bei Überschreitung wird -- wie schon zuvor bei einem
        // regelkonformen Rumpf -- verworfen; die Verbindung wandert in
        // diesem Fall statt in den Pool in die Schließung.
        let _ = Limited::new(response.into_body(), RESPONSE_BODY_LIMIT)
            .collect()
            .await;

        if status.is_success() {
            Ok(())
        } else {
            Err(OtlpError::Send {
                reason: format!("collector at {} responded with status {status}", self.uri),
            })
        }
    }
}

impl OtlpTransport for HttpTransport {
    fn send_batch(&self, payload: &[u8]) -> Result<(), OtlpError> {
        // `deliver` allein hätte kein Gesamt-Zeitlimit -- ein Collector, der
        // die Verbindung annimmt, aber nie antwortet oder endlos streamt,
        // würde `block_on` (und damit `flush()`, siehe Moduldoc, Abschnitt
        // „Zeitüberschreitung und Antwortrumpf-Obergrenze") auf unbestimmte
        // Zeit blockieren. `tokio::time::timeout` umschließt deshalb den
        // gesamten `deliver`-Aufruf, nicht nur einen Teilschritt davon.
        self.runtime.block_on(async {
            match tokio::time::timeout(self.request_timeout, self.deliver(payload)).await {
                Ok(result) => result,
                Err(_elapsed) => Err(OtlpError::Send {
                    reason: format!(
                        "HTTP request to {} did not complete within {:?}",
                        self.uri, self.request_timeout
                    ),
                }),
            }
        })
    }
}

// `config.endpoint` muss eine gültige `http://`-URI sein; siehe Moduldoc,
// Abschnitt „TLS" für die Begründung, warum `https://` hier verweigert statt
// versucht wird.
fn parse_http_endpoint(endpoint: &str) -> Result<Uri, OtlpError> {
    let uri: Uri = endpoint.parse().map_err(|error| OtlpError::Send {
        reason: format!("invalid OTLP endpoint {endpoint:?}: {error}"),
    })?;
    match uri.scheme_str() {
        Some("http") => Ok(uri),
        Some("https") => Err(OtlpError::Send {
            reason: format!(
                "endpoint {endpoint:?} uses https://, which this transport does not implement \
                 (no TLS dependency, see crate docs section \"TLS\"); use http:// against a \
                 local collector or a TLS-terminating proxy"
            ),
        }),
        _ => Err(OtlpError::Send {
            reason: format!("endpoint {endpoint:?} must use the http:// scheme"),
        }),
    }
}

fn build_headers(
    entries: &[crate::config::HeaderEntry],
) -> Result<Vec<(HeaderName, HeaderValue)>, OtlpError> {
    entries
        .iter()
        .map(|entry| {
            let name =
                HeaderName::from_bytes(entry.name.as_bytes()).map_err(|error| OtlpError::Send {
                    reason: format!("invalid header name {:?}: {error}", entry.name),
                })?;
            let value = HeaderValue::from_str(&entry.value).map_err(|error| OtlpError::Send {
                reason: format!("invalid header value for header {:?}: {error}", entry.name),
            })?;
            Ok((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::config::HeaderEntry;
    use crate::test_support::{TestError, TestResult, ctx};

    fn test_config(endpoint: &str) -> OtlpConfig {
        OtlpConfig {
            endpoint: endpoint.to_owned(),
            headers: Vec::new(),
            batch_size: 100,
            max_buffer: 10_000,
            resource_service_name: "harw-sentinel".to_owned(),
        }
    }

    // Startet einen minimalen, lokalen HTTP/1.1-„Server" auf 127.0.0.1:0 für
    // genau eine Anfrage (siehe Moduldoc, Abschnitt „Tests ohne echtes
    // Netz"). Liefert die vom Test konfigurierte Statuszeile, liest die
    // vollständige Anfrage (Kopf + `Content-Length`-Rumpf) und reicht die
    // rohen Anfragebytes über den Kanal zurück, damit der aufrufende Test sie
    // prüfen kann.
    fn spawn_single_request_server(
        status_line: &'static str,
    ) -> TestResult<(String, mpsc::Receiver<Vec<u8>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("test listener binds"))?;
        let addr = listener
            .local_addr()
            .map_err(ctx("bound listener has an address"))?;
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut received = Vec::new();
            let mut buf = [0_u8; 4096];
            let mut header_end = None;
            loop {
                let read = stream.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..read]);
                if header_end.is_none() {
                    if let Some(pos) = find_subslice(&received, b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                    }
                }
                if let Some(header_end) = header_end {
                    let content_length = content_length_of(&received[..header_end]);
                    if received.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            let _ = stream.write_all(status_line.as_bytes());
            let _ = stream.flush();
            let _ = tx.send(received);
        });

        Ok((format!("http://{addr}/v1/metrics"), rx))
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn content_length_of(header_bytes: &[u8]) -> usize {
        String::from_utf8_lossy(header_bytes)
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().to_owned())
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0)
    }

    #[test]
    fn test_new_accepts_http_endpoint() {
        let config = test_config("http://127.0.0.1:4318/v1/metrics");
        assert!(HttpTransport::new(&config).is_ok());
    }

    #[test]
    fn test_new_rejects_https_endpoint_instead_of_downgrading() -> TestResult {
        let config = test_config("https://collector.example/v1/metrics");
        let Err(error) = HttpTransport::new(&config) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, OtlpError::Send { .. }));
        assert!(error.to_string().contains("https"));
        Ok(())
    }

    #[test]
    fn test_new_rejects_non_http_scheme() {
        let config = test_config("ftp://127.0.0.1/v1/metrics");
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_new_rejects_unparseable_endpoint() {
        let config = test_config("not a uri at all");
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_new_rejects_invalid_header_name() {
        let mut config = test_config("http://127.0.0.1:4318/v1/metrics");
        config.headers = vec![HeaderEntry {
            name: "bad header name".to_owned(),
            value: "x".to_owned(),
        }];
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_new_rejects_invalid_header_value() {
        let mut config = test_config("http://127.0.0.1:4318/v1/metrics");
        config.headers = vec![HeaderEntry {
            name: "X-Test".to_owned(),
            value: "bad\nvalue".to_owned(),
        }];
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_send_batch_posts_payload_to_local_listener() -> TestResult {
        let (endpoint, received) = spawn_single_request_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        let config = test_config(&endpoint);
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;

        let result = transport.send_batch(b"{\"resourceMetrics\":[]}");
        assert!(result.is_ok(), "{result:?}");

        let request_bytes = received
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(ctx("server observed a request"))?;
        let request = String::from_utf8_lossy(&request_bytes);
        assert!(
            request.starts_with("POST /v1/metrics HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        assert!(request.ends_with("{\"resourceMetrics\":[]}"));
        Ok(())
    }

    #[test]
    fn test_send_batch_includes_configured_headers() -> TestResult {
        let (endpoint, received) = spawn_single_request_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        let mut config = test_config(&endpoint);
        config.headers = vec![HeaderEntry {
            name: "X-Test-Header".to_owned(),
            value: "test-value".to_owned(),
        }];
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;

        transport.send_batch(b"{}").map_err(ctx("send succeeds"))?;

        let request_bytes = received
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(ctx("server observed a request"))?;
        let request = String::from_utf8_lossy(&request_bytes);
        assert!(request.contains("x-test-header: test-value"), "{request}");
        Ok(())
    }

    #[test]
    fn test_send_batch_reports_non_success_status_as_send_error() -> TestResult {
        let (endpoint, _received) = spawn_single_request_server(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        let config = test_config(&endpoint);
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;

        let Err(error) = transport.send_batch(b"{}") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, OtlpError::Send { .. }));
        assert!(error.to_string().contains("503"));
        Ok(())
    }

    #[test]
    fn test_send_batch_reports_connection_refused_as_send_error() -> TestResult {
        // Port 0 never accepts connections; nothing is bound here on purpose
        // -- this stays within "no real network connection", it simply never
        // establishes one.
        let config = test_config("http://127.0.0.1:1/v1/metrics");
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;

        let Err(error) = transport.send_batch(b"{}") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, OtlpError::Send { .. }));
        Ok(())
    }

    #[test]
    fn test_send_batch_times_out_when_collector_never_responds() -> TestResult {
        // Nimmt die Verbindung an, schreibt aber nie eine Antwort -- genau
        // das im Fund beschriebene Szenario: ein Collector, der TCP annimmt,
        // aber nie antwortet, darf `send_batch` nicht unbegrenzt blockieren
        // (siehe Moduldoc, Abschnitt „Zeitüberschreitung und
        // Antwortrumpf-Obergrenze").
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("test listener binds"))?;
        let addr = listener
            .local_addr()
            .map_err(ctx("bound listener has an address"))?;
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                // Hält die Verbindung offen und schreibt absichtlich nichts
                // zurück; der Test muss längst vor Ablauf dieser Schlafzeit
                // fertig sein. Der Thread wird beim Prozessende einfach
                // verworfen, kein Join nötig (wie bei den übrigen
                // Testservern dieses Moduls).
                thread::sleep(std::time::Duration::from_secs(30));
                drop(stream);
            }
        });

        let config = test_config(&format!("http://{addr}/v1/metrics"));
        let mut transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;
        // Kurzes Zeitlimit statt `DEFAULT_REQUEST_TIMEOUT` (10 s), damit der
        // Test nicht selbst mehrere Sekunden wartet -- das Feld ist privat,
        // aber dieses Testmodul liegt innerhalb desselben Moduls und sieht
        // es deshalb direkt (siehe Feldkommentar an `request_timeout`).
        transport.request_timeout = std::time::Duration::from_millis(200);

        let started = std::time::Instant::now();
        let Err(error) = transport.send_batch(b"{}") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, OtlpError::Send { .. }));
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "send_batch must return promptly once the request timeout elapses, took {elapsed:?}"
        );
        Ok(())
    }

    #[test]
    fn test_send_batch_ignores_oversized_response_body() -> TestResult {
        // Ein Rumpf, der `RESPONSE_BODY_LIMIT` überschreitet, darf einen
        // sonst erfolgreichen Status nicht in einen Fehlschlag verwandeln
        // -- `deliver` verwirft den Lesefehler von `Limited` absichtlich
        // (siehe Moduldoc, Abschnitt „Zeitüberschreitung und
        // Antwortrumpf-Obergrenze").
        let body_len = RESPONSE_BODY_LIMIT + 4096;
        let body = "a".repeat(body_len);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n{body}"
        );
        let status_line: &'static str = Box::leak(response.into_boxed_str());
        let (endpoint, _received) = spawn_single_request_server(status_line)?;
        let config = test_config(&endpoint);
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;

        let result = transport.send_batch(b"{}");
        assert!(result.is_ok(), "{result:?}");
        Ok(())
    }

    #[test]
    fn test_http_transport_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + fmt::Debug>() {}
        assert_bounds::<HttpTransport>();
    }

    #[test]
    fn test_http_transport_usable_through_arc_dyn_otlp_transport() -> TestResult {
        use std::sync::Arc;
        let (endpoint, _received) = spawn_single_request_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        let config = test_config(&endpoint);
        let transport: Arc<dyn OtlpTransport> =
            Arc::new(HttpTransport::new(&config).map_err(ctx("transport builds"))?);
        assert!(transport.send_batch(b"{}").is_ok());
        Ok(())
    }

    #[test]
    fn test_debug_output_omits_header_values() -> TestResult {
        let mut config = test_config("http://127.0.0.1:4318/v1/metrics");
        config.headers = vec![HeaderEntry {
            name: "Authorization".to_owned(),
            value: "Bearer super-secret-token".to_owned(),
        }];
        let transport = HttpTransport::new(&config).map_err(ctx("transport builds"))?;
        let debug = format!("{transport:?}");
        assert!(!debug.contains("super-secret-token"), "{debug}");
        assert!(debug.contains("header_count"), "{debug}");
        Ok(())
    }
}
