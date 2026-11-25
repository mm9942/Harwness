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
    /// Vision projector (`mmproj` GGUF) that lets the model read images.
    pub mmproj: Option<std::path::PathBuf>,
    /// Lower bound of the tokens one image costs (`-1`: the projector's default).
    pub image_min_tokens: i32,
    /// Upper bound of the tokens one image costs; mtmd scales larger images down
    /// to fit (`-1`: the projector's default, up to 4096 tokens).
    pub image_max_tokens: i32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            n_ctx: DEFAULT_N_CTX,
            n_threads: 0,
            n_batch: 512,
            mmproj: None,
            image_min_tokens: DEFAULT_IMAGE_MIN_TOKENS,
            image_max_tokens: DEFAULT_IMAGE_MAX_TOKENS,
        }
    }
}

/// Text that stands for one image in a prompt (libmtmd's default marker).
pub const MEDIA_MARKER: &str = "<__media__>";

/// Image token budget on a phone CPU: about 64..256 tokens, i.e. images are scaled
/// to roughly 512x512 pixels (Qwen3.5: one token covers 32x32 pixels).
pub const DEFAULT_IMAGE_MIN_TOKENS: i32 = 64;
/// See [`DEFAULT_IMAGE_MIN_TOKENS`].
pub const DEFAULT_IMAGE_MAX_TOKENS: i32 = 256;

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

        /// Whether a vision projector is loaded.
        #[must_use]
        pub fn supports_vision(&self) -> bool {
            false
        }

        /// Why the projector did not load (never in this build).
        #[must_use]
        pub fn vision_error(&self) -> Option<&str> {
            None
        }

        /// Always [`LlamaError::Unavailable`] in this build.
        pub fn generate(
            &self,
            _prompt: &str,
            _images: &[&[u8]],
            _sampling: &SamplingParams,
            _should_stop: &dyn Fn() -> bool,
            _on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            Err(LlamaError::Unavailable)
        }

        pub(crate) fn generate_compact_vision(
            &self,
            prompt: &str,
            images: &[&[u8]],
            sampling: &SamplingParams,
            should_stop: &dyn Fn() -> bool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            self.generate(prompt, images, sampling, should_stop, on_text)
        }
    }
}

#[cfg(feature = "engine")]
mod real {
    use std::num::NonZeroU32;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock, PoisonError};

    use llama_cpp_2::context::LlamaContext;
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::LlamaModel;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::mtmd::{
        MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText,
    };
    use llama_cpp_2::sampling::LlamaSampler;
    use llama_cpp_2::token::LlamaToken;

    use super::{EngineConfig, FinishReason, GenerateStats, SamplingParams, StopFilter, default_threads};
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
        /// The vision projector; encoding needs `&mut`, and generations are
        /// serialised by the provider anyway.
        vision: Option<Mutex<MtmdContext>>,
        /// Why a configured projector could not be used.
        vision_error: Option<String>,
        template: Option<String>,
        bos: String,
        eos: String,
        n_ctx: u32,
        n_threads: i32,
        n_batch: u32,
    }

    impl std::fmt::Debug for Engine {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Engine")
                .field("n_ctx", &self.n_ctx)
                .field("vision", &self.vision.is_some())
                .finish_non_exhaustive()
        }
    }

    impl Engine {
        /// Memory-maps the GGUF at `path` and prepares it for generation.
        ///
        /// A projector that fails to load is not fatal: the engine then reads text only.
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
            let (vision, vision_error) = match config.mmproj.as_deref() {
                None => (None, None),
                Some(mmproj) => match load_vision(mmproj, &model, config, n_threads) {
                    Ok(context) => (Some(context), None),
                    Err(reason) => {
                        tracing::warn!("{reason}");
                        (None, Some(reason))
                    }
                },
            };
            Ok(Self {
                backend,
                model,
                vision,
                vision_error,
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

        /// Whether a vision projector is loaded.
        #[must_use]
        pub fn supports_vision(&self) -> bool {
            self.vision.is_some()
        }

        /// Why the configured projector could not be loaded, if it could not.
        #[must_use]
        pub fn vision_error(&self) -> Option<&str> {
            self.vision_error.as_deref()
        }

        /// Generates a reply to `prompt`, calling `on_text` with each decoded chunk.
        ///
        /// With `images`, `prompt` holds one media marker ([`MEDIA_MARKER`]) per
        /// image, in order; the images are encoded by the vision projector.
        /// `should_stop` is polled between tokens and prompt batches.
        pub fn generate(
            &self,
            prompt: &str,
            images: &[&[u8]],
            sampling: &SamplingParams,
            should_stop: &dyn Fn() -> bool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            self.generate_with_limits(
                prompt, images, sampling, should_stop, on_text,
                (self.n_ctx, self.n_batch, self.n_threads),
            )
        }

        /// Short caption-only contexts. Ordinary chat and private image turns
        /// keep their original context, batch size and CPU threading.
        pub(crate) fn generate_compact_vision(
            &self,
            prompt: &str,
            images: &[&[u8]],
            sampling: &SamplingParams,
            should_stop: &dyn Fn() -> bool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<GenerateStats, LlamaError> {
            self.generate_with_limits(
                prompt, images, sampling, should_stop, on_text,
                (self.n_ctx.min(2048), self.n_batch.min(128), 1),
            )
        }

        fn generate_with_limits(
            &self,
            prompt: &str,
            images: &[&[u8]],
            sampling: &SamplingParams,
            should_stop: &dyn Fn() -> bool,
            on_text: &mut dyn FnMut(&str),
            limits: (u32, u32, i32),
        ) -> Result<GenerateStats, LlamaError> {
            let (context_size, batch_size, threads) = limits;
            let vocab = self.model.vocab();
            let n_ctx = context_size as usize;
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                .with_n_batch(batch_size)
                .with_n_ubatch(batch_size)
                .with_n_threads(threads)
                .with_n_threads_batch(threads);
            let mut batch = LlamaBatch::new(batch_size as usize, 1);

            let (mut ctx, mut pos, mut logits_index, prompt_tokens);
            if images.is_empty() {
                // The template already wrote every special token; parse them as such.
                let tokens = vocab.tokenize(prompt.as_bytes(), false, true);
                if tokens.is_empty() || tokens.len() + REPLY_HEADROOM > n_ctx {
                    return Err(LlamaError::ContextLength {
                        needed: tokens.len() + REPLY_HEADROOM,
                        available: n_ctx,
                    });
                }
                prompt_tokens = tokens.len() as u64;
                ctx = self
                    .model
                    .new_context(self.backend, ctx_params)
                    .map_err(generate_error)?;
                let Some(next_pos) = prefill_text(&mut ctx, &mut batch, &tokens, batch_size, should_stop)? else {
                    return Ok(GenerateStats {
                        prompt_tokens,
                        completion_tokens: 0,
                        finish: FinishReason::Cancelled,
                    });
                };
                pos = next_pos;
                logits_index = batch.n_tokens() - 1;
            } else {
                let vision = self.vision.as_ref().ok_or_else(|| {
                    LlamaError::Generate("this model has no vision projector loaded".to_owned())
                })?;
                let mut vision = vision.lock().unwrap_or_else(PoisonError::into_inner);
                let marker = super::MEDIA_MARKER;
                if prompt.matches(marker).count() != images.len() {
                    return Err(LlamaError::Generate(
                        "the prompt has a different number of image markers than images".to_owned(),
                    ));
                }
                let bitmaps = images
                    .iter()
                    .map(|data| {
                        MtmdBitmap::from_buffer(&vision, data, false)
                            .map_err(|e| LlamaError::Generate(format!("could not read the image: {e}")))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let refs: Vec<&MtmdBitmap> = bitmaps.iter().collect();
                let chunks = vision
                    .tokenize(
                        MtmdInputText {
                            text: prompt.to_owned(),
                            add_special: false,
                            parse_special: true,
                        },
                        &refs,
                    )
                    .map_err(|e| LlamaError::Generate(format!("could not encode the image: {e}")))?;
                let needed = chunks.total_tokens() + REPLY_HEADROOM;
                if chunks.is_empty() || needed > n_ctx {
                    return Err(LlamaError::ContextLength { needed, available: n_ctx });
                }
                prompt_tokens = chunks.total_tokens() as u64;
                if should_stop() {
                    return Ok(GenerateStats {
                        prompt_tokens,
                        completion_tokens: 0,
                        finish: FinishReason::Cancelled,
                    });
                }
                ctx = self
                    .model
                    .new_context(self.backend, ctx_params)
                    .map_err(generate_error)?;
                // Encodes the images and decodes text and image chunks in order;
                // the logits of the last token are kept (index -1).
                pos = chunks
                    .eval_chunks(&mut vision, &mut ctx, 0, 0, batch_size as i32, true)
                    .map_err(|e| LlamaError::Generate(format!("image prefill failed: {e}")))?;
                logits_index = -1;
            }

            let mut sampler = self.sampler(sampling);

            let mut decoder = Utf8Stream::default();
            let mut stop_filter = StopFilter::default();
            let mut generated: u64 = 0;
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
                let (emit, hit_stop) = stop_filter.push(&text);
                if !emit.is_empty() {
                    on_text(&emit);
                }
                if hit_stop {
                    break FinishReason::EndOfGeneration;
                }
                batch.clear();
                batch.add(token, pos, &[0], true).map_err(generate_error)?;
                pos += 1;
                logits_index = 0;
                ctx.decode(&mut batch).map_err(generate_error)?;
            };
            Ok(GenerateStats { prompt_tokens, completion_tokens: generated, finish })
        }

        fn sampler(&self, sampling: &SamplingParams) -> LlamaSampler {
            if sampling.temperature <= 0.0 {
                return LlamaSampler::greedy();
            }
            // Repetition penalty keeps small models from getting stuck in loops.
            let mut chain = vec![LlamaSampler::penalties(self.model.n_vocab(), 256, 1.08, 0.0, 0.4)];
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
        }
    }

    // Prompt prefill in batches; only the last token needs logits. The caller has checked
    // that tokens is nonempty. None means cancelled before a subsequent batch.
    fn prefill_text(
        ctx: &mut LlamaContext<'_>,
        batch: &mut LlamaBatch<'_>,
        tokens: &[LlamaToken],
        batch_size: u32,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<Option<i32>, LlamaError> {
        let last = tokens.len() - 1;
        let mut pos = 0_i32;
        for chunk in tokens.chunks(batch_size as usize) {
            if should_stop() {
                return Ok(None);
            }
            batch.clear();
            for token in chunk {
                let index = pos as usize;
                batch
                    .add(*token, pos, &[0], index == last)
                    .map_err(generate_error)?;
                pos += 1;
            }
            ctx.decode(batch).map_err(generate_error)?;
        }
        Ok(Some(pos))
    }

    /// Loads the projector; the reason when it cannot be used.
    fn load_vision(
        mmproj: &Path,
        model: &LlamaModel,
        config: &EngineConfig,
        n_threads: i32,
    ) -> Result<Mutex<MtmdContext>, String> {
        let params = MtmdContextParams {
            use_gpu: false,
            print_timings: false,
            n_threads,
            media_marker: std::ffi::CString::new(super::MEDIA_MARKER)
                .map_err(|e| e.to_string())?,
            image_min_tokens: config.image_min_tokens,
            image_max_tokens: config.image_max_tokens,
        };
        let path = mmproj
            .to_str()
            .ok_or_else(|| "the projector path is not valid text".to_owned())?;
        match MtmdContext::init_from_file(path, model, &params) {
            Ok(context) if context.support_vision() => Ok(Mutex::new(context)),
            Ok(_) => Err(format!("the projector {} has no vision encoder", mmproj.display())),
            Err(error) => Err(format!(
                "could not load the projector {}: {error}",
                mmproj.display()
            )),
        }
    }
}

/// Textuelle Endmarken, die manche Modelle als normalen Text ausgeben (statt als Sondertoken).
const STOP_STRINGS: [&str; 5] = ["<|eos|>", "<|im_end|>", "<|endoftext|>", "</s>", "<|end|>"];

/// Filtert Endmarken aus dem Textstrom, auch wenn sie sich auf mehrere Token verteilen.
#[derive(Default)]
struct StopFilter {
    held: String,
}

impl StopFilter {
    /// Gibt (auszugebender Text, Endmarke gefunden) zurück; ein möglicher Marken-Anfang wird zurückgehalten.
    fn push(&mut self, text: &str) -> (String, bool) {
        self.held.push_str(text);
        for stop in STOP_STRINGS {
            if let Some(at) = self.held.find(stop) {
                let emit = self.held[..at].to_owned();
                self.held.clear();
                return (emit, true);
            }
        }
        // Längstes Ende von `held`, das ein Anfang einer Endmarke sein könnte, zurückhalten.
        let mut keep = 0;
        for stop in STOP_STRINGS {
            for len in (1..stop.len()).rev() {
                if self.held.len() >= len && self.held.is_char_boundary(self.held.len() - len) && self.held.ends_with(&stop[..len]) {
                    keep = keep.max(len);
                    break;
                }
            }
        }
        let cut = self.held.len() - keep;
        let emit = self.held[..cut].to_owned();
        self.held = self.held[cut..].to_owned();
        (emit, false)
    }
}
