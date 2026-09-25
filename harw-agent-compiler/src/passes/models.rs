//! `ResolveModels`.

use std::collections::BTreeSet;

use harw_agent_dsl::ir_v2::Interface;
use harw_agent_dsl::{Diagnostic, Diagnostics};

use super::Pass;
use crate::codes;
use crate::unit::CompileUnit;

/// Environment variable holding the bearer token of the `http` interface.
pub const HTTP_TOKEN_ENV: &str = "HARW_AGENT_HTTP_TOKEN";

/// The credential variable of a model provider, if it needs one.
#[must_use]
pub fn provider_env(provider: &str) -> Option<&'static str> {
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "google" | "gemini" => Some("GEMINI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        _ => None,
    }
}

/// Puts the environment variables the agent needs at runtime into the
/// manifest (`permissions.required_env`).
///
/// # Description
/// - `[models] required_env` is already in the manifest (lowering).
/// - If the definition lists **no** `required_env`, the credential of the
///   preferred provider and of every fallback provider is added (a note per
///   variable). An explicit list is the author's statement and is kept as is
///   (a gateway may use other variables).
/// - The `http` interface adds [`HTTP_TOKEN_ENV`].
/// - Without `[models]`, a warning: the runner falls back to its default.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveModels;

impl Pass for ResolveModels {
    fn name(&self) -> &'static str {
        "resolve-models"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        let mut required: BTreeSet<String> =
            unit.ir.permissions.required_env.iter().cloned().collect();
        match unit.ir.models.clone() {
            None => diagnostics.push(Diagnostic::new(
                &codes::NO_MODELS,
                format!("`{}` has no `[models]` table", unit.name),
            )),
            Some(models) if models.required_env.is_empty() => {
                let providers = models.provider.iter().cloned().chain(
                    models
                        .fallbacks
                        .iter()
                        .map(|fallback| fallback.provider.clone()),
                );
                for provider in providers {
                    let Some(env) = provider_env(&provider) else {
                        continue;
                    };
                    if required.insert(env.to_owned()) {
                        diagnostics.push(unit.diagnostic(
                            &codes::PROVIDER_ENV,
                            "models",
                            format!("provider `{provider}` needs `{env}`; added to the manifest"),
                        ));
                    }
                }
            }
            Some(_) => {}
        }
        if unit.ir.binary.interfaces.contains(&Interface::Http)
            && required.insert(HTTP_TOKEN_ENV.to_owned())
        {
            diagnostics.push(unit.diagnostic(
                &codes::PROVIDER_ENV,
                "binary.interfaces",
                format!("the `http` interface needs the bearer token `{HTTP_TOKEN_ENV}`; added to the manifest"),
            ));
        }
        unit.ir.permissions.required_env = required.into_iter().collect();
        diagnostics
    }
}
