//! Fehlertypen für `harw-extension-api`.
//!
//! `ExtensionResult<T>` wird vom `HarwError`-Derive miterzeugt.

use harw_macros::HarwError;

#[derive(Debug, HarwError)]
pub enum ExtensionError {
    #[msg("extension failed: {0}")]
    Failed(String),

    #[msg("provider '{name}' not found")]
    ProviderNotFound { name: String },
}
