//! `harw-oauth` — Setup-Token-/OAuth-PKCE-Flow und Token-Store für Claude,
//! sowie ein eigenständiger Refresh-Mechanismus für die ChatGPT-/Codex-Login.
//!
//! ## Verantwortung
//! Diese Crate besitzt den **Paste-basierten** PKCE-Setup-Token-Flow gegen
//! Anthropic (Claude), das Ableiten der PKCE-Challenge, den Token-Exchange und
//! das sichere Ablegen des Tokens als 0600-Datei mit `SecretRef`. Sie kennt
//! keine CLI und keinen Terminal-Code — das ist Aufgabe von `harw-cli`.
//!
//! Zusätzlich übernimmt sie den **proaktiven/reaktiven Refresh** der
//! ChatGPT-/Codex-OAuth-Tokens in `~/.codex/auth.json` (Modul
//! [`codex_refresh`]) — bisher besaß nur die Codex-CLI diesen Flow; `harw`
//! erneuert das kurzlebige Access-Token nun selbst, statt auf ein rechtzeitig
//! erneuertes externes Token zu warten.
//!
//! ## Schlüssel-Typen
//! - [`PkcePair`] / [`generate_pkce`] / [`challenge_from_verifier`] — PKCE.
//! - [`authorize_url`] / [`split_callback`] / [`exchange_code`] — Anthropic-Flow.
//! - [`save_token`] — Token-Store (0600) → [`harw_config::SecretRef`].
//! - [`refresh_codex_tokens`] / [`jwt_needs_refresh`] / [`jwt_exp_unix_seconds`]
//!   — Codex-/ChatGPT-Token-Refresh.
//! - [`OAuthError`] — handgeschriebener Fehlertyp (kein `anyhow`/`thiserror`).
//!
//! ## Nebenläufigkeit
//! PKCE- und Store-Funktionen sind zustandslos. [`exchange_code`] und
//! [`refresh_codex_tokens`] sind `async` und treiben jeweils eine einzelne
//! HTTP-Anfrage; [`refresh_codex_tokens`] serialisiert parallele Refreshes
//! zusätzlich über einen Datei-Lock (siehe [`codex_refresh`]-Moduldokumentation).
//!
//! ## Sicherheit
//! Tokens tragen `secrecy::SecretString` und werden nur beim Schreiben in die
//! 0600-Datei bzw. beim Setzen des Auth-Headers offengelegt — nie geloggt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_oauth::{generate_pkce, authorize_url};
//! let pkce = generate_pkce();
//! let url = authorize_url(&pkce.challenge, "state-123");
//! println!("Öffne im Browser: {url}");
//! ```

#![forbid(unsafe_code)]

mod codex_refresh;
mod error;
mod flow;
mod pkce;
mod store;

pub use codex_refresh::{
    RefreshedCodexTokens, jwt_exp_unix_seconds, jwt_needs_refresh, refresh_codex_tokens,
};
pub use error::{OAuthError, OAuthResult};
pub use flow::{authorize_url, client_id, exchange_code, redirect_uri, scopes, split_callback};
pub use pkce::{PkcePair, challenge_from_verifier, generate_pkce};
pub use store::save_token;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
