use harw_macros::HarwError;

#[derive(Debug, HarwError)]
pub enum ConfigError {
    #[msg("config file not found: {0}")]
    FileNotFound(String),

    #[msg("failed to parse TOML: {0}")]
    TomlParse(String),

    #[msg("failed to read file '{path}': {reason}")]
    ReadFailed { path: String, reason: String },

    #[msg("agent '{name}' not found in any layer")]
    AgentNotFound { name: String },

    #[msg("provider '{name}' not found")]
    ProviderNotFound { name: String },

    #[msg("invalid config: {0}")]
    Invalid(String),

    #[msg("invalid secret reference '{0}': expected env:/file:/keyring:/secrets: prefix")]
    InvalidSecretRef(String),

    #[msg("duplicate {kind} name '{name}' within a single config layer")]
    DuplicateName { kind: String, name: String },

    #[msg("unresolved {kind} reference '{reference}'")]
    UnresolvedRef { kind: String, reference: String },

    #[msg(
        "plaintext secret found in '{file}' field '{field}' — use env:/file:/keyring:/secrets: instead"
    )]
    PlaintextSecret { file: String, field: String },

    #[msg("channel token '{reference}' is reused by more than one channel binding")]
    DuplicateChannelToken { reference: String },

    #[msg(
        "internal error: key '{key}' was just normalised to {expected} but could not be read back as one"
    )]
    WriterShapeMismatch { key: String, expected: &'static str },

    #[from]
    Io(std::io::Error),
}
