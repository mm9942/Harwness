//! Session-Titel per Modell (`generate_title`/`ensure_title`/`spawn_title_job`).
//!
//! Deckt Session-Titel und Resume-Picker ab. Der Titel-Aufruf selbst
//! läuft über [`harw_core::one_shot::complete_text`] — ein einzelner
//! Modell-Aufruf ohne Werkzeuge und ohne Turn-Schleife (siehe dessen
//! Moduldoku), deshalb ist auch hier keine Freigabekette beteiligt.
//!
//! # Ablauf
//! [`generate_title`] baut aus einer [`TitleRequest`] einen kurzen
//! System-Prompt plus Nutzertext, ruft `complete_text` auf und säubert das
//! Ergebnis (erste Zeile, Anführungszeichen und Satzpunkt entfernt, auf 80
//! Zeichen gekürzt). [`ensure_title`] ist die Sidecar-Fassade darüber: sie
//! lädt/leitet [`harw_session_store::meta::load_or_derive`] ab, überspringt
//! Sessions, die bereits einen von Modell oder Nutzerin gesetzten Titel
//! tragen, und fällt sonst bei Fehler oder leerem Modell-Ergebnis auf
//! [`harw_session_store::meta::fallback_title`] zurück.
//! [`spawn_title_job`] startet `ensure_title` als eigenständigen
//! `tokio::spawn`-Task — der Aufrufer (z. B. der Turn-Loop nach der ersten
//! Antwort) wartet nie darauf; ein Fehlschlag landet nur im `tracing::warn!`.
//!
//! # Nebenläufigkeit
//! [`generate_title`] und [`ensure_title`] sind zustandslose `async fn`s.
//! [`spawn_title_job`] nimmt einen `Arc<dyn ModelProvider>` entgegen, damit
//! derselbe, im Root-Turn geteilte Provider (`crate::model::ModelSource`)
//! auch dem abgekoppelten Titel-Task zur Verfügung steht, ohne ihn zu klonen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_config::{InternalModelPoint, ResolvedConfig, resolve_internal_model};
use harw_core::PinnedModelProvider;
use harw_core::model::ModelProvider;
use harw_core::one_shot::complete_text;
use harw_macros::HarwError;
use harw_session_store::{SessionMeta, SessionStoreError, TitleSource, meta};
use harw_types::{ModelId, ProviderId, SessionId};

/// Obergrenze für die vom Modell angeforderten Ausgabe-Tokens.
const TITLE_MAX_OUTPUT_TOKENS: u32 = 30;

/// Obergrenze für den in den Prompt aufgenommenen Ausschnitt der ersten
/// Nutzernachricht.
const MAX_FIRST_MESSAGE_CHARS: usize = 2000;

/// Obergrenze für den in den Prompt aufgenommenen Ausschnitt der ersten
/// Assistant-Antwort.
const MAX_REPLY_CHARS: usize = 500;

/// Obergrenze für den gesäuberten Titel.
const MAX_TITLE_CHARS: usize = 80;

/// System-Prompt für die Titelgenerierung (siehe „Schritt 7" der Spec).
const TITLE_SYSTEM_PROMPT: &str = "Erzeuge einen Titel mit 3-6 Wörtern in der Sprache des Nutzers. Nur der Titel, keine Anführungszeichen, kein Punkt.";

/// Eingabe für [`generate_title`]: der Beginn einer Session, aus dem ein
/// Titel destilliert werden soll.
#[derive(Debug, Clone)]
pub struct TitleRequest {
    /// Session, für die ein Titel erzeugt werden soll.
    pub session_id: SessionId,
    /// Erste Nutzernachricht der Session (unbegrenzt; [`generate_title`]
    /// kürzt sie selbst auf [`MAX_FIRST_MESSAGE_CHARS`] Zeichen).
    pub first_user_message: String,
    /// Erste Assistant-Antwort der Session, falls zum Zeitpunkt des Aufrufs
    /// bereits vorhanden.
    pub first_assistant_reply: Option<String>,
}

/// Fehler von [`ensure_title`].
#[derive(Debug, HarwError)]
pub enum SessionTitleError {
    /// Der Sidecar (`<session-id>.meta.json`) konnte nicht gelesen oder
    /// geschrieben werden.
    #[from]
    Store(SessionStoreError),
}

/// Erzeugt per Modell-Aufruf einen kurzen Sitzungstitel.
///
/// # Description
/// Baut den Nutzerteil des Prompts aus `req.first_user_message` (gekürzt auf
/// [`MAX_FIRST_MESSAGE_CHARS`] Zeichen) und optional den ersten
/// [`MAX_REPLY_CHARS`] Zeichen von `req.first_assistant_reply`, ruft
/// [`complete_text`] mit [`TITLE_SYSTEM_PROMPT`] und
/// [`TITLE_MAX_OUTPUT_TOKENS`] auf und säubert das Ergebnis über
/// [`clean_title`]: nur die erste Zeile, Anführungszeichen entfernt,
/// abschließender Satzpunkt entfernt, auf [`MAX_TITLE_CHARS`] Zeichen
/// zeichensicher gekürzt.
///
/// # Returns
/// `Some(title)` bei einem nicht-leeren, gesäuberten Titel; `None`, wenn der
/// Modell-Aufruf fehlgeschlagen ist oder das gesäuberte Ergebnis leer war —
/// in beiden Fällen entscheidet der Aufrufer (siehe [`ensure_title`]) über
/// einen Fallback-Titel.
///
/// # Concurrency
/// `async fn`; führt genau den einen `.await` von [`complete_text`] aus.
pub async fn generate_title(
    provider: &dyn ModelProvider,
    model: &str,
    req: &TitleRequest,
) -> Option<String> {
    let mut user = truncate_chars(&req.first_user_message, MAX_FIRST_MESSAGE_CHARS);
    if let Some(reply) = req.first_assistant_reply.as_deref() {
        let reply_excerpt = truncate_chars(reply, MAX_REPLY_CHARS);
        if !reply_excerpt.is_empty() {
            if !user.is_empty() {
                user.push_str("\n\n");
            }
            user.push_str(&reply_excerpt);
        }
    }

    let raw = complete_text(
        provider,
        model,
        TITLE_SYSTEM_PROMPT,
        &user,
        TITLE_MAX_OUTPUT_TOKENS,
    )
    .await
    .ok()?;

    let cleaned = clean_title(&raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Stellt sicher, dass eine Session einen Titel trägt, und liefert die
/// (ggf. aktualisierten) Metadaten.
///
/// # Description
/// Lädt/leitet den Sidecar über
/// [`harw_session_store::meta::load_or_derive`] ab. Trägt die Session
/// bereits einen Titel mit Herkunft [`TitleSource::Model`] oder
/// [`TitleSource::Manual`], wird nichts verändert. Sonst versucht diese
/// Funktion [`generate_title`]; bei Erfolg wird der Titel mit
/// [`TitleSource::Model`] gesetzt
/// ([`harw_session_store::meta::set_title`]), sonst (Modell-Fehler oder
/// leeres Ergebnis) wird
/// [`harw_session_store::meta::fallback_title`] der ersten Nutzernachricht
/// mit [`TitleSource::Fallback`] gesetzt.
///
/// # Arguments
/// - `store_root` (`&Path`): Wurzel des Session-Stores (Sidecar-Verzeichnis).
/// - `id` (`&SessionId`): die zu betitelnde Session.
/// - `provider` (`&dyn ModelProvider`): Modell-Provider für [`generate_title`].
/// - `model` (`&str`): Modell-ID für den Titel-Aufruf.
///
/// # Returns
/// Die (ggf. aktualisierten) Sitzungs-Metadaten.
///
/// # Errors
/// [`SessionTitleError::Store`]: der Sidecar konnte nicht gelesen/abgeleitet
/// oder nicht geschrieben werden (siehe
/// [`harw_session_store::meta::load_or_derive`]/[`harw_session_store::meta::set_title`]).
///
/// # Concurrency
/// `async fn`; die Sidecar-I/O ist synchron (kein `.await` darin), der
/// Modell-Aufruf in [`generate_title`] ist der einzige nebenläufige Schritt.
pub async fn ensure_title(
    store_root: &Path,
    id: &SessionId,
    provider: &dyn ModelProvider,
    model: &str,
) -> Result<SessionMeta, SessionTitleError> {
    let current = meta::load_or_derive(store_root, id)?;
    if matches!(
        current.title_source,
        TitleSource::Model | TitleSource::Manual
    ) {
        return Ok(current);
    }

    let first_user_message = current.first_user_message.clone().unwrap_or_default();
    // Ohne erste Nutzernachricht gibt es nichts zu betiteln; ein leerer
    // Fallback-Titel würde sonst als `title = ""` gespeichert und der Picker
    // zeigte weiter „(ohne Titel)“.
    if first_user_message.trim().is_empty() {
        return Ok(current);
    }
    let request = TitleRequest {
        session_id: id.clone(),
        first_user_message: first_user_message.clone(),
        first_assistant_reply: None,
    };

    match generate_title(provider, model, &request).await {
        Some(title) => Ok(meta::set_title(store_root, id, &title, TitleSource::Model)?),
        None => {
            let fallback = meta::fallback_title(&first_user_message);
            Ok(meta::set_title(
                store_root,
                id,
                &fallback,
                TitleSource::Fallback,
            )?)
        }
    }
}

/// Wählt Provider/Modell-Pin für die Titel-Generierung nach den internen
/// Modellstellen (Addendum C, [`InternalModelPoint::SessionTitle`]).
///
/// # Description
/// Löst die Stelle über [`resolve_internal_model`] auf. Der Resolver deckt
/// das Legacy-`[session] title_model` bereits selbst als Regel (c) ab, ein
/// gesondertes Nachschlagen entfällt hier also. Ist die aufgelöste Stelle
/// das Hauptmodell ([`ResolvedInternalModel::is_main_model`](harw_config::ResolvedInternalModel::is_main_model)),
/// bleibt der Aufrufer unverändert bei seinem eigenen Modell (z. B. dem
/// aktiven Sitzungsmodell) — genau das bisherige Verhalten vor Addendum C.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration, aus der die
///   Stelle aufgelöst wird.
///
/// # Returns
/// `Some((provider_id, model_id))`, wenn [`spawn_title_job`] den
/// Modell-Anbieter über [`PinnedModelProvider`] fest verdrahten soll (Regel
/// (a)/(d) aus Addendum C: `Explicit` oder `OpenRouterDefault`); `None` für
/// `MainModel` (Legacy-Pfad, kein Pin).
pub fn title_model_selection(config: &ResolvedConfig) -> Option<(Option<ProviderId>, ModelId)> {
    let resolved = resolve_internal_model(config, InternalModelPoint::SessionTitle);
    if resolved.is_main_model() {
        return None;
    }
    let provider_id = resolved.provider.as_deref().map(ProviderId::from);
    let model_id = resolved.model.as_deref().map(ModelId::from)?;
    Some((provider_id, model_id))
}

/// Startet [`ensure_title`] als abgekoppelten `tokio::spawn`-Task.
///
/// # Description
/// Der Aufrufer wartet nie auf diesen Task: ein Fehlschlag von
/// [`ensure_title`] wird nur per `tracing::warn!` protokolliert, nie
/// zurückgegeben oder propagiert. Nimmt `Arc<dyn ModelProvider>` entgegen,
/// damit derselbe Root-Provider ohne Klon in den `'static`-Task wandert
/// (siehe Moduldoku „Nebenläufigkeit").
///
/// Ist `pin` gesetzt (siehe [`title_model_selection`]), wird `provider` vor
/// dem Aufruf in einen [`PinnedModelProvider`] gehüllt und `model` durch die
/// aufgelöste Modell-ID ersetzt — die interne Modellstelle
/// [`InternalModelPoint::SessionTitle`] gewinnt dann über das vom Aufrufer
/// übergebene `model` (Addendum C, Regel 4: „Explizite Wahl gewinnt immer").
/// Ist `pin` `None`, bleibt das bisherige Verhalten unverändert: `provider`
/// und `model` gehen unverändert an [`ensure_title`].
///
/// # Arguments
/// - `store_root` (`PathBuf`): Wurzel des Session-Stores; wird in den Task
///   verschoben.
/// - `id` (`SessionId`): die zu betitelnde Session; wird in den Task
///   verschoben.
/// - `provider` (`Arc<dyn ModelProvider>`): geteilter Root-Provider.
/// - `model` (`String`): Modell-ID für den Titel-Aufruf (Legacy-Pfad, wenn
///   `pin` `None` ist).
/// - `pin` (`Option<(Option<ProviderId>, ModelId)>`): das Ergebnis von
///   [`title_model_selection`], vom Aufrufer einmal pro Job aufgelöst.
///
/// # Concurrency
/// Spawnt einen `tokio`-Task auf dem aktuellen Runtime-Handle; erfordert
/// deshalb einen laufenden `tokio`-Executor-Kontext beim Aufruf. Panics
/// innerhalb des Tasks werden vom `JoinHandle` verschluckt (nicht abgefragt)
/// — bewusst, weil der Aufrufer nie auf den Task wartet.
pub fn spawn_title_job(
    store_root: PathBuf,
    id: SessionId,
    provider: Arc<dyn ModelProvider>,
    model: String,
    pin: Option<(Option<ProviderId>, ModelId)>,
) {
    let _handle = tokio::spawn(async move {
        let (effective_provider, effective_model): (Arc<dyn ModelProvider>, String) = match pin {
            Some((provider_id, model_id)) => {
                let pinned = PinnedModelProvider::new(
                    Arc::clone(&provider),
                    provider_id,
                    Some(model_id.clone()),
                );
                (Arc::new(pinned), model_id.as_str().to_owned())
            }
            None => (Arc::clone(&provider), model),
        };
        if let Err(error) = ensure_title(
            &store_root,
            &id,
            effective_provider.as_ref(),
            &effective_model,
        )
        .await
        {
            tracing::warn!(
                session = %id,
                error = %error,
                "session title generation failed; leaving fallback/existing title in place"
            );
        }
    });
}

/// Säubert eine rohe Modell-Antwort zu einem anzeigbaren Titel.
///
/// # Description
/// Nimmt nur die erste Zeile, entfernt alle Anführungszeichen-Varianten
/// (`"`, `'` sowie typografische Varianten), trimmt Leerraum, entfernt einen
/// abschließenden Satzpunkt (`.`) und kürzt zeichensicher auf
/// [`MAX_TITLE_CHARS`] Zeichen.
fn clean_title(raw: &str) -> String {
    let first_line = raw.lines().next().unwrap_or("").trim();
    let without_quotes: String = first_line
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '„' | '“' | '”' | '‚' | '‘' | '’'))
        .collect();
    let trimmed = without_quotes.trim();
    let without_period = trimmed.trim_end_matches('.').trim();
    truncate_chars(without_period, MAX_TITLE_CHARS)
}

/// Kürzt `input` zeichensicher (Unicode-Codepoints, nie mitten in einem
/// Mehrbyte-Zeichen) auf höchstens `max_chars` Zeichen.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_owned()
    } else {
        input.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::model::{ModelError, ModelFuture, ModelRequest, ModelResponse};

    /// Stub-Provider für die Tests dieses Moduls: liefert `reply`
    /// unverändert zurück, unabhängig vom eingehenden `ModelRequest`.
    struct StubProvider {
        reply: Result<String, ()>,
    }

    impl ModelProvider for StubProvider {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            let outcome = self.reply.clone();
            Box::pin(async move {
                match outcome {
                    Ok(text) => Ok(ModelResponse::text(text)),
                    Err(()) => Err(ModelError::RequestFailed("stub failure".to_owned())),
                }
            })
        }
    }

    fn runtime() -> TestResult<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("current-thread runtime"))
    }

    /// Seeds a sidecar with a first user message, without this test module
    /// needing to name `jiff::Timestamp` itself (harw-runtime does not
    /// depend on `jiff` directly): the timestamps come untouched from
    /// [`meta::load_or_derive`]'s own fresh-`SessionMeta` construction.
    fn seed_meta(root: &Path, id: &SessionId, first_user_message: &str) -> TestResult {
        let mut current = meta::load_or_derive(root, id).map_err(ctx("fresh meta derives"))?;
        current.first_user_message = Some(first_user_message.to_owned());
        meta::save(root, &current).map_err(ctx("seed meta saves"))
    }

    #[test]
    fn ensure_title_sets_a_model_title_on_success() -> TestResult {
        let temp = tempfile::tempdir()?;
        let session = SessionId::from_str("session-a");
        seed_meta(temp.path(), &session, "Wie baue ich einen Titel-Generator?")?;
        let provider = StubProvider {
            reply: Ok("Titel Generator Bauen".to_owned()),
        };

        let result = runtime()?
            .block_on(ensure_title(temp.path(), &session, &provider, "test-model"))
            .map_err(ctx("ensure_title succeeds"))?;

        assert_eq!(result.title_source, TitleSource::Model);
        assert_eq!(result.title.as_deref(), Some("Titel Generator Bauen"));
        Ok(())
    }

    #[test]
    fn ensure_title_falls_back_to_the_first_message_on_provider_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let session = SessionId::from_str("session-a");
        seed_meta(temp.path(), &session, "Erste Nachricht als Fallback-Quelle")?;
        let provider = StubProvider { reply: Err(()) };

        let result = runtime()?
            .block_on(ensure_title(temp.path(), &session, &provider, "test-model"))
            .map_err(ctx("ensure_title succeeds even on provider error"))?;

        let expected_fallback = meta::fallback_title("Erste Nachricht als Fallback-Quelle");
        assert_eq!(result.title_source, TitleSource::Fallback);
        assert_eq!(result.title.as_deref(), Some(expected_fallback.as_str()));
        Ok(())
    }

    #[test]
    fn ensure_title_does_not_overwrite_an_existing_model_title() -> TestResult {
        let temp = tempfile::tempdir()?;
        let session = SessionId::from_str("session-a");
        seed_meta(temp.path(), &session, "Erste Nachricht")?;
        meta::set_title(
            temp.path(),
            &session,
            "Bestehender Titel",
            TitleSource::Model,
        )
        .map_err(ctx("seed title"))?;
        let provider = StubProvider {
            reply: Ok("Anderer Titel Vom Modell".to_owned()),
        };

        let result = runtime()?
            .block_on(ensure_title(temp.path(), &session, &provider, "test-model"))
            .map_err(ctx("ensure_title succeeds"))?;

        assert_eq!(result.title_source, TitleSource::Model);
        assert_eq!(result.title.as_deref(), Some("Bestehender Titel"));
        Ok(())
    }

    #[test]
    fn ensure_title_does_not_overwrite_a_manual_title() -> TestResult {
        let temp = tempfile::tempdir()?;
        let session = SessionId::from_str("session-a");
        seed_meta(temp.path(), &session, "Erste Nachricht")?;
        meta::set_title(
            temp.path(),
            &session,
            "Von Hand Gesetzt",
            TitleSource::Manual,
        )
        .map_err(ctx("seed title"))?;
        let provider = StubProvider {
            reply: Ok("Anderer Titel Vom Modell".to_owned()),
        };

        let result = runtime()?
            .block_on(ensure_title(temp.path(), &session, &provider, "test-model"))
            .map_err(ctx("ensure_title succeeds"))?;

        assert_eq!(result.title_source, TitleSource::Manual);
        assert_eq!(result.title.as_deref(), Some("Von Hand Gesetzt"));
        Ok(())
    }

    #[test]
    fn generate_title_strips_quotes_and_uses_only_the_first_line() -> TestResult {
        let provider = StubProvider {
            reply: Ok("\"Mein Toller Titel\".\nZweite Zeile wird ignoriert".to_owned()),
        };
        let request = TitleRequest {
            session_id: SessionId::from_str("session-a"),
            first_user_message: "Erste Nachricht".to_owned(),
            first_assistant_reply: None,
        };

        let title = runtime()?
            .block_on(generate_title(&provider, "test-model", &request))
            .ok_or(TestError::Missing("cleaned title is non-empty"))?;

        assert_eq!(title, "Mein Toller Titel");
        Ok(())
    }

    #[test]
    fn generate_title_returns_none_for_a_blank_cleaned_result() -> TestResult {
        let provider = StubProvider {
            reply: Ok("\"'.".to_owned()),
        };
        let request = TitleRequest {
            session_id: SessionId::from_str("session-a"),
            first_user_message: "Erste Nachricht".to_owned(),
            first_assistant_reply: None,
        };

        let title = runtime()?.block_on(generate_title(&provider, "test-model", &request));

        assert_eq!(title, None);
        Ok(())
    }

    #[test]
    fn generate_title_returns_none_on_provider_error() -> TestResult {
        let provider = StubProvider { reply: Err(()) };
        let request = TitleRequest {
            session_id: SessionId::from_str("session-a"),
            first_user_message: "Erste Nachricht".to_owned(),
            first_assistant_reply: None,
        };

        let title = runtime()?.block_on(generate_title(&provider, "test-model", &request));

        assert_eq!(title, None);
        Ok(())
    }
}
