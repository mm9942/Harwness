//! `harw-oauth` — Setup-Token-/OAuth-PKCE-Flow und Token-Store für Claude.
//!
//! ## Verantwortung
//! Diese Crate besitzt den **Paste-basierten** PKCE-Setup-Token-Flow gegen
//! Anthropic (Claude), das Ableiten der PKCE-Challenge, den Token-Exchange und
//! das sichere Ablegen des Tokens als 0600-Datei mit `SecretRef`. Sie kennt
//! keine CLI und keinen Terminal-Code — das ist Aufgabe von `harw-cli`.
//!
//! ## Schlüssel-Typen
//! - [`PkcePair`] / [`generate_pkce`] / [`challenge_from_verifier`] — PKCE.
//! - [`authorize_url`] / [`split_callback`] / [`exchange_code`] — Flow.
//! - [`save_token`] — Token-Store (0600) → [`harw_config::SecretRef`].
//! - [`OAuthError`] — handgeschriebener Fehlertyp (kein `anyhow`/`thiserror`).
//!
//! ## Nebenläufigkeit
//! PKCE- und Store-Funktionen sind zustandslos. [`exchange_code`] ist `async`
//! und treibt eine einzelne HTTP-Anfrage.
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

mod error;
mod flow;
mod pkce;
mod store;

pub use error::{OAuthError, OAuthResult};
pub use flow::{authorize_url, client_id, exchange_code, redirect_uri, scopes, split_callback};
pub use pkce::{PkcePair, challenge_from_verifier, generate_pkce};
pub use store::save_token;
