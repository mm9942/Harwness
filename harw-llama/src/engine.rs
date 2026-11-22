//! The llama.cpp engine: model loading (mmap) and token generation.
//!
//! The real implementation needs the `engine` feature; without it [`Engine::load`]
//! fails with [`LlamaError::Unavailable`] and nothing here links llama.cpp.

#[cfg(not(feature = "engine"))]
use std::path::Path;

use crate::DEFAULT_N_CTX;
use crate::error::LlamaError;

/// How the model is loaded.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Context window in tokens (capped by the model's trained context).
    pub n_ctx: u32,
    /// Generation threads; `0` picks a phone-friendly count.
    pub n_threads: i32,
    /// Prompt-processing batch size in tokens.
    pub n_batch: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            n_ctx: DEFAULT_N_CTX,
            n_threads: 0,
            n_batch: 512,
        }
    }
}

/// Sampling and length limits of one generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplingParams {
    /// Softmax temperature; `<= 0` selects greedy decoding.
    pub temperature: f32,
    /// Nucleus sampling threshold.
    pub top_p: f32,
    /// Top-k cut; `<= 0` disables it.
    pub top_k: i32,
    /// Min-p cut; `0` disables it.
    pub min_p: f32,
    /// Random seed.
    pub seed: u32,
    /// Most tokens the reply may have.
    pub max_new_tokens: u32,
}

impl SamplingParams {
    /// Qwen3.x recommended settings for thinking and non-thinking replies.
    #[must_use]
    pub fn for_mode(thinking: bool, max_new_tokens: u32, seed: u32) -> Self {
        if thinking {
            Self { temperature: 1.0, top_p: 0.95, top_k: 20, min_p: 0.0, seed, max_new_tokens }
        } else {
            Self { temperature: 0.7, top_p: 0.8, top_k: 20, min_p: 0.0, seed, max_new_tokens }
        }
    }
}

/// Why generation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// The model produced an end-of-generation token.
    EndOfGeneration,
    /// `max_new_tokens` was reached.
    MaxTokens,
    /// The context window is full.
    ContextFull,
    /// The caller asked to stop.
    Cancelled,
}

/// Token counts and end reason of one generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerateStats {
    /// Tokens of the prompt.
    pub prompt_tokens: u64,
    /// Tokens generated.
    pub completion_tokens: u64,
    /// Why generation ended.
    pub finish: FinishReason,
}

/// Default thread count: the big cores of a phone, not all of them.
#[cfg_attr(not(feature = "engine"), allow(dead_code))]
fn default_threads() -> i32 {
    let available = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    i32::try_from(available.clamp(2, 4)).unwrap_or(4)
}

#[cfg(feature = "engine")]
pub use real::Engine;
#[cfg(not(feature = "engine"))]
pub use stub::Engine;

#[cfg(not(feature = "engine"))]
mod stub {
    use super::{EngineConfig, GenerateStats, LlamaError, Path, SamplingParams};

    /// Placeholder of builds without llama.cpp.
    #[derive(Debug)]
    pub struct Engine {
        _private: (),
    }

    impl Engine {
        /// Always [`LlamaError::Unavailable`] in this build.
        pub fn load(_path: &Path, _config: &EngineConfig) -> Result<Self, LlamaError> {
            Err(LlamaError::Unavailable)
        }

        /// The chat template stored in the GGUF.
        #[must_use]
        pub fn chat_template(&self) -> Option<&str> {
            None
        }

        /// Text of the BOS token (empty when the model has none).
        #[must_use]
        pub fn bos_token(&self) -> &str {
            ""
        }

        /// Text of the EOS token.
        #[must_use]
        pub fn eos_token(&self) -> &str {
            ""
        }

        /// Context window in tokens.
        #[must_use]
        pub fn n_ctx(&self) -> u32 {
            0
        }

        /// Always [`LlamaError::Unavailable`] in this build.
        pub fn generate(
            &self,
            _prompt: &str,
            _sampling: &SamplingParams,
            _should_stop: &dyn Fn() -> bool,
            _on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            Err(LlamaError::Unavailable)
        }
    }
}

#[cfg(feature = "engine")]
mod real {
    use std::num::NonZeroU32;
    use std::path::Path;
    use std::sync::OnceLock;

    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::LlamaModel;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::sampling::LlamaSampler;

    use super::{EngineConfig, FinishReason, GenerateStats, SamplingParams, default_threads};
    use crate::error::LlamaError;
    use crate::stream::Utf8Stream;

    /// Reserved room for the reply when checking that a prompt fits.
    const REPLY_HEADROOM: usize = 64;

    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

    fn backend() -> Result<&'static LlamaBackend, LlamaError> {
        BACKEND
            .get_or_init(|| {
                // llama.cpp logs go to `tracing`, which has no subscriber on the phone.
                llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
                LlamaBackend::init().map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| LlamaError::Load(e.clone()))
    }

    fn generate_error(error: impl std::fmt::Display) -> LlamaError {
        LlamaError::Generate(error.to_string())
    }

    /// A loaded GGUF model. `Send + Sync`; generations create their own context.
    pub struct Engine {
        backend: &'static LlamaBackend,
        model: LlamaModel,
        template: Option<String>,
        bos: String,
        eos: String,
        n_ctx: u32,
        n_threads: i32,
        n_batch: u32,
    }

    impl std::fmt::Debug for Engine {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Engine").field("n_ctx", &self.n_ctx).finish_non_exhaustive()
        }
    }

    impl Engine {
        /// Memory-maps the GGUF at `path` and prepares it for generation.
        pub fn load(path: &Path, config: &EngineConfig) -> Result<Self, LlamaError> {
            if !path.is_file() {
                return Err(LlamaError::MissingModel(path.display().to_string()));
            }
            let backend = backend()?;
            let params = LlamaModelParams::default()
                .with_n_gpu_layers(0)
                .with_use_mmap(true);
            let model = LlamaModel::load_from_file(backend, path, &params)
                .map_err(|e| LlamaError::Load(e.to_string()))?;
            let template = model
                .chat_template(None)
                .ok()
                .and_then(|t| t.to_str().ok().map(str::to_owned));
            let vocab = model.vocab();
            let token_text = |token: llama_cpp_2::token::LlamaToken| {
                if token.0 < 0 {
                    String::new()
                } else {
                    vocab
                        .text(token)
                        .map(|text| text.to_string_lossy().into_owned())
                        .unwrap_or_default()
                }
            };
            let bos = token_text(vocab.bos());
            let eos = token_text(vocab.eos());
            drop(vocab);
            let trained = model.n_ctx_train().max(512);
            let n_ctx = config.n_ctx.max(512).min(trained);
            let n_threads = if config.n_threads > 0 {
                config.n_threads
            } else {
                default_threads()
            };
            Ok(Self {
                backend,
                model,
                template,
                bos,
                eos,
                n_ctx,
                n_threads,
                n_batch: config.n_batch.clamp(32, 2048),
            })
        }

        /// The chat template stored in the GGUF.
        #[must_use]
        pub fn chat_template(&self) -> Option<&str> {
            self.template.as_deref()
        }

        /// Text of the BOS token (empty when the model has none).
        #[must_use]
        pub fn bos_token(&self) -> &str {
            &self.bos
        }

        /// Text of the EOS token.
        #[must_use]
        pub fn eos_token(&self) -> &str {
            &self.eos
        }

        /// Context window in tokens.
        #[must_use]
        pub fn n_ctx(&self) -> u32 {
            self.n_ctx
        }

        /// Generates a reply to `prompt`, calling `on_text` with each decoded chunk.
        ///
        /// `should_stop` is polled between tokens and prompt batches.
        pub fn generate(
            &self,
            prompt: &str,
            sampling: &SamplingParams,
            should_stop: &dyn Fn() -> bool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            let vocab = self.model.vocab();
            // The template already wrote every special token; parse them as such.
            let tokens = vocab.tokenize(prompt.as_bytes(), false, true);
            let n_ctx = self.n_ctx as usize;
            if tokens.is_empty() || tokens.len() + REPLY_HEADROOM > n_ctx {
                return Err(LlamaError::ContextLength {
                    needed: tokens.len() + REPLY_HEADROOM,
                    available: n_ctx,
                });
            }
            let prompt_tokens = tokens.len() as u64;
            let stats = |completion: u64, finish| GenerateStats {
                prompt_tokens,
                completion_tokens: completion,
                finish,
            };

            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(self.n_ctx))
                .with_n_batch(self.n_batch)
                .with_n_ubatch(self.n_batch)
                .with_n_threads(self.n_threads)
                .with_n_threads_batch(self.n_threads);
            let mut ctx = self
                .model
                .new_context(self.backend, ctx_params)
                .map_err(generate_error)?;
            let mut batch = LlamaBatch::new(self.n_batch as usize, 1);

            // Prompt prefill in batches; only the very last token needs logits.
            let last = tokens.len() - 1;
            let mut pos: i32 = 0;
            for chunk in tokens.chunks(self.n_batch as usize) {
                if should_stop() {
                    return Ok(stats(0, FinishReason::Cancelled));
                }
                batch.clear();
                for token in chunk {
                    let index = pos as usize;
                    batch
                        .add(*token, pos, &[0], index == last)
                        .map_err(generate_error)?;
                    pos += 1;
                }
                ctx.decode(&mut batch).map_err(generate_error)?;
            }

            let mut sampler = if sampling.temperature <= 0.0 {
                LlamaSampler::greedy()
            } else {
                let mut chain = Vec::new();
                if sampling.top_k > 0 {
                    chain.push(LlamaSampler::top_k(sampling.top_k));
                }
                if sampling.top_p > 0.0 && sampling.top_p < 1.0 {
                    chain.push(LlamaSampler::top_p(sampling.top_p, 1));
                }
                if sampling.min_p > 0.0 {
                    chain.push(LlamaSampler::min_p(sampling.min_p, 1));
                }
                chain.push(LlamaSampler::temp(sampling.temperature));
                chain.push(LlamaSampler::dist(sampling.seed));
                LlamaSampler::chain_simple(chain)
            };

            let mut decoder = Utf8Stream::default();
            let mut generated: u64 = 0;
            let mut logits_index = batch.n_tokens() - 1;
            let finish = loop {
                if should_stop() {
                    break FinishReason::Cancelled;
                }
                if generated >= u64::from(sampling.max_new_tokens) {
                    break FinishReason::MaxTokens;
                }
                if pos as usize >= n_ctx {
                    break FinishReason::ContextFull;
                }
                let token = sampler.sample(&ctx, logits_index);
                if vocab.is_eog(token) {
                    break FinishReason::EndOfGeneration;
                }
                generated += 1;
                // `special = true`: render `<think>`/`<tool_call>` even if flagged special.
                let text = decoder.push(&vocab.token_to_piece(token, true, None));
                if !text.is_empty() {
                    on_text(&text);
                }
                batch.clear();
                batch.add(token, pos, &[0], true).map_err(generate_error)?;
                pos += 1;
                logits_index = 0;
                ctx.decode(&mut batch).map_err(generate_error)?;
            };
            Ok(stats(generated, finish))
        }
    }
}
