//! Handgeschriebener Fehlertyp der OAuth-/Setup-Token-Schicht.
//!
//! Kein `anyhow`/`thiserror`. Jede Variante trägt genug Kontext, um die Ursache
//! ohne Blick in den Quellcode zu verstehen.

use std::fmt;

/// Bequemer Ergebnistyp der OAuth-Schicht.
pub type OAuthResult<T> = Result<T, OAuthError>;

/// Fehler beim Setup-Token-/OAuth-Flow, Token-Store und Codex-Token-Refresh.
pub enum OAuthError {
    /// Der Token-Exchange-Endpoint antwortete nicht-erfolgreich.
    TokenExchange {
        /// HTTP-Statuscode.
        status: u16,
        /// Roher Antwort-Body (für Diagnose; kann Fehlermeldung enthalten).
        body: String,
    },
    /// HTTP-Transportfehler (Netzwerk, TLS, …).
    Http(String),
    /// Eine erwartete Antwort-Struktur/-Feld fehlte.
    MissingField(&'static str),
    /// Die vom Nutzer eingegebene Callback-Eingabe war unbrauchbar.
    MalformedInput(String),
    /// I/O-Fehler beim Schreiben/Lesen der Token-Datei.
    TokenStoreIo(std::io::Error),
    /// Der Providername ist kein sicherer Bestandteil des Token-Dateinamens.
    ///
    /// Die fehlerhafte Eingabe wird absichtlich nicht mitgeführt, damit weder
    /// sensible Providerwerte noch Tokenmaterial in Fehlermeldungen gelangen.
    UnsafeProviderFilenameComponent,
    /// Ein aufgelöster Wert konnte nicht als `SecretRef` geparst werden.
    SecretRef(String),
    /// Die Codex-Credential-Datei (`~/.codex/auth.json`) ließ sich nicht als
    /// erwartetes JSON-Dokument lesen oder enthielt kein `tokens`-Objekt.
    CodexCredentialFile(String),
    /// Der Datei-Lock für den Codex-Token-Refresh konnte innerhalb des
    /// Zeitlimits nicht erworben werden (paralleler Refresh eines anderen
    /// Prozesses/Threads hält ihn vermutlich noch).
    RefreshLockTimeout(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenExchange { status, body } => {
                write!(f, "OAuth token exchange failed (HTTP {status}): {body}")
            }
            Self::Http(reason) => write!(f, "OAuth HTTP transport failed: {reason}"),
            Self::MissingField(field) => {
                write!(f, "OAuth response missing required field: {field}")
            }
            Self::MalformedInput(reason) => write!(f, "malformed callback input: {reason}"),
            Self::TokenStoreIo(error) => write!(f, "token store I/O failed: {error}"),
            Self::UnsafeProviderFilenameComponent => {
                write!(
                    f,
                    "OAuth provider name cannot be used in the token filename"
                )
            }
            Self::SecretRef(reason) => write!(f, "could not build secret reference: {reason}"),
            Self::CodexCredentialFile(reason) => {
                write!(f, "Codex credential file is unusable: {reason}")
            }
            Self::RefreshLockTimeout(path) => {
                write!(f, "timed out waiting for Codex refresh lock at {path}")
            }
        }
    }
}

impl fmt::Debug for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for OAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TokenStoreIo(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for OAuthError {
    fn from(error: std::io::Error) -> Self {
        Self::TokenStoreIo(error)
    }
}

impl From<reqwest::Error> for OAuthError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.to_string())
    }
}
