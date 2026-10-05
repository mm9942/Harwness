//! `learning_extract`-Jobs: LLM-Extraktion als Job des Job-Workers (X6).
//!
//! Die Runtime reiht den Job nur ein (nur wenn `[memory] llm_extraction`
//! gesetzt ist); dieser Worker claimt ihn wie jede andere Job-Art und ruft den
//! gemeinsamen Treiber [`harw_ops::learning_job::run_learning_extract_job`]
//! mit dem Modell-Provider des Workers. Der Modellaufruf trägt weder Tools
//! noch Kontext: nur den Extraktions-Prompt über dem geschwärzten Digest.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use harw_core::{ConversationHistory, ModelProvider, ModelRequest};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome};
use harw_ops::learning_job::{
    ExtractionModel, LearningExtractSpec, is_learning_extract_kind, run_learning_extract_job,
};
use harw_session_store::JobStore;

use super::WorkerExecutionControl;

const INVALID_PAYLOAD_REASON: &str = "invalid learning_extract payload";
/// Obergrenze der Ausgabe-Tokens des Extraktionsaufrufs.
const MAX_OUTPUT_TOKENS: u32 = 1500;

/// `true` für die Job-Art der LLM-Extraktion.
pub(super) fn is_learning_kind(kind: &JobKind) -> bool {
    is_learning_extract_kind(kind)
}

/// Adapter: Modell-Provider des Workers als [`ExtractionModel`].
struct ProviderExtractionModel {
    provider: Arc<dyn ModelProvider>,
}

impl ExtractionModel for ProviderExtractionModel {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut history = ConversationHistory::new();
            history.push_user_text(user.to_owned());
            let request = ModelRequest {
                stream: None,
                system_prompt: system.to_owned(),
                instruction_fragments: Vec::new(),
                context: Vec::new(),
                history,
                tools: Vec::new(),
                context_assembly: Default::default(),
                reasoning_effort: None,
                model_id: None,
                provider_id: None,
                data_block: None,
                max_output_tokens: Some(MAX_OUTPUT_TOKENS),
                tool_result_max_bytes: None,
                cancel: None,
                identity: None,
            };
            let response = self
                .provider
                .respond(request)
                .await
                .map_err(|error| error.to_string())?;
            response
                .message
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| "empty model answer".to_owned())
        })
    }
}

/// Führt einen geclaimten `learning_extract`-Job aus.
///
/// # Returns
/// `Succeeded { result }`, `Cancelled` bei Abbruch, `Failed` mit
/// `timed_out: …` bei Fristablauf, sonst `Failed` (ungültige Payload).
pub(super) async fn execute_learning_extract_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    let spec: LearningExtractSpec = match serde_json::from_value(input) {
        Ok(spec) => spec,
        Err(error) => {
            tracing::warn!(work_id = %work_id.as_str(), %error, "learning_extract payload rejected");
            return JobOutcome::Failed {
                reason: INVALID_PAYLOAD_REASON.to_owned(),
            };
        }
    };
    let mut cancellation = control.cancellation();
    let signal = async move {
        loop {
            if *cancellation.borrow() {
                return;
            }
            if cancellation.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    let model: Arc<dyn ExtractionModel> = Arc::new(ProviderExtractionModel { provider });
    run_learning_extract_job(spec, work_id, job_store, signal, model).await
}
