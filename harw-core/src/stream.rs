//! Provider-neutrales Token-Streaming.
//!
//! Ein [`ModelRequest`](crate::ModelRequest) kann einen [`StreamSink`] tragen.
//! Ein streaming-fähiger Provider liefert darüber Deltas (Text, Reasoning,
//! Tool-Call-Fragmente, Usage), **während** die Antwort eintrifft. Die
//! finale [`ModelResponse`](crate::ModelResponse) bleibt unverändert die
//! einzige Quelle der Wahrheit: Tool-Calls werden erst ausgeführt, wenn der
//! Provider die vollständige Antwort zurückgegeben hat.
//!
//! Provider ohne Streaming (oder mit `streaming = false` in der Modell-/
//! Provider-TOML) ignorieren den Sink; der Turn-Loop meldet dann Usage und
//! Text weiterhin pro Modell-Runde (Fallback).

use std::fmt;
use std::sync::Arc;

use harw_types::TokenUsage;

/// Ein einzelnes Delta eines laufenden Modell-Aufrufs.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStreamEvent {
    /// Neuer Assistant-Text.
    TextDelta(String),
    /// Neuer lesbarer Reasoning-Text (Anthropic `thinking_delta`, OpenAI
    /// `reasoning_summary_text.delta`).
    ReasoningDelta(String),
    /// Fragment eines Tool-Calls; `name`/`id` kommen typischerweise nur im
    /// ersten Fragment eines Index.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_fragment: String,
    },
    /// Kumulativer Usage-Stand des laufenden Aufrufs (nicht additiv).
    Usage(TokenUsage),
}

/// Empfänger für [`ModelStreamEvent`]s; billig klonbar, `Send + Sync`.
#[derive(Clone)]
pub struct StreamSink(Arc<dyn Fn(ModelStreamEvent) + Send + Sync>);

impl StreamSink {
    /// Baut einen Sink aus einer Closure.
    pub fn new(f: impl Fn(ModelStreamEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Liefert ein Event an den Sink (best effort, blockiert nie).
    pub fn emit(&self, event: ModelStreamEvent) {
        (self.0)(event);
    }
}

impl fmt::Debug for StreamSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamSink(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn sink_forwards_events_in_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let sink = StreamSink::new(move |event| {
            if let Ok(mut guard) = seen2.lock() {
                guard.push(event);
            }
        });
        sink.emit(ModelStreamEvent::TextDelta("a".into()));
        sink.clone().emit(ModelStreamEvent::TextDelta("b".into()));
        let got = seen.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(
            got,
            vec![
                ModelStreamEvent::TextDelta("a".into()),
                ModelStreamEvent::TextDelta("b".into())
            ]
        );
    }
}
