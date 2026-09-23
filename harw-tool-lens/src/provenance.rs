//! Woher `lens.ask` seine [`QueryProvenance`] nimmt — und warum nicht aus
//! dem Index.
//!
//! # Die Regel, die dieses Modul durchsetzt (K43)
//! [`harw_lens_query::QueryProvenance`] sagt, **womit eingebettet wurde**
//! (Modellname, Zerlegungsfassung). Die erste Fassung von
//! `harw_lens_query::query` nahm dafür `index.manifest().clone()` — also das
//! Manifest genau des Index, den sie durchsuchte. Ein Index ist nach
//! Definition immer mit sich selbst kompatibel, also konnte
//! `IndexManifest::compatible_with` in dieser Fassung **nie** fehlschlagen:
//! die Prüfung verglich den Index mit sich selbst. [`crate::ask_tool`] wickelt
//! genau in diesen Fehler nicht zurück, indem es `provenance` unabhängig vom
//! aufgelösten Index konstruiert — an derselben Stelle, an der `harw_lens`
//! selbst es verlangt (Pflichtparameter von [`harw_lens::ask`] bzw.
//! [`harw_lens_federation::federated_query`]).
//!
//! # Wer die Provenienz tatsächlich kennt
//! Der Aufrufer (das Modell, das `lens.ask` ruft) kennt sie nicht — es
//! übergibt nur eine Textfrage. Der durchsuchte Index darf sie laut K43 nicht
//! liefern. Es bleibt der Einbetter selbst: **dieses Werkzeug bettet die
//! Abfrage mit einem fest eingebauten Embedder ein**
//! ([`ASK_EMBEDDING_MODEL`]/[`ask_embedder`]), und genau dieser Embedder ist
//! die Quelle der Provenienz — nicht Konfiguration, nicht Aufrufer-Eingabe,
//! sondern derselbe Code, der auch tatsächlich rechnet.
//!
//! # Der aktualisierte Befund: ein produktionsreifer Embedder ist jetzt erreichbar -- und wird gewählt, wenn ein Endpunkt konfiguriert ist
//! Zum Zeitpunkt des ursprünglichen Knotens AW6-10 bot [`harw_lens`] **eine
//! einzige** [`harw_lens::Embedder`]-Implementierung an:
//! [`harw_lens::DeterministicEmbedder`] — laut dessen eigener
//! Moduldokumentation ausdrücklich für Tests gedacht (ein Hash-basierter
//! Platzhaltervektor ohne semantischen Gehalt). Knoten AW7-06 hat
//! `harw-lens-embed` um [`harw_lens::HttpEmbedBackend`] erweitert, einen
//! echten HTTP-Transport hinter der bereits vorhandenen
//! `RemoteEmbedder`/`RemoteEmbedBackend`-Typebene; zu jenem Zeitpunkt reichte
//! `harw-lens` (die kuratierte Fassade) diese Namen aber noch nicht durch,
//! und `harw-tool-lens/Cargo.toml` hing nicht direkt von `harw-lens-embed`
//! ab -- der Embedder existierte, war für dieses Werkzeug aber unerreichbar.
//!
//! **Dieser Knoten schließt genau diese Lücke, ohne die Vorgabe zu
//! ändern.** `harw-lens` exportiert jetzt [`harw_lens::RemoteEmbedder`],
//! [`harw_lens::HttpEmbedBackend`] und [`harw_lens::DimensionCheckedEmbedder`]
//! (siehe deren `//!`-Block für die vollständige Begründung).
//! [`ask_embedder`]/[`ask_provenance`] wählen jetzt zwischen zwei Pfaden --
//! siehe [`remote_ask_config`] --, aber die Vorgabe **ohne konfigurierten
//! Endpunkt bleibt exakt [`DeterministicEmbedder`]** mit
//! [`ASK_EMBEDDING_MODEL`]/[`ASK_EMBEDDING_DIMENSIONS`], unverändert
//! gegenüber dem Stand vor diesem Knoten -- der wichtigste Test dieses
//! Moduls (`test_ask_embedder_defaults_to_the_deterministic_placeholder_without_configured_endpoint`)
//! bindet genau das fest.
//!
//! # Warum die Vorgabe unverändert bleibt, statt automatisch auf HTTP umzusteigen
//! Zwei Kosten sprechen dagegen, den entfernten Einbetter zur *neuen* Vorgabe
//! zu machen, sobald er nur erreichbar ist:
//!
//! 1. **Dimensionswechsel.** `text-embedding-3-large` hat 3072 statt
//!    [`ASK_EMBEDDING_DIMENSIONS`]s 32 Dimensionen. Jeder mit dem alten
//!    Modell gebaute Index würde inkompatibel
//!    (`IndexManifest::compatible_with` lehnt ihn ab, K43 greift genau
//!    hier) und müsste vollständig neu gebaut werden -- eine automatische
//!    Umstellung würde also stillschweigend jede bestehende Installation
//!    brechen, statt sie zu verbessern.
//! 2. **Geheimnis nötig.** Ein `HttpEmbedBackend` verlangt einen API-Key;
//!    ohne einen von einem Betreiber bewusst gesetzten Endpunkt gibt es
//!    keinen sinnvollen Standardwert, den diese Crate raten könnte.
//!
//! Deshalb bleibt der Wechsel **opt-in**: [`remote_ask_config`] liest zwei
//! Umgebungsvariablen ([`REMOTE_EMBED_BASE_URL_VAR`],
//! [`REMOTE_EMBED_API_KEY_VAR`]); nur wenn **beide** nicht-leer gesetzt sind,
//! versucht [`ask_embedder`]/[`ask_provenance`] den entfernten Pfad -- und
//! fällt bei jedem Fehler dabei (Katalog lässt sich nicht laden, keine
//! `query`-Rolle registriert) auf den Platzhalter zurück, statt zu
//! `panic!`en oder `unwrap()`en. Das ist keine Systemuhr und keine
//! Zufallszahl -- reines Umgebungslesen, deterministisch für die Dauer eines
//! Prozesses.
//!
//! # Die Blockier-Falle: warum `lens_ask` jetzt über `spawn_blocking` läuft
//! [`harw_lens::HttpEmbedBackend`] verwendet `reqwest::blocking` (siehe
//! dessen Moduldoku): synchroner Code, der den aufrufenden Thread blockiert.
//! [`crate::ask_tool::ask_with_home`] -- die Funktion, die [`ask_embedder`]
//! tatsächlich aufruft -- ist selbst vollständig synchron, wird aber aus der
//! `async fn lens_ask` heraus aufgerufen, die auf einer Tokio-Runtime läuft.
//! Ein direkter Aufruf von `ask_with_home` aus `lens_ask` heraus liefe damit
//! auf demselben Runtime-Worker-Thread; sobald der entfernte Pfad gewählt
//! ist, bricht `reqwest::blocking` mit "Cannot start a runtime from within a
//! runtime" ab -- kein Fehlerwert, sondern ein Absturz. Deshalb ruft
//! `lens_ask` (siehe `crate::ask_tool`) `ask_with_home` seit diesem Knoten
//! über `tokio::task::spawn_blocking` auf: der synchrone Aufruf (egal ob
//! deterministisch oder über HTTP) läuft auf dem dafür vorgesehenen
//! Blocking-Thread-Pool, nicht auf einem Runtime-Worker-Thread.
//!
//! [`ASK_EMBEDDING_MODEL`] trägt den Platzhalter-Modellnamen weiterhin
//! explizit im Namen, damit ein Aufrufer, der die zurückgegebene Provenienz
//! inspiziert, die Einschränkung erkennt, wenn kein Endpunkt konfiguriert
//! ist, statt sie für ein echtes Modell zu halten.
//!
//! # Warum `model` aus dem tatsächlich benutzten Embedder abgeleitet wird
//! Der Modellname in [`QueryProvenance`] muss exakt dem Modellnamen
//! entsprechen, mit dem `harw_lens::build` die durchsuchten Indizes gebaut
//! hat -- sonst lehnt `IndexManifest::compatible_with` jede Abfrage ab (das
//! ist die beabsichtigte Wirkung von K43, nicht ein Fehler). [`ask_provenance`]
//! wiederholt deshalb **dieselbe** Auswahl wie [`ask_embedder`] (dieselbe
//! Umgebungsprüfung, derselbe Katalogeintrag) statt eine zweite, unabhängige
//! Quelle für den Modellnamen zu führen: beide Funktionen fallen unter
//! identischen Bedingungen auf denselben Wert zurück, sodass die gemeldete
//! Provenienz nie von dem tatsächlich benutzten Embedder abweichen kann.
//!
//! # Was `Confidential` weiterhin nicht hat
//! `lens.ask` bettet ausschließlich Abfragen ein (nie vertraulichen
//! Dokumentinhalt, siehe [`ask_descriptor`]) und wählt dafür nie
//! [`harw_lens::EmbeddingRole::Confidential`] -- dieses Modul befragt den
//! Katalog nur für [`harw_lens::EmbeddingRole::Query`]. Das ändert nichts an
//! dem in `harw_lens_embed::http_backend`s Moduldoku festgehaltenen Befund:
//! **`Confidential` hat weiterhin keinen produktionsreifen Einbetter** --
//! weder vor noch nach diesem Knoten existiert im Baum ein lokales
//! ML-Modell, das ihn bedienen könnte.
use harw_lens::{
    CHUNKER_VERSION, DeterministicEmbedder, DimensionCheckedEmbedder, Embedder, EmbeddingCatalog,
    EmbeddingDescriptor, EmbeddingRole, HttpEmbedBackend, ModelEntry, QueryProvenance,
    RemoteEmbedder,
};
use secrecy::SecretString;

/// Umgebungsvariable: Basis-URL eines OpenAI-kompatiblen
/// Einbettungs-Endpunkts (ohne `/embeddings`-Suffix), z. B.
/// `"https://api.openai.com/v1"`.
///
/// # Description
/// Siehe [`remote_ask_config`]: nur wenn diese **und**
/// [`REMOTE_EMBED_API_KEY_VAR`] nicht-leer gesetzt sind, versucht
/// [`ask_embedder`] den entfernten Pfad.
pub const REMOTE_EMBED_BASE_URL_VAR: &str = "HARW_LENS_ASK_REMOTE_EMBED_BASE_URL";

/// Umgebungsvariable: Bearer-API-Key für den unter
/// [`REMOTE_EMBED_BASE_URL_VAR`] konfigurierten Endpunkt.
///
/// # Description
/// Wird nie geloggt und nie in eine `Debug`-Ausgabe übernommen -- nur über
/// [`secrecy::SecretString`] in [`HttpEmbedBackend::new`] weitergereicht.
pub const REMOTE_EMBED_API_KEY_VAR: &str = "HARW_LENS_ASK_REMOTE_EMBED_API_KEY";

/// Dimension des fest eingebauten [`DeterministicEmbedder`].
///
/// # Description
/// Muss mit der Dimension übereinstimmen, mit der die über [`crate::scope`]
/// bekannten Indizes ohne konfigurierten Endpunkt gebaut wurden -- siehe die
/// Moduldokumentation, Abschnitt „Warum `model` aus dem tatsächlich
/// benutzten Embedder abgeleitet wird". Gilt **nur** für den
/// Platzhalter-Pfad; ein über [`remote_ask_config`] gewähltes entferntes
/// Modell trägt seine eigene, aus dem Katalog gelesene Dimension.
pub const ASK_EMBEDDING_DIMENSIONS: usize = 32;

/// Modellname des fest eingebauten Platzhalters -- die Vorgabe ohne
/// konfigurierten Endpunkt.
///
/// # Description
/// Trägt den Platzhalter-Charakter ausdrücklich im Namen: ohne konfigurierten
/// Endpunkt hat dieses Werkzeug keinen Zugriff auf ein echtes
/// Einbettungsmodell. Ein über [`remote_ask_config`] gewähltes entferntes
/// Modell trägt stattdessen seinen eigenen, aus dem Katalog gelesenen Namen
/// (z. B. `"text-embedding-3-large"`) -- niemals diesen Platzhalter.
pub const ASK_EMBEDDING_MODEL: &str = "deterministic-placeholder-32";

/// Liest die beiden Umgebungsvariablen, die einen entfernten Einbettungs-
/// Endpunkt konfigurieren.
///
/// # Description
/// Nur wenn **beide** Variablen gesetzt und nach Trimmen nicht leer sind,
/// liefert diese Funktion `Some` -- ein einseitig gesetztes Paar (z. B. nur
/// die Basis-URL, ohne Schlüssel) zählt als „nicht konfiguriert" und fällt
/// auf den Platzhalter zurück, statt mit einem leeren Geheimnis einen
/// Endpunkt anzusprechen.
///
/// # Returns
/// `Some((base_url, api_key))`, wenn beide Umgebungsvariablen nicht-leer
/// gesetzt sind; sonst `None`.
fn remote_ask_config() -> Option<(String, SecretString)> {
    remote_ask_config_from(
        std::env::var(REMOTE_EMBED_BASE_URL_VAR).ok(),
        std::env::var(REMOTE_EMBED_API_KEY_VAR).ok(),
    )
}

/// Die reine Auswahllogik hinter [`remote_ask_config`], getrennt von der
/// tatsächlichen Umgebungsabfrage.
///
/// # Description
/// Nimmt die beiden gelesenen Werte als Parameter entgegen, statt selbst
/// `std::env::var` aufzurufen -- so lässt sich die Auswahlregel (beide Werte
/// nicht-leer) in Tests prüfen, ohne die Prozessumgebung zu mutieren
/// (`std::env::set_var` ist seit Rust 2024 `unsafe` und würde bei
/// nebenläufig laufenden Tests im selben Binary zu einer Datenwettlaufgefahr
/// über gemeinsam genutzten Prozesszustand führen).
///
/// # Returns
/// `Some((base_url, api_key))`, wenn beide Argumente `Some` und nach Trimmen
/// nicht leer sind; sonst `None`.
fn remote_ask_config_from(
    base_url: Option<String>,
    api_key: Option<String>,
) -> Option<(String, SecretString)> {
    let base_url = base_url.filter(|value| !value.trim().is_empty())?;
    let api_key = api_key.filter(|value| !value.trim().is_empty())?;
    Some((base_url, SecretString::from(api_key)))
}

/// Der Katalogeintrag, den ein entfernter `lens.ask`-Pfad verwenden würde.
///
/// # Description
/// Befragt den eingebetteten Standardkatalog für
/// [`EmbeddingRole::Query`] -- niemals [`EmbeddingRole::Confidential`], siehe
/// die Moduldokumentation, Abschnitt „Was `Confidential` weiterhin nicht
/// hat". Liefert `None` bei jedem Fehler (Katalog lässt sich nicht parsen,
/// keine `query`-Rolle registriert), statt zu `panic!`en -- der Aufrufer
/// fällt in diesem Fall auf den Platzhalter zurück.
///
/// # Returns
/// Den passenden [`ModelEntry`], oder `None` bei jedem Fehler.
fn remote_ask_model_entry() -> Option<ModelEntry> {
    let catalog = EmbeddingCatalog::load_default().ok()?;
    catalog.entry_for_role(EmbeddingRole::Query).ok().cloned()
}

/// Baut den Embedder, den `lens.ask` für diesen Prozess verwendet.
///
/// # Description
/// Ohne konfigurierten Endpunkt (siehe [`remote_ask_config`]) unverändert
/// ein [`DeterministicEmbedder`] mit [`ASK_EMBEDDING_DIMENSIONS`]
/// Dimensionen -- siehe die Moduldokumentation, Abschnitt „Der aktualisierte
/// Befund". Mit konfiguriertem Endpunkt und auflösbarem Katalogeintrag ein
/// [`HttpEmbedBackend`] hinter [`RemoteEmbedder`], zusätzlich über
/// [`DimensionCheckedEmbedder`] gegen eine falsch dimensionierte Antwort
/// abgesichert.
///
/// # Returns
/// Einen einsatzbereiten [`Embedder`] hinter `Box<dyn Embedder>` -- die
/// beiden möglichen konkreten Typen sind sonst nicht ein und derselbe Typ.
///
/// # Examples
/// ```rust
/// use harw_lens::Embedder;
/// use harw_tool_lens::provenance::{ask_embedder, ASK_EMBEDDING_DIMENSIONS};
///
/// let embedder = ask_embedder();
/// let vectors = embedder.embed(&["hallo".to_owned()]).unwrap();
/// assert_eq!(vectors[0].len(), ASK_EMBEDDING_DIMENSIONS);
/// ```
#[must_use]
pub fn ask_embedder() -> Box<dyn Embedder> {
    if let Some((base_url, api_key)) = remote_ask_config() {
        if let Some(entry) = remote_ask_model_entry() {
            let backend = HttpEmbedBackend::new(base_url, entry.spec.name, api_key);
            let remote = RemoteEmbedder::new(backend, entry.spec.dimensions);
            return Box::new(DimensionCheckedEmbedder::new(remote));
        }
    }
    Box::new(DeterministicEmbedder::new(ASK_EMBEDDING_DIMENSIONS))
}

/// Baut den fest eingebauten Präfix-Deskriptor von `lens.ask`.
///
/// # Description
/// Nur das Abfrage-Präfix ist für `lens.ask` relevant
/// (`harw_lens::ask`/`federated_query` präfixieren Abfragen intern über
/// [`EmbeddingDescriptor::query_prefix`]); das Dokument-Präfix bleibt
/// dennoch Teil des Deskriptors, weil [`EmbeddingDescriptor`] beide Felder
/// gemeinsam trägt.
///
/// # Returns
/// Einen [`EmbeddingDescriptor`] mit E5-artigen `"query: "`/`"passage: "`
/// Präfixen, ohne Normalisierung.
///
/// # Examples
/// ```rust
/// use harw_tool_lens::provenance::ask_descriptor;
///
/// let descriptor = ask_descriptor();
/// assert_eq!(descriptor.query_prefix, "query: ");
/// ```
#[must_use]
pub fn ask_descriptor() -> EmbeddingDescriptor {
    EmbeddingDescriptor {
        document_prefix: "passage: ".to_owned(),
        query_prefix: "query: ".to_owned(),
        normalize: false,
    }
}

/// Der Modellname, den [`ask_provenance`] einträgt -- abgeleitet über
/// dieselbe Auswahl wie [`ask_embedder`].
///
/// # Description
/// Wiederholt genau die Bedingung aus [`ask_embedder`] (dieselbe
/// [`remote_ask_config`]-Prüfung, derselbe [`remote_ask_model_entry`]),
/// statt eine zweite, unabhängige Quelle für den Modellnamen zu führen --
/// siehe die Moduldokumentation, Abschnitt „Warum `model` aus dem
/// tatsächlich benutzten Embedder abgeleitet wird". Beide Funktionen fallen
/// unter identischen Bedingungen auf denselben Wert zurück.
///
/// # Returns
/// [`ModelEntry::spec`]s `name`, wenn ein entfernter Endpunkt konfiguriert
/// und der Katalogeintrag auflösbar ist; sonst [`ASK_EMBEDDING_MODEL`].
fn ask_embedding_model() -> String {
    if remote_ask_config().is_some() {
        if let Some(entry) = remote_ask_model_entry() {
            return entry.spec.name;
        }
    }
    ASK_EMBEDDING_MODEL.to_owned()
}

/// Baut die [`QueryProvenance`], mit der `lens.ask` jede Abfrage stellt.
///
/// # Description
/// Konstruiert unabhängig von jedem durchsuchten Index (siehe die
/// Moduldokumentation, Abschnitt „Die Regel, die dieses Modul durchsetzt").
/// `model` stammt aus [`ask_embedding_model`] -- derselben Auswahl, die
/// [`ask_embedder`] trifft, damit die gemeldete Provenienz nie von dem
/// tatsächlich benutzten Embedder abweichen kann.
/// `chunker_version` stammt aus [`harw_lens::CHUNKER_VERSION`] -- derselben
/// Konstante, die `harw-lens-source` beim Bauen eines Index einträgt --,
/// nicht aus einer zweiten, hier neu erfundenen Zahl.
///
/// # Returns
/// Eine [`QueryProvenance`] mit dem tatsächlich verwendeten Modellnamen und
/// [`harw_lens::CHUNKER_VERSION`].
///
/// # Examples
/// ```rust
/// use harw_lens::CHUNKER_VERSION;
/// use harw_tool_lens::provenance::{ask_provenance, ASK_EMBEDDING_MODEL};
///
/// let provenance = ask_provenance();
/// assert_eq!(provenance.model, ASK_EMBEDDING_MODEL);
/// assert_eq!(provenance.chunker_version, CHUNKER_VERSION);
/// ```
#[must_use]
pub fn ask_provenance() -> QueryProvenance {
    QueryProvenance {
        model: ask_embedding_model(),
        chunker_version: CHUNKER_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_ask_provenance_uses_placeholder_model_and_shared_chunker_version() {
        let provenance = ask_provenance();
        assert_eq!(provenance.model, ASK_EMBEDDING_MODEL);
        assert_eq!(provenance.chunker_version, CHUNKER_VERSION);
    }

    #[test]
    fn test_ask_embedder_produces_configured_dimensions() -> TestResult {
        let embedder = ask_embedder();
        let vectors = embedder
            .embed(&["eine testfrage".to_owned()])
            .map_err(ctx("Embedder darf hier nicht fehlschlagen"))?;
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), ASK_EMBEDDING_DIMENSIONS);
        Ok(())
    }

    #[test]
    fn test_ask_descriptor_carries_query_and_document_prefixes() {
        let descriptor = ask_descriptor();
        assert_eq!(descriptor.query_prefix, "query: ");
        assert_eq!(descriptor.document_prefix, "passage: ");
        assert!(!descriptor.normalize);
    }

    /// **Der wichtigste Test dieses Moduls:** ohne konfigurierten Endpunkt
    /// (`remote_ask_config()` liest die reale Prozessumgebung, die dieser
    /// Test bewusst nicht mutiert -- siehe
    /// [`remote_ask_config_from`]s Tests für die Auswahllogik selbst) bleibt
    /// die Vorgabe exakt der deterministische Platzhalter, unverändert
    /// gegenüber dem Stand vor diesem Knoten.
    #[test]
    fn test_ask_embedder_defaults_to_the_deterministic_placeholder_without_configured_endpoint()
    -> TestResult {
        let embedder = ask_embedder();
        let vectors = embedder
            .embed(&["eine testfrage".to_owned()])
            .map_err(ctx("Platzhalter-Embedder darf nicht fehlschlagen"))?;
        assert_eq!(vectors[0].len(), ASK_EMBEDDING_DIMENSIONS);
        assert_eq!(embedder.locality(), harw_lens::Locality::Local);
        assert_eq!(ask_provenance().model, ASK_EMBEDDING_MODEL);
        Ok(())
    }

    /// [`remote_ask_config_from`] verlangt **beide** Werte; eine gesetzte
    /// Basis-URL ohne Schlüssel zählt als „nicht konfiguriert". Prüft die
    /// reine Auswahllogik ohne Prozessumgebung zu berühren (siehe
    /// [`remote_ask_config_from`]s Dokumentation) -- keine Datenwettlaufgefahr
    /// mit anderen, nebenläufig laufenden Tests in diesem Binary.
    #[test]
    fn test_remote_ask_config_from_requires_both_values() {
        assert!(remote_ask_config_from(Some("http://unused.invalid".to_owned()), None).is_none());
        assert!(remote_ask_config_from(None, Some("secret".to_owned())).is_none());
        assert!(remote_ask_config_from(None, None).is_none());
    }

    /// Ein nur aus Leerraum bestehender Wert zählt ebenfalls als
    /// „nicht gesetzt".
    #[test]
    fn test_remote_ask_config_from_treats_blank_values_as_unset() {
        assert!(
            remote_ask_config_from(Some("   ".to_owned()), Some("secret".to_owned())).is_none()
        );
    }

    /// Sind beide Werte nicht-leer gesetzt, liefert
    /// [`remote_ask_config_from`] sie unverändert zurück.
    #[test]
    fn test_remote_ask_config_from_returns_both_values_when_configured() -> TestResult {
        let (base_url, _api_key) = remote_ask_config_from(
            Some("http://example.invalid".to_owned()),
            Some("secret".to_owned()),
        )
        .ok_or(TestError::Missing("beide Werte sind gesetzt"))?;
        assert_eq!(base_url, "http://example.invalid");
        Ok(())
    }
}
