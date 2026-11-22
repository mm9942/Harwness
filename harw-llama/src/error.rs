//! Errors of the on-device engine.

/// Failure of loading a model, rendering a prompt or generating.
#[derive(Debug, thiserror::Error)]
pub enum LlamaError {
    /// This build has no llama.cpp engine (the `engine` feature is off).
    #[error("on-device inference is not available in this build")]
    Unavailable,
    /// The model file is missing or unreadable.
    #[error("model file not found: {0}")]
    MissingModel(String),
    /// llama.cpp could not initialise or load the model.
    #[error("could not load the model: {0}")]
    Load(String),
    /// The chat template could not be rendered.
    #[error("could not render the chat template: {0}")]
    Template(String),
    /// The prompt does not fit the context window.
    #[error("the prompt needs {needed} tokens but the context holds {available}")]
    ContextLength {
        /// Tokens the prompt needs, including room for the reply.
        needed: usize,
        /// Tokens the context window holds.
        available: usize,
    },
    /// Decoding failed inside llama.cpp.
    #[error("generation failed: {0}")]
    Generate(String),
}
