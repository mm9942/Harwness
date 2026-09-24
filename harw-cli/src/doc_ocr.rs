//! Installiert `doc.read_pdf`s Mistral-OCR-Backend, sobald ein Laufzeitpfad
//! (`serve`, `chat`/`analyze`, `gateway`) seinen Secret-Resolver geöffnet hat
//! (Design: `docs/design/doc_read_pdf_design.md` §„Anbindung" → W4).
//!
//! # Beschreibung
//! [`install_doc_ocr`] sucht unter den konfigurierten Providern
//! (`config.providers`) über [`find_mistral_provider`] den ersten aktivierten
//! Mistral-Provider — erkannt am `base_url`-Host `api.mistral.ai` oder am
//! Providerschlüssel `mistral` — und löst dessen Credential über
//! [`harw_provider_http::resolve_provider_credential`] auf. Gelingt das,
//! installiert sie den prozessweiten Mistral-OCR-Client der Crate
//! `harw-tool-doc` (`harw_tool_doc::install_mistral_ocr`) mit
//! [`harw_tool_doc::DEFAULT_OCR_MODEL`] und dem Freigabe-Modus aus
//! `[tools.doc].remote_ocr`; `doc.read_pdf` greift danach über
//! `harw_tool_doc::mistral_ocr()` auf ihn zu. Bei `remote_ocr = "off"`
//! installiert sie gar nichts. Ohne passenden Provider, ohne
//! auflösbares Credential oder bei einem Installationsfehler bleibt
//! `doc.read_pdf` beim lokalen `oxidize-pdf`-Pfad — diese Funktion bricht
//! einen Laufzeitpfad nie ab.
//!
//! # Verantwortung
//! Reine Komposition: kein eigener Zustand, keine eigene Fehlerausbreitung.
//! Providerauswahl ist eine reine, unit-getestete Funktion
//! ([`find_mistral_provider`]); Credential-Auflösung und Installation
//! delegieren vollständig an `harw-provider-http`/`harw-tool-doc`.
//!
//! # Nebenläufigkeit
//! Synchron, ohne eigene Threads. `harw_tool_doc::install_mistral_ocr` legt
//! den Client in einem prozessweiten `OnceLock` ab; ruft mehr als ein
//! Laufzeitpfad desselben Prozesses diese Funktion auf (z. B. `gateway`s
//! Telegram- und Dream-Montage), liefert jeder weitere Aufruf
//! `Err(DocToolError::AlreadyConfigured)`, das hier nur als `debug!`
//! protokolliert wird — kein Fehlerpfad für den Aufrufer.
//!
//! # Fehler
//! Diese Datei produziert keinen eigenen Fehlertyp. Jeder Fehlerfall (kein
//! Provider, kein Credential, fehlgeschlagene Installation) wird über
//! `tracing` protokolliert und führt zum lokalen Rückfall, nie zu einem
//! propagierten `Err`.

use std::collections::HashMap;
use std::path::Path;

use harw_config::{ProviderToml, RemoteOcrMode, ResolvedConfig};
use harw_provider_http::SecretResolver;
use harw_tool_doc::{
    DEFAULT_OCR_MODEL, DocToolError, MistralOcrConfig, RemoteOcrApproval, install_mistral_ocr,
};

/// Host des offiziellen Mistral-API-Endpunkts, gegen den `base_url` geprüft wird.
const MISTRAL_API_HOST: &str = "api.mistral.ai";

/// Providerschlüssel, der einen Provider unabhängig vom Host als Mistral
/// erkennt (Kleinschreibung unbeachtet).
const MISTRAL_PROVIDER_NAME: &str = "mistral";

/// Installiert das Mistral-OCR-Backend für `doc.read_pdf`, falls
/// `[tools.doc].remote_ocr` nicht `off` ist, ein aktivierter
/// Mistral-Provider konfiguriert und sein Credential auflösbar ist.
///
/// # Beschreibung
/// Sucht über [`find_mistral_provider`] den ersten passenden Provider in
/// `config.providers`, löst dessen Credential über
/// [`harw_provider_http::resolve_provider_credential`] auf — mit
/// `config.env_layer`, `home` und `resolver` als Quellen, demselben Vertrag,
/// den auch `build_provider_with_home`/`build_provider_with_resolver` für den
/// eigentlichen Modell-Provider nutzen — und installiert bei Erfolg den
/// prozessweiten Mistral-OCR-Client mit [`harw_tool_doc::DEFAULT_OCR_MODEL`].
/// Jeder Rufer übergibt denselben Secret-Resolver, den er für den eigentlichen
/// Modell-Provider bereits geöffnet hat.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): aufgelöste Konfiguration des Laufs;
///   liefert `providers` und `env_layer`.
/// - `home` (`Option<&Path>`): aufgelöster Root-Space, wie ihn
///   `resolve_provider_credential` für `file:`/`file-json:`-Credentials
///   braucht; `None`, wenn kein Home vorliegt (z. B. `--config-dir`).
/// - `resolver` (`Option<&dyn SecretResolver>`): bereits geöffneter
///   `secrets:`-Resolver desselben Laufs, oder `None`, wenn dieser Lauf
///   keinen benötigt.
///
/// # Returns
/// `()`. Das Ergebnis ist ausschließlich in `tracing`-Ereignissen sichtbar:
/// `info!` bei erfolgreicher Installation, `debug!`/`warn!` bei jedem
/// Rückfall auf die lokale Extraktion.
///
/// # Errors
/// Keine — jeder Fehlschlag (kein Mistral-Provider, kein auflösbares
/// Credential, `install_mistral_ocr`-Fehler inklusive
/// `DocToolError::AlreadyConfigured`) wird protokolliert und niemals an den
/// Aufrufer zurückgegeben.
///
/// # Concurrency
/// Synchron. Sicher aus mehreren Laufzeitpfaden desselben Prozesses
/// aufzurufen: ein zweiter Aufruf trifft auf `harw_tool_doc`s prozessweites
/// `OnceLock` und wird als `AlreadyConfigured` nur protokolliert, niemals als
/// Fehler zurückgegeben.
///
/// # Examples
/// ```rust,ignore
/// // Direkt nachdem `serve_mcp` seinen Secret-Resolver geöffnet hat:
/// crate::doc_ocr::install_doc_ocr(
///     &config,
///     home.as_deref(),
///     secret_resolver
///         .as_ref()
///         .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver),
/// );
/// ```
pub fn install_doc_ocr(
    config: &ResolvedConfig,
    home: Option<&Path>,
    resolver: Option<&dyn SecretResolver>,
) {
    let Some(approval) = remote_ocr_approval(config.harness.tools.doc.remote_ocr) else {
        tracing::info!("doc.read_pdf: remote OCR disabled by config, nutzt lokale Extraktion");
        return;
    };
    let Some((provider_id, provider)) = find_mistral_provider(&config.providers) else {
        tracing::debug!(
            "doc.read_pdf: kein aktivierter Mistral-Provider konfiguriert, nutzt lokale Extraktion"
        );
        return;
    };

    let api_key = match harw_provider_http::resolve_provider_credential(
        provider,
        &config.env_layer,
        home,
        resolver,
    ) {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            tracing::debug!(
                provider = provider_id,
                "doc.read_pdf: Mistral-Provider hat kein auflösbares Credential, nutzt lokale Extraktion"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(
                provider = provider_id,
                error = %error,
                "doc.read_pdf: Mistral-Credential konnte nicht aufgelöst werden, nutzt lokale Extraktion"
            );
            return;
        }
    };

    let ocr_config = MistralOcrConfig {
        base_url: provider.base_url.clone(),
        api_key,
        model: DEFAULT_OCR_MODEL.to_owned(),
    };

    match install_mistral_ocr(ocr_config, approval) {
        Ok(()) => {
            tracing::info!(
                provider = provider_id,
                approval = ?approval,
                "doc.read_pdf nutzt Mistral OCR"
            );
        }
        Err(DocToolError::AlreadyConfigured) => {
            tracing::debug!(
                provider = provider_id,
                "doc.read_pdf: Mistral-OCR-Client bereits installiert, zweiter Aufruf ignoriert"
            );
        }
        Err(error) => {
            tracing::warn!(
                provider = provider_id,
                error = %error,
                "doc.read_pdf: Mistral-OCR-Client konnte nicht installiert werden, nutzt lokale Extraktion"
            );
        }
    }
}

// Übersetzt `[tools.doc].remote_ocr` in den Freigabe-Modus des
// OCR-Clients; `None` bei `off` (dann wird kein Client installiert).
fn remote_ocr_approval(mode: RemoteOcrMode) -> Option<RemoteOcrApproval> {
    match mode {
        RemoteOcrMode::Off => None,
        RemoteOcrMode::Ask => Some(RemoteOcrApproval::Ask),
        RemoteOcrMode::On => Some(RemoteOcrApproval::Auto),
    }
}

// Wählt den ersten aktivierten Provider aus `providers`, der Mistral OCR
// anbietet: Providerschlüssel `mistral` (Kleinschreibung unbeachtet) oder
// `base_url`-Host `api.mistral.ai`. "Erster" meint alphabetisch nach
// Schlüssel, da `HashMap`-Iteration keine stabile Reihenfolge liefert.
fn find_mistral_provider(
    providers: &HashMap<String, ProviderToml>,
) -> Option<(&str, &ProviderToml)> {
    let mut candidates: Vec<(&str, &ProviderToml)> = providers
        .iter()
        .filter(|(_, provider)| provider.enabled)
        .filter(|(id, provider)| provider_is_mistral(id, provider))
        .map(|(id, provider)| (id.as_str(), provider))
        .collect();
    candidates.sort_by_key(|(id, _)| *id);
    candidates.into_iter().next()
}

// `true`, wenn `provider` als Mistral-OCR-Anbieter gilt (siehe
// `find_mistral_provider`).
fn provider_is_mistral(id: &str, provider: &ProviderToml) -> bool {
    id.eq_ignore_ascii_case(MISTRAL_PROVIDER_NAME)
        || provider_host(&provider.base_url)
            .is_some_and(|host| host.eq_ignore_ascii_case(MISTRAL_API_HOST))
}

// Extrahiert den Host aus einer `base_url`; `None` bei ungültiger URL oder
// einer URL ohne Host-Komponente.
fn provider_host(base_url: &str) -> Option<String> {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use harw_config::{OriginAllowlistToml, ProviderToml, SecretRef};

    use harw_config::{RemoteOcrMode, ResolvedConfig};
    use harw_tool_doc::RemoteOcrApproval;

    use super::{find_mistral_provider, install_doc_ocr, remote_ocr_approval};
    use crate::test_support::{TestError, TestResult};

    // Minimaler, vollständiger `ProviderToml`-Testwert; nur `base_url` und
    // `enabled` variieren je Test.
    fn test_provider(base_url: &str, enabled: bool) -> ProviderToml {
        ProviderToml {
            stream: None,
            name: "test".to_owned(),
            api: "openai-compatible".to_owned(),
            base_url: base_url.to_owned(),
            auth: Some(SecretRef::Env("TEST_API_KEY".to_owned())),
            auth_header: None,
            api_key: None,
            originator: None,
            headers: HashMap::new(),
            models: Vec::new(),
            enabled,
            origin_allowlist: OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        }
    }

    #[test]
    fn find_mistral_provider_matches_by_host() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert(
            "openai_compatible".to_owned(),
            test_provider("https://api.mistral.ai/v1", true),
        );

        let Some((id, _provider)) = find_mistral_provider(&providers) else {
            return Err(TestError::Missing("Mistral-Provider per Host"));
        };
        assert_eq!(id, "openai_compatible");
        Ok(())
    }

    #[test]
    fn find_mistral_provider_matches_by_name() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert(
            "mistral".to_owned(),
            test_provider("https://example.internal/v1", true),
        );

        let Some((id, _provider)) = find_mistral_provider(&providers) else {
            return Err(TestError::Missing("Mistral-Provider per Name"));
        };
        assert_eq!(id, "mistral");
        Ok(())
    }

    #[test]
    fn find_mistral_provider_ignores_disabled_provider() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert(
            "mistral".to_owned(),
            test_provider("https://api.mistral.ai/v1", false),
        );

        assert!(find_mistral_provider(&providers).is_none());
        Ok(())
    }

    #[test]
    fn find_mistral_provider_returns_none_without_a_match() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_owned(),
            test_provider("https://api.openai.com/v1", true),
        );

        assert!(find_mistral_provider(&providers).is_none());
        Ok(())
    }

    #[test]
    fn find_mistral_provider_prefers_alphabetically_first_key() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert(
            "zeta-mistral".to_owned(),
            test_provider("https://api.mistral.ai/v1", true),
        );
        providers.insert(
            "alpha-mistral".to_owned(),
            test_provider("https://api.mistral.ai/v1", true),
        );

        let Some((id, _provider)) = find_mistral_provider(&providers) else {
            return Err(TestError::Missing("Mistral-Provider"));
        };
        assert_eq!(id, "alpha-mistral");
        Ok(())
    }

    #[test]
    fn find_mistral_provider_ignores_an_unparsable_base_url() -> TestResult {
        let mut providers = HashMap::new();
        providers.insert("broken".to_owned(), test_provider("not a url", true));

        assert!(find_mistral_provider(&providers).is_none());
        Ok(())
    }

    #[test]
    fn remote_ocr_approval_maps_config_modes() {
        assert_eq!(remote_ocr_approval(RemoteOcrMode::Off), None);
        assert_eq!(
            remote_ocr_approval(RemoteOcrMode::Ask),
            Some(RemoteOcrApproval::Ask)
        );
        assert_eq!(
            remote_ocr_approval(RemoteOcrMode::On),
            Some(RemoteOcrApproval::Auto)
        );
    }

    // `remote_ocr = "off"` installiert keinen Client, obwohl ein Mistral-
    // Provider mit auflösbarem Credential konfiguriert ist. Kein anderer
    // Test dieser Crate installiert einen OCR-Client, der prozessweite
    // `OnceLock` bleibt darum leer.
    #[test]
    fn install_doc_ocr_off_installs_no_client() {
        let mut config = ResolvedConfig::default();
        config.harness.tools.doc.remote_ocr = RemoteOcrMode::Off;
        config.providers.insert(
            "mistral".to_owned(),
            test_provider("https://api.mistral.ai/v1", true),
        );
        config
            .env_layer
            .insert("TEST_API_KEY".to_owned(), "sk-test".to_owned());

        install_doc_ocr(&config, None, None);

        assert!(harw_tool_doc::remote_ocr_target().is_none());
        assert!(harw_tool_doc::mistral::mistral_ocr().is_none());
    }
}
