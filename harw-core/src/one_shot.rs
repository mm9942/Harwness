//! Ein einziger Modell-Aufruf ohne Werkzeuge und ohne Turn-Schleife.
//!
//! Spec: `/home/mia/.claude/plans/nope-permissions-gibt-es-wild-lobster.md`
//! „Schritt 7" (Session-Titel und Resume-Picker) — dieser Knoten liefert den
//! zugrunde liegenden One-Shot-Baustein, den `harw-runtime/src/session_title.rs`
//! für die Titelgenerierung nutzt.
//!
//! # Kein Werkzeugpfad, keine Freigabekette
//! [`complete_text`] baut einen [`ModelRequest`] mit einer **leeren**
//! Werkzeugliste (`Vec::new()`) und ruft [`ModelProvider::respond`] genau
//! einmal auf — es gibt keine Turn-Schleife (`crate::turn_loop`), die aus der
//! Antwort zurückkommende Tool-Calls ausführen könnte. Antwortet ein Modell
//! trotzdem mit Tool-Calls, ignoriert diese Funktion sie schlicht (gelesen
//! wird nur `ModelResponse::message`, nie `tool_calls`). Weil hier niemals
//! ein Werkzeug ausgeführt wird, gibt es auch **keine** Freigabekette
//! (`crate::policy::ConfigApprovalPolicy`, `crate::session::PendingApproval`)
//! zu durchlaufen — dieser Pfad ist bewusst so schmal gehalten, dass er ohne
//! jede Genehmigungslogik sicher bleibt.

use crate::history::ConversationHistory;
use crate::model::{ModelError, ModelProvider, ModelRequest};
use harw_extension_api::LoadedInstructions;
use harw_macros::HarwError;
use harw_types::ModelId;

/// Fehler eines [`complete_text`]-Aufrufs.
#[derive(Debug, HarwError)]
pub enum OneShotError {
    /// Der zugrunde liegende Modell-Aufruf ist fehlgeschlagen; Text und
    /// Ursache kommen unverändert von [`ModelError`] (`source()` verlinkt
    /// dorthin).
    #[from]
    Model(ModelError),

    /// Das Modell hat geantwortet, aber die Antwort enthielt (nach dem
    /// Trimmen) keinen Text.
    #[msg("model returned an empty text response")]
    EmptyResponse,
}

/// Führt genau einen Modell-Aufruf ohne Werkzeuge aus und liefert den reinen
/// Antworttext.
///
/// # Description
/// Baut einen [`ModelRequest`] aus `system` (System-Prompt) und `user` (als
/// einzige Nutzernachricht in einer frischen [`ConversationHistory`]), ohne
/// Kontext-Fragmente und ohne Werkzeuge (`ModelRequest::new` mit
/// `Vec::new()` als Tool-Liste), ruft `provider.respond(..)` genau einmal
/// auf und gibt `response.message` getrimmt zurück. Siehe die Modul-Doku
/// „Kein Werkzeugpfad, keine Freigabekette" — es gibt in diesem Aufruf weder
/// eine Turn-Schleife noch einen Genehmigungspfad.
///
/// # Arguments
/// - `provider` (`&dyn ModelProvider`): der aufzurufende Modell-Provider.
/// - `model` (`&str`): Modell-ID, die für diesen Request verwendet wird
///   (`ModelRequest::with_model_id`).
/// - `system` (`&str`): System-Prompt des Requests.
/// - `user` (`&str`): einzige Nutzernachricht des Requests.
/// - `max_output_tokens` (`u32`): Obergrenze für die Ausgabe-Tokens
///   (`ModelRequest::with_max_output_tokens`).
///
/// # Returns
/// Der getrimmte Antworttext (`response.message`), nie leer.
///
/// # Errors
/// - [`OneShotError::Model`]: der Modell-Aufruf selbst ist fehlgeschlagen.
/// - [`OneShotError::EmptyResponse`]: die Antwort enthielt keinen (oder nach
///   dem Trimmen keinen) Text.
///
/// # Concurrency
/// `async fn`; führt genau einen `.await` auf `provider.respond(..)` aus und
/// hält dabei keine eigenen Locks.
///
/// # Examples
/// ```rust,no_run
/// use harw_core::one_shot::complete_text;
/// use harw_core::EchoModelProvider;
///
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// let provider = EchoModelProvider::default();
/// let text = complete_text(&provider, "echo-model", "sei kurz", "hallo", 32).await?;
/// println!("{text}");
/// # Ok(())
/// # }
/// ```
pub async fn complete_text(
    provider: &dyn ModelProvider,
    model: &str,
    system: &str,
    user: &str,
    max_output_tokens: u32,
) -> Result<String, OneShotError> {
    let instructions = LoadedInstructions {
        system_prompt: system.to_owned(),
        fragments: Vec::new(),
    };
    let mut history = ConversationHistory::new();
    history.push_user_text(user);

    let request = ModelRequest::new(instructions, Vec::new(), history, Vec::new())
        .with_model_id(Some(ModelId::from(model)))
        .with_max_output_tokens(Some(max_output_tokens));

    let response = provider.respond(request).await?;
    let trimmed = response.message.unwrap_or_default().trim().to_owned();
    if trimmed.is_empty() {
        return Err(OneShotError::EmptyResponse);
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelFuture, ModelResponse};

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

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
    }

    #[test]
    fn complete_text_trims_and_returns_the_model_text() {
        let provider = StubProvider {
            reply: Ok("  Hallo Welt  ".to_owned()),
        };

        let text = runtime()
            .block_on(complete_text(&provider, "m", "system", "user", 16))
            .expect("stub reply succeeds");

        assert_eq!(text, "Hallo Welt");
    }

    #[test]
    fn complete_text_propagates_model_errors() {
        let provider = StubProvider { reply: Err(()) };

        let error = runtime()
            .block_on(complete_text(&provider, "m", "system", "user", 16))
            .expect_err("stub failure propagates");

        assert!(matches!(
            error,
            OneShotError::Model(ModelError::RequestFailed(_))
        ));
    }

    #[test]
    fn complete_text_rejects_a_blank_response() {
        let provider = StubProvider {
            reply: Ok("   \n\t  ".to_owned()),
        };

        let error = runtime()
            .block_on(complete_text(&provider, "m", "system", "user", 16))
            .expect_err("blank response is rejected");

        assert!(matches!(error, OneShotError::EmptyResponse));
    }
}
