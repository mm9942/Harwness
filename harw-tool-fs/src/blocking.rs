//! Ausführung blockierender Tool-Arbeit außerhalb des async-Executors (W1-02).
//!
//! Walks und Datei-IO der fs-Tools laufen über [`run_blocking`] in
//! `tokio::task::spawn_blocking`, damit ein langer Walk den (häufig
//! `current_thread`-)Runtime des Harness nicht einfriert. Wird das Future
//! verworfen (Turn-Abbruch), läuft die bereits gestartete Arbeit bis zu ihrer
//! eigenen Deadline weiter; ihr Ergebnis wird verworfen.

use harw_tools::{ToolOutput, ToolsError};

/// Führt `job` im Blocking-Pool des aktuellen Tokio-Runtimes aus.
///
/// Ohne umgebenden Runtime (z. B. synchroner Aufrufer mit eigenem Executor)
/// wird `job` direkt ausgeführt, statt wie `tokio::task::spawn_blocking` zu
/// paniken.
///
/// # Errors
/// Das Ergebnis von `job`. Ein Panic im Blocking-Task wird als
/// `Ok(ToolOutput::error(..))` gemeldet.
pub async fn run_blocking<F>(tool: &'static str, job: F) -> Result<ToolOutput, ToolsError>
where
    F: FnOnce() -> Result<ToolOutput, ToolsError> + Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current().ok();
    match handle {
        Some(handle) => match handle.spawn_blocking(job).await {
            Ok(result) => result,
            Err(err) => Ok(ToolOutput::error(format!(
                "{tool}: interner Fehler im Blocking-Task: {err}"
            ))),
        },
        None => job(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[tokio::test]
    async fn arbeit_laeuft_nicht_auf_dem_runtime_thread() -> TestResult {
        let caller = std::thread::current().id();
        let output = run_blocking("fs.test", move || {
            let worker = std::thread::current().id();
            Ok(ToolOutput::text(if worker == caller { "gleich" } else { "anders" }))
        })
        .await?;
        match output {
            ToolOutput::Text { content } => assert_eq!(content, "anders"),
            other => return Err(TestError::Unexpected(format!("unexpected: {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn ohne_runtime_wird_direkt_ausgefuehrt() -> TestResult {
        use std::task::{Context, Poll, Waker};

        assert!(tokio::runtime::Handle::try_current().is_err());
        let mut cx = Context::from_waker(Waker::noop());
        let job = || Ok(ToolOutput::text("ok"));
        let mut future = std::pin::pin!(run_blocking("fs.test", job));
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(ToolOutput::Text { content })) => assert_eq!(content, "ok"),
            Poll::Ready(other) => {
                return Err(TestError::Unexpected(format!("unexpected: {other:?}")));
            }
            Poll::Pending => {
                return Err(TestError::Unexpected(
                    "ohne Runtime muss das Ergebnis sofort bereitstehen".to_string(),
                ));
            }
        }
        Ok(())
    }
}
