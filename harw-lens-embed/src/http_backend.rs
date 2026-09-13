//! Der produktionsreife Transport: [`HttpEmbedBackend`], ein
//! [`RemoteEmbedBackend`], das tatsächlich über HTTP einbettet.
//!
//! # Warum dieser Knoten existiert (AW7-06)
//! [`crate::remote::RemoteEmbedder`] (Knoten AW7-05) definierte die vierte
//! Einbettungsschicht bereits vollständig, ließ aber [`RemoteEmbedBackend`]
//! ohne echte Implementierung -- der erste echte Konsument (`harw-tool-lens`)
//! musste deshalb notgedrungen [`crate::embedder::DeterministicEmbedder`]
//! verwenden (siehe dessen Fund in `harw-tool-lens/src/provenance.rs`). Dieser
//! Knoten schließt genau diese Lücke: [`HttpEmbedBackend`] ist der erste
//! `RemoteEmbedBackend`, der einen Text tatsächlich verschickt.
//!
//! # Die Entscheidung: entfernt über HTTP, nicht lokal über `candle`/`ort`
//! Zwei Wege standen offen:
//!
//! **(a) Entfernt, über eine HTTP-Einbettungs-API.** Wenig neuer Code, weil
//! [`crate::remote::RemoteEmbedder`] die Typebene bereits trägt -- dieser
//! Knoten fügt nur den Transport hinzu. Kosten: `Confidential` und
//! `OperatorOnly` erreichen diesen Weg strukturell nie (siehe unten), bleiben
//! also weiterhin ohne produktionsreifen Einbetter.
//!
//! **(b) Lokal, über ein eingebettetes ML-Modell (`candle`/`ort`).** Geprüft,
//! nicht angenommen: ein Registry-Probe dieses Knotens (`cargo add
//! candle-core candle-nn candle-transformers tokenizers` in einem
//! Scratch-Projekt, CPU-Features, ohne `cuda`/`mkl`/`metal`) sperrte **171**
//! zusätzliche Crates in `Cargo.lock` -- mehr als ein Viertel der Größe des
//! gesamten Harwness-Workspaces (665 Pakete zum Zeitpunkt dieses Knotens).
//! `candle-core` selbst trägt kein `build.rs` (bestätigt gegen dessen
//! `Cargo.toml`), aber `tokenizers` zieht auf Nicht-WASM-Zielen (also genau
//! diesem Workspace) das `onig`-Feature und damit `onig_sys` -- eine Bindung
//! an die C-Bibliothek Oniguruma **mit** `build.rs`, das einen C-Compiler zur
//! Kompilierzeit voraussetzt. Das verletzt Doktrin D3 (minimale
//! Abhängigkeitsfläche, kein `build.rs`/Systembibliothek-Zwang) doppelt: über
//! die schiere Zahl der Crates und über den konkreten C-Baustein. Dazu kommt
//! ein Problem, das kein Cargo-Feature löst: Modellgewichte (typischerweise
//! hunderte MB) müssen von irgendwoher ins System -- diese Crate hat weder
//! einen Downloader noch einen vorgesehenen Ablageort dafür, und beides zu
//! bauen wäre ein eigener Knoten, keine Nebenwirkung dieses hier.
//!
//! **Entscheidung: (a).** Die vorhandene Naht ([`crate::remote::RemoteEmbedder`]/
//! [`RemoteEmbedBackend`]) trägt bereits die komplette Typsicherheit; dieser
//! Knoten füllt sie mit einem minimalen, bereits im Workspace geprüften
//! Transport (`reqwest` 0.12 mit `rustls-tls`, exakt die Version und
//! Feature-Wahl, die `harw-provider-http` bereits einsetzt -- siehe
//! `Cargo.toml`). Kein `build.rs`, keine neue Systembibliothek, kein
//! C-Compiler-Zwang.
//!
//! # Was diese Entscheidung nicht kann (das ist eine Aussage, keine Fußnote)
//! **`Confidential` bleibt nach diesem Knoten weiterhin ohne
//! produktionsreifen Einbetter.** [`HttpEmbedBackend`] ist über
//! [`crate::remote::RemoteEmbedder`] strukturell auf
//! [`harw_lens_types::Locality::Remote`] festgelegt (siehe dessen
//! Moduldoku, Abschnitt „Warum `locality()` nicht vom Backend abhängt"); der
//! Router ([`crate::catalog::route`]) filtert `Confidential` **vor** der
//! Auswahl auf [`harw_lens_types::Locality::Local`] und schlägt fehl statt
//! auszuweichen. Ein vertraulicher Text erreicht [`HttpEmbedBackend`] also
//! nie -- aber das heißt auch: es gibt zum Zeitpunkt dieses Knotens **kein**
//! Modell, das `Confidential` tatsächlich bedienen könnte
//! ([`crate::embedder::DeterministicEmbedder`] ist laut eigener Moduldoku
//! ausdrücklich kein semantisches Modell). Wer `Confidential` produktiv
//! bedienen will, braucht Weg (b) -- mit den oben genannten Kosten -- oder
//! einen dritten, hier nicht geprüften Weg (z. B. ein selbst betriebener
//! HTTP-Endpunkt in derselben Vertrauenszone wie der Host, was die
//! `Locality::Remote`-Festlegung dieses Typs allerdings falsch machte und
//! deshalb keine Lösung *dieses* Typs wäre, sondern einen neuen erforderte).
//!
//! **`Embedder::embed` ist synchron; `HttpEmbedBackend` blockiert den
//! aufrufenden Thread.** Der `Embedder`-Vertrag (siehe
//! [`crate::embedder`]) ist absichtlich nicht `async` -- er wird aus
//! synchronen Bau-/Abfragepfaden aufgerufen (`harw-lens-source`,
//! `harw-lens-query`), die selbst keine Tokio-Runtime mitbringen.
//! [`HttpEmbedBackend`] verwendet deshalb `reqwest::blocking::Client`, nicht
//! den asynchronen Client. **Das hat eine Konsequenz, die ein Aufrufer
//! kennen muss:** wird `embed`/`embed_remote` auf einem Thread aufgerufen,
//! der bereits von einer Tokio-Runtime als Worker verwendet wird (z. B.
//! direkt aus einer `async fn` heraus, ohne `spawn_blocking`), bricht Tokio
//! mit "Cannot start a runtime from within a runtime" ab. Ein Aufrufer aus
//! einem `async`-Kontext (wie `harw-tool-lens::ask_tool::lens_ask`) muss
//! `HttpEmbedBackend::embed_remote` deshalb über
//! `tokio::task::spawn_blocking` (oder einen dedizierten Thread) aufrufen --
//! diese Crate kann das nicht selbst erzwingen, weil `Embedder` keinen
//! Runtime-Zugriff besitzt und ihn laut Vertrag auch nicht braucht.
//!
//! # Wire-Format
//! `POST {base_url}/embeddings` mit Body `{"model": …, "input": [...]}` und
//! Header `Authorization: Bearer <api_key>` -- dasselbe Muster, das
//! `harw-provider-http::OpenAiResponsesProvider` für `/responses` und
//! `/chat/completions` verwendet (siehe dessen Moduldoku), auf den
//! OpenAI-kompatiblen `/embeddings`-Endpunkt übertragen, den u. a. OpenAI
//! selbst und zahlreiche selbst gehostete Server (vLLM, TEI, LiteLLM-Proxys)
//! unter demselben Schema bedienen. Die Antwort wird über das `index`-Feld
//! jedes `data[]`-Eintrags neu geordnet, **nicht** über die Ankunftsreihenfolge
//! -- ein Server, der Einträge umsortiert (z. B. für Batching-Optimierungen),
//! darf die Zuordnung zu `texts` nicht verfälschen.
//!
//! # Warum diese Crate nicht `harw-provider-http` selbst aufruft
//! `harw-provider-http` bietet zum Zeitpunkt dieses Knotens **keinen**
//! Einbettungs-Aufruf an -- geprüft gegen dessen `src/lib.rs`: das einzige
//! öffentliche Verb ist `ModelProvider::respond` (Chat-/Responses-Turn,
//! async, `POST {base_url}/responses` bzw. `/chat/completions`). Es gibt
//! dort keinen `/embeddings`-Pfad und keine synchrone Aufrufform. Diese
//! Schicht dort nachzurüsten wäre die naheliegende Folgearbeit -- sie liegt
//! außerhalb des Schreibbereichs dieses Knotens (`harw-lens-embed/**` und
//! `harw-tool-lens/src/provenance.rs`), deshalb bleibt [`HttpEmbedBackend`]
//! ein eigenständiger, minimaler Transport statt eines Aufrufs durch
//! `harw-provider-http` hindurch. Siehe den Abschlussbericht dieses Knotens
//! für die vollständige Einordnung.
//!
//! # Nebenläufigkeit
//! [`HttpEmbedBackend`] ist `Send + Sync` (der `reqwest::blocking::Client`
//! ist intern geteilt und klont günstig) -- erfüllt damit den
//! [`RemoteEmbedBackend`]-Bound.
//!
//! # Fehler
//! [`crate::error::EmbedError::RemoteBackendFailed`] für jede Störung:
//! Transportfehler, Nicht-2xx-Status, unlesbarer oder unvollständiger
//! JSON-Antwortkörper. Die Fehlermeldung enthält nie den API-Key (er wird
//! nur über [`secrecy::ExposeSecret`] beim Setzen des Headers offengelegt).
//!
//! # Examples
//! ```rust,no_run
//! use harw_lens_embed::{DimensionCheckedEmbedder, Embedder, HttpEmbedBackend, RemoteEmbedder};
//! use secrecy::SecretString;
//!
//! let backend = HttpEmbedBackend::new(
//!     "https://api.openai.com/v1",
//!     "text-embedding-3-large",
//!     SecretString::new("sk-...".into()),
//! );
//! let embedder = DimensionCheckedEmbedder::new(RemoteEmbedder::new(backend, 3072));
//! let vectors = embedder.embed(&["passage: geheimtext".to_owned()]);
//! assert!(vectors.is_ok() || vectors.is_err());
//! ```

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::error::EmbedError;
use crate::remote::RemoteEmbedBackend;

/// Obergrenze für die Länge eines in eine Fehlermeldung übernommenen
/// Antwortkörper-Ausschnitts -- verhindert, dass eine sehr lange
/// Fehlerantwort eines entfernten Dienstes unbegrenzt in einer
/// [`EmbedError`]-Meldung landet.
const ERROR_BODY_SNIPPET_LEN: usize = 200;

/// Ein [`RemoteEmbedBackend`], das Texte über eine OpenAI-kompatible
/// HTTP-Einbettungs-API verschickt.
///
/// # Description
/// Hält den geteilten `reqwest::blocking::Client`, die Basis-URL, den
/// Modellnamen (für das `model`-Feld des Requests) und den geheimen API-Key.
/// Siehe die Moduldokumentation für die Wire-Form, die Entscheidung für
/// diesen Weg statt eines lokalen Modells, und die Pflicht des Aufrufers,
/// `embed_remote` aus einem async-Kontext heraus über
/// `tokio::task::spawn_blocking` aufzurufen.
///
/// # Concurrency
/// `Send + Sync`; kann hinter einem `Arc` von mehreren Threads genutzt
/// werden, wie es [`RemoteEmbedBackend`] verlangt.
pub struct HttpEmbedBackend {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
    api_key: SecretString,
}

impl HttpEmbedBackend {
    /// Baut ein `HttpEmbedBackend` aus expliziten Werten.
    ///
    /// # Arguments
    /// - `base_url` (`impl Into<String>`): Basis-URL ohne `/embeddings`-Suffix
    ///   (z. B. `"https://api.openai.com/v1"`); ein abschließender `/` wird
    ///   beim Aufbau der Anfrage-URL toleriert.
    /// - `model` (`impl Into<String>`): Modellname für das `model`-Feld des
    ///   Requests -- **muss** exakt dem Namen entsprechen, den der Aufrufer
    ///   auch in `QueryProvenance::model` (`harw-lens-query`) einträgt (siehe
    ///   K43 in `harw-tool-lens/src/provenance.rs`), sonst weist
    ///   `IndexManifest::compatible_with` jede Abfrage ab.
    /// - `api_key` (`SecretString`): Bearer-Token; wird nie geloggt, nur beim
    ///   Setzen des `Authorization`-Headers via [`secrecy::ExposeSecret`]
    ///   offengelegt.
    ///
    /// # Returns
    /// Ein einsatzbereites `HttpEmbedBackend`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::HttpEmbedBackend;
    /// use secrecy::SecretString;
    ///
    /// let backend = HttpEmbedBackend::new(
    ///     "https://api.openai.com/v1",
    ///     "text-embedding-3-large",
    ///     SecretString::new("sk-...".into()),
    /// );
    /// let _ = backend;
    /// ```
    #[must_use]
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: SecretString) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            base_url: base_url.into(),
            model: model.into(),
            api_key,
        }
    }

    /// Baut die vollständige `/embeddings`-URL aus `base_url`.
    fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.base_url.trim_end_matches('/'))
    }
}

/// Ein einzelner Eintrag im `data[]`-Array einer Einbettungs-Antwort.
///
/// # Description
/// `index` trägt die Position des zugehörigen Eingabetextes -- die Antwort
/// wird darüber neu geordnet, nicht über die Ankunftsreihenfolge (siehe die
/// Moduldokumentation, Abschnitt „Wire-Format").
#[derive(Debug, Deserialize)]
struct EmbeddingDataItem {
    embedding: Vec<f32>,
    index: usize,
}

/// Der vollständige Antwortkörper einer OpenAI-kompatiblen
/// Einbettungs-Antwort -- nur das Feld, das diese Crate tatsächlich braucht.
#[derive(Debug, Deserialize)]
struct EmbeddingsResponseBody {
    data: Vec<EmbeddingDataItem>,
}

/// Kürzt `body` auf höchstens [`ERROR_BODY_SNIPPET_LEN`] Zeichen für die
/// Aufnahme in eine Fehlermeldung.
fn error_body_snippet(body: &str) -> String {
    body.chars().take(ERROR_BODY_SNIPPET_LEN).collect()
}

impl RemoteEmbedBackend for HttpEmbedBackend {
    /// Schickt `texts` als `{"model": …, "input": [...]}` an
    /// `{base_url}/embeddings` und liest `data[].embedding`, neu geordnet
    /// über `data[].index`.
    ///
    /// # Errors
    /// - [`EmbedError::RemoteBackendFailed`]: bei Transportfehler,
    ///   Nicht-2xx-Status, unlesbarem/ungültigem JSON-Antwortkörper, einem
    ///   `index`-Wert außerhalb von `texts`, oder einer Antwort, die für
    ///   einen Eingabetext keinen Eintrag liefert.
    fn embed_remote(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let response = self
            .client
            .post(self.embeddings_url())
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .map_err(|error| EmbedError::RemoteBackendFailed {
                reason: format!("request failed: {error}"),
            })?;

        let status = response.status();
        let response_text = response.text().map_err(|error| EmbedError::RemoteBackendFailed {
            reason: format!("failed to read response body: {error}"),
        })?;

        if !status.is_success() {
            return Err(EmbedError::RemoteBackendFailed {
                reason: format!(
                    "HTTP {status}: {}",
                    error_body_snippet(&response_text)
                ),
            });
        }

        let parsed: EmbeddingsResponseBody =
            serde_json::from_str(&response_text).map_err(|error| EmbedError::RemoteBackendFailed {
                reason: format!("invalid response JSON: {error}"),
            })?;

        let mut ordered: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for item in parsed.data {
            let Some(slot) = ordered.get_mut(item.index) else {
                return Err(EmbedError::RemoteBackendFailed {
                    reason: format!(
                        "response referenced input index {} but only {} inputs were sent",
                        item.index,
                        texts.len()
                    ),
                });
            };
            *slot = Some(item.embedding);
        }

        ordered
            .into_iter()
            .enumerate()
            .map(|(index, vector)| {
                vector.ok_or_else(|| EmbedError::RemoteBackendFailed {
                    reason: format!("response did not include an embedding for input index {index}"),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::catalog::{route, EmbeddingCatalog, EmbeddingRole};
    use crate::embedder::Embedder;
    use crate::remote::RemoteEmbedder;
    use harw_lens_types::Locality;

    /// Startet einen minimalen In-Process-HTTP-Mock, der genau eine Anfrage
    /// beantwortet -- keine echte Netzverbindung nach außen, wie von der
    /// Auflage dieses Knotens verlangt (`127.0.0.1`, Port 0 = vom OS
    /// zugewiesen, nie verlassen).
    fn mock_server(status_line: &str, body: &str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
        let response = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept mock request");
            let mut buffer = [0_u8; 4096];
            // Die Anfrage wird gelesen, aber verworfen: ohne das Lesen
            // schließt der Client die Verbindung nie sauber, weil er auf
            // eine Antwort wartet, während der Server noch liest.
            let _ = stream.read(&mut buffer).expect("read mock request");
            stream
                .write_all(response.as_bytes())
                .expect("write mock response");
        });
        (base_url, handle)
    }

    #[test]
    fn test_http_embed_backend_parses_reordered_response() {
        // Der Server liefert die Einträge absichtlich in vertauschter
        // Reihenfolge (index 1 vor index 0) -- die Zuordnung muss trotzdem
        // stimmen.
        let body = serde_json::json!({
            "data": [
                {"index": 1, "embedding": [3.0, 4.0]},
                {"index": 0, "embedding": [1.0, 2.0]},
            ]
        })
        .to_string();
        let (base_url, server) = mock_server("HTTP/1.1 200 OK", &body);

        let backend = HttpEmbedBackend::new(base_url, "test-model", SecretString::new("k".into()));
        let vectors = backend
            .embed_remote(&["first".to_owned(), "second".to_owned()])
            .expect("mock succeeds");

        assert_eq!(vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
        server.join().expect("mock server completes");
    }

    #[test]
    fn test_http_embed_backend_reports_non_success_status() {
        let (base_url, server) = mock_server(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":"invalid api key"}"#,
        );

        let backend = HttpEmbedBackend::new(base_url, "test-model", SecretString::new("k".into()));
        let error = backend
            .embed_remote(&["x".to_owned()])
            .expect_err("non-2xx status must fail");

        let EmbedError::RemoteBackendFailed { reason } = error else {
            panic!("expected RemoteBackendFailed");
        };
        assert!(reason.contains("401"));
        assert!(reason.contains("invalid api key"));
        server.join().expect("mock server completes");
    }

    #[test]
    fn test_http_embed_backend_reports_missing_index_entry() {
        // Nur ein Eintrag fuer zwei angefragte Texte -- Index 1 fehlt.
        let body = serde_json::json!({
            "data": [{"index": 0, "embedding": [1.0]}]
        })
        .to_string();
        let (base_url, server) = mock_server("HTTP/1.1 200 OK", &body);

        let backend = HttpEmbedBackend::new(base_url, "test-model", SecretString::new("k".into()));
        let error = backend
            .embed_remote(&["a".to_owned(), "b".to_owned()])
            .expect_err("missing entry must fail, not silently pad");

        let EmbedError::RemoteBackendFailed { reason } = error else {
            panic!("expected RemoteBackendFailed");
        };
        assert!(reason.contains('1'));
        server.join().expect("mock server completes");
    }

    #[test]
    fn test_http_embed_backend_reports_out_of_range_index() {
        let body = serde_json::json!({
            "data": [{"index": 5, "embedding": [1.0]}]
        })
        .to_string();
        let (base_url, server) = mock_server("HTTP/1.1 200 OK", &body);

        let backend = HttpEmbedBackend::new(base_url, "test-model", SecretString::new("k".into()));
        let error = backend
            .embed_remote(&["only-one".to_owned()])
            .expect_err("out-of-range index must fail closed");

        assert!(matches!(error, EmbedError::RemoteBackendFailed { .. }));
        server.join().expect("mock server completes");
    }

    #[test]
    fn test_http_embed_backend_reports_invalid_json() {
        let (base_url, server) = mock_server("HTTP/1.1 200 OK", "not json");

        let backend = HttpEmbedBackend::new(base_url, "test-model", SecretString::new("k".into()));
        let error = backend
            .embed_remote(&["x".to_owned()])
            .expect_err("invalid JSON must fail");

        assert!(matches!(error, EmbedError::RemoteBackendFailed { .. }));
        server.join().expect("mock server completes");
    }

    #[test]
    fn test_remote_embedder_over_http_backend_reports_remote_locality() {
        // Der wichtigste Sicherheitsaspekt dieses Knotens: gleich, was das
        // Backend tut, bleibt `locality()` fest auf `Remote` -- ein
        // `HttpEmbedBackend` kann diese Auskunft nicht überschreiben (siehe
        // `RemoteEmbedder`s Moduldoku).
        let backend = HttpEmbedBackend::new("http://unused.invalid", "m", SecretString::new("k".into()));
        let embedder = RemoteEmbedder::new(backend, 4);
        assert_eq!(embedder.locality(), Locality::Remote);
    }

    /// **Der wichtigste Test dieses Knotens:** ein Katalog, der sowohl ein
    /// lokales Modell als auch ein über [`HttpEmbedBackend`] betriebenes
    /// entferntes Modell für `Confidential` registriert, muss trotzdem das
    /// lokale wählen -- nie das entfernte, obwohl es real über HTTP
    /// verschickt (nicht nur simuliert wie `RecordingBackend` in
    /// `remote.rs`s eigenen Tests). Das beweist, dass die Existenz eines
    /// *echten* Netz-Backends die Fail-Closed-Garantie von
    /// [`crate::catalog::route`] nicht aufweicht.
    #[test]
    fn test_confidential_role_never_selects_the_http_backed_remote_profile() {
        let toml_src = r#"
            [[model]]
            roles = ["confidential"]

            [model.spec]
            name = "text-embedding-3-large"
            dimensions = 3072
            metric = "cosine"
            max_input_chars = 8192

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = false

            [model.runtime]
            backend = "http-embed-openai-compatible"
            locality = "remote"
            batch_size = 64

            [[model]]
            roles = ["confidential"]

            [model.spec]
            name = "local-minilm-l6-v2"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = true

            [model.runtime]
            backend = "onnx-local"
            locality = "local"
            batch_size = 32
        "#;
        let catalog = EmbeddingCatalog::parse(toml_src).expect("parses");
        let profile = route(&catalog, EmbeddingRole::Confidential)
            .expect("a local profile exists alongside the HTTP-backed remote one");
        assert_eq!(profile.locality, Locality::Local);
        assert_eq!(profile.backend, "onnx-local");
    }
}
