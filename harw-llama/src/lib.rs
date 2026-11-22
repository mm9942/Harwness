//! On-device GGUF inference for the phone app.
//!
//! [`LocalGgufProvider`] is a [`harw_core::ModelProvider`] that runs a GGUF model
//! (target: `Qwen3.5-4B-Q4_K_M`, architecture `qwen35`) in-process through
//! llama.cpp. The prompt is rendered from the chat template stored in the GGUF
//! with a pure-Rust Jinja engine, so tools and the thinking switch reach the
//! template; output is streamed as text/reasoning deltas and tool calls are
//! parsed from the model's `<tool_call>` markup.
//!
//! The llama.cpp binding (`llama-cpp-2`, which builds llama.cpp with cmake) sits
//! behind the opt-in `engine` feature. Without it everything but generation
//! compiles, and [`LocalGgufProvider::open`] returns [`LlamaError::Unavailable`].

mod engine;
mod error;
mod provider;
mod stream;
mod template;
mod toolcall;

pub use engine::{Engine, EngineConfig, FinishReason, GenerateStats, SamplingParams};
pub use error::LlamaError;
pub use provider::{LOCAL_PROVIDER_ID, LocalGgufConfig, LocalGgufProvider};

/// Context size used when the caller does not choose one.
pub const DEFAULT_N_CTX: u32 = 16384;

/// Whether this build contains the llama.cpp engine.
#[must_use]
pub const fn engine_available() -> bool {
    cfg!(feature = "engine")
}
