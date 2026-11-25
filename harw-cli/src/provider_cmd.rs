//! `harw provider list|add|remove|enable|disable|scan|catalog|add-known`:
//! Provider des aktiven Profils verwalten.
//!
//! Dünner Adapter: die Verwaltung läuft über denselben Weg wie
//! `harw config provider …`, der Scan über denselben Weg wie
//! `harw model scan`. `catalog` listet die eingebauten bekannten Anbieter
//! (ohne Datei- oder Netzzugriff), `add-known` legt einen davon über
//! `harw_ops::provider::{plan_change, apply_plan}` an; ein Schlüssel kommt
//! nur per Pipe (`--key-stdin`) und wird sofort versiegelt, nie ausgegeben.

use std::io::IsTerminal as _;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Mutex;

use harw_ops::provider::{
    PlanOrigin, ProviderAuthKind, ProviderChange, StagedKeyInfo, StagedKeyRef, StagedKeyShape,
    StagedSecretPort,
};
use harw_secrets::write_only::{KeyShape, MAX_SECRET_LEN, MIN_SECRET_LEN};
use secrecy::SecretString;

use crate::cli::{
    GlobalArgs, ModelsAction, ProviderAction, ProviderAuthKindArg, SettingsAction,
    SettingsProviderAction,
};
use crate::output::{OutputFormat, Printer};

/// Griff des lokal per Pipe gelesenen Schlüssels (nie ausgegeben).
const LOCAL_KEY_HANDLE: &str = "local-stdin";

/// Zweck im Secret-Store für Provider-Schlüssel.
const PROVIDER_SECRET_PURPOSE: &str = "provider authentication";

/// Führt `harw provider …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` verlangt wird (nur
/// Textausgabe) oder die Provider-Verwaltung bzw. der Scan scheitert.
pub fn run(g: &GlobalArgs, a: ProviderAction) -> Result<(), String> {
    let printer = Printer::new(g.output());
    let action = match a {
        ProviderAction::Scan {
            provider,
            free_only,
            prune,
        } => {
            printer.require_text("harw provider scan")?;
            return crate::models::run(
                g,
                Some(ModelsAction::Scan {
                    provider,
                    add: false,
                    free_only,
                    prune,
                }),
            );
        }
        ProviderAction::Catalog => return run_catalog(g.output(), printer),
        ProviderAction::AddKnown {
            catalog_id,
            auth_kind,
            name,
            key_stdin,
        } => return run_add_known(g, printer, catalog_id, auth_kind, name, key_stdin),
        ProviderAction::List => SettingsProviderAction::List,
        ProviderAction::Add {
            name,
            api,
            base_url,
            auth,
            models,
            auth_header,
            no_auth,
            allow_insecure_lan,
        } => SettingsProviderAction::Add {
            name,
            api,
            base_url,
            auth,
            models,
            auth_header,
            no_auth,
            allow_insecure_lan,
        },
        ProviderAction::Remove { name } => SettingsProviderAction::Remove { name },
        ProviderAction::Enable { name } => SettingsProviderAction::Enable { name },
        ProviderAction::Disable { name } => SettingsProviderAction::Disable { name },
    };
    printer.require_text("harw provider")?;
    crate::settings::run(g.home.clone(), Some(SettingsAction::Provider { action }))
}

/// `harw provider catalog`: eingebauter Katalog, kein Datei- oder Netzzugriff.
fn run_catalog(format: OutputFormat, printer: Printer) -> Result<(), String> {
    let providers = harw_ops::provider::known_providers();
    match format {
        OutputFormat::Text => {
            let rows = catalog_rows(&providers);
            printer.table(&["ID", "NAME", "ADRESSE", "ANMELDUNG", "STANDARDNAME"], &rows)
        }
        OutputFormat::Json => printer.value("", serde_json::json!({ "providers": providers })),
    }
}

/// Tabellenzeilen des Katalogs (nur öffentliche Felder, nie Schlüssel).
fn catalog_rows(providers: &[harw_ops::provider::KnownProvider]) -> Vec<Vec<String>> {
    providers
        .iter()
        .map(|provider| {
            let auth_kinds = provider
                .auth_kinds
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let mut default_names = provider.default_name_api_key.clone();
            if let Some(oauth) = &provider.default_name_oauth {
                default_names.push(',');
                default_names.push_str(oauth);
            }
            vec![
                provider.id.clone(),
                provider.display_name.clone(),
                provider.base_url.clone(),
                auth_kinds,
                default_names,
            ]
        })
        .collect()
}

/// `harw provider add-known`: lokal planen und anwenden.
fn run_add_known(
    g: &GlobalArgs,
    printer: Printer,
    catalog_id: String,
    auth_kind: ProviderAuthKindArg,
    name: Option<String>,
    key_stdin: bool,
) -> Result<(), String> {
    let home = crate::home::resolve_home(g.home.clone())?;
    crate::home::ensure_home(&home)?;
    let profile_name = harw_home::active_profile_name(&home);
    let profile_dir =
        harw_home::profile_dir(&home, &profile_name).map_err(|error| error.to_string())?;
    let layers = harw_home::config_layers(&home).map_err(|error| error.to_string())?;
    let config = harw_config::discover_config(&layers).map_err(|error| error.to_string())?;

    let key = if key_stdin {
        Some(read_piped_key()?)
    } else {
        None
    };
    let auth = auth_kind.as_ops();
    let change = ProviderChange::AddKnown {
        catalog_id,
        auth,
        name,
        key: key.is_some().then(|| StagedKeyRef {
            handle: LOCAL_KEY_HANDLE.to_owned(),
        }),
    };
    let port = LocalSecretPort::new(home, config, auth, key);
    let plan = harw_ops::provider::plan_change(
        &port.config,
        &profile_dir,
        change,
        PlanOrigin::Local,
        Some(&port),
    )
    .map_err(|error| error.to_string())?;
    let applied = harw_ops::provider::apply_plan(&profile_dir, &plan, Some(&port), None)
        .map_err(|error| error.to_string())?;

    let mut text = format!("{}: {}", applied.name, applied.note);
    if applied.key_stored {
        text.push_str(" (Schluessel versiegelt)");
    }
    printer.value(
        &text,
        serde_json::json!({
            "effect": "applied",
            "action": applied.action,
            "name": applied.name,
            "key_stored": applied.key_stored,
            "note": applied.note,
        }),
    )
}

/// Liest den Schlüssel begrenzt von einer Pipe; ein Terminal wird abgelehnt
/// (fail closed, keine verdeckte Eingabe ohne neue Crate).
fn read_piped_key() -> Result<SecretString, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(
            "--key-stdin erwartet den Schluessel per Pipe; im Terminal wuerde er sichtbar"
                .to_owned(),
        );
    }
    let limit = u64::try_from(MAX_SECRET_LEN.saturating_add(2)).unwrap_or(u64::MAX);
    let mut buffer = Vec::new();
    let read = stdin.lock().take(limit).read_to_end(&mut buffer);
    let parsed = match read {
        Ok(_) => parse_piped_key(&buffer),
        Err(_) => Err("Schluessel konnte nicht von stdin gelesen werden".to_owned()),
    };
    buffer.fill(0);
    drop(buffer);
    parsed
}

/// Prüft einen per Pipe gelesenen Schlüssel. Fehlertexte enthalten nie den Wert.
///
/// # Errors
/// Kein UTF-8, leer, zu kurz, zu lang oder Zeichen außerhalb `0x21..=0x7E`.
fn parse_piped_key(bytes: &[u8]) -> Result<SecretString, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Schluessel ist kein gueltiges UTF-8".to_owned())?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Schluessel ist leer".to_owned());
    }
    if trimmed.len() < MIN_SECRET_LEN {
        return Err(format!(
            "Schluessel ist zu kurz (mindestens {MIN_SECRET_LEN} Zeichen)"
        ));
    }
    if trimmed.len() > MAX_SECRET_LEN {
        return Err(format!(
            "Schluessel ist zu lang (hoechstens {MAX_SECRET_LEN} Zeichen)"
        ));
    }
    if !trimmed.bytes().all(|byte| (0x21..=0x7E).contains(&byte)) {
        return Err(
            "Schluessel enthaelt unzulaessige Zeichen (nur sichtbares ASCII ohne Leerzeichen)"
                .to_owned(),
        );
    }
    Ok(SecretString::from(trimmed.to_owned()))
}

/// Lokale, einmalige Schlüsselablage für `add-known --key-stdin`.
/// Keine Methode liefert den Wert.
struct LocalSecretPort {
    home: PathBuf,
    config: harw_config::ResolvedConfig,
    auth: ProviderAuthKind,
    value: Mutex<Option<SecretString>>,
}

impl LocalSecretPort {
    fn new(
        home: PathBuf,
        config: harw_config::ResolvedConfig,
        auth: ProviderAuthKind,
        value: Option<SecretString>,
    ) -> Self {
        Self {
            home,
            config,
            auth,
            value: Mutex::new(value),
        }
    }

    /// Name im Secret-Store: `<name>-api-key` bzw. `<name>-oauth-token`.
    fn key_name(&self, for_provider: &str) -> String {
        match self.auth {
            ProviderAuthKind::Oauth => format!("{for_provider}-oauth-token"),
            ProviderAuthKind::ApiKey | ProviderAuthKind::None => {
                format!("{for_provider}-api-key")
            }
        }
    }

    fn check_handle(handle: &str) -> Result<(), String> {
        if handle == LOCAL_KEY_HANDLE {
            Ok(())
        } else {
            Err("unbekannter Schluesselgriff".to_owned())
        }
    }
}

/// Bildet die Form aus harw-secrets auf den Spiegel in harw-ops ab.
const fn map_shape(shape: KeyShape) -> StagedKeyShape {
    match shape {
        KeyShape::AnthropicOAuthToken => StagedKeyShape::AnthropicOAuthToken,
        KeyShape::AnthropicApiKey => StagedKeyShape::AnthropicApiKey,
        KeyShape::OpenAiStyleKey => StagedKeyShape::OpenAiStyleKey,
        KeyShape::Other => StagedKeyShape::Other,
    }
}

impl StagedSecretPort for LocalSecretPort {
    fn describe(&self, handle: &str, for_provider: &str) -> Result<StagedKeyInfo, String> {
        Self::check_handle(handle)?;
        let guard = self
            .value
            .lock()
            .map_err(|_| "interner Zustand".to_owned())?;
        let Some(value) = guard.as_ref() else {
            return Err("Schluessel bereits verbraucht".to_owned());
        };
        let probe = secrecy_08::SecretString::new(
            secrecy::ExposeSecret::expose_secret(value).to_owned(),
        );
        let shape = map_shape(harw_secrets::write_only::classify_key(&probe));
        drop(probe);
        Ok(StagedKeyInfo {
            key_name: self.key_name(for_provider),
            for_provider: for_provider.to_owned(),
            shape,
        })
    }

    fn commit(&self, handle: &str, for_provider: &str) -> Result<harw_config::SecretRef, String> {
        Self::check_handle(handle)?;
        let value = self
            .value
            .lock()
            .map_err(|_| "interner Zustand".to_owned())?
            .take()
            .ok_or_else(|| "Schluessel bereits verbraucht".to_owned())?;
        let mut writer =
            crate::secret_store::open_secret_store_for_writing(&self.home, &self.config)?;
        writer.store(&self.key_name(for_provider), PROVIDER_SECRET_PURPOSE, &value)
    }

    fn rollback(&self, reference: &harw_config::SecretRef) {
        let harw_config::SecretRef::Secrets(raw) = reference else {
            return;
        };
        let Ok(id) = harw_secrets::SecretId::parse(raw) else {
            return;
        };
        let Ok(mut writer) =
            crate::secret_store::open_secret_store_for_writing(&self.home, &self.config)
        else {
            return;
        };
        // Bestes Bemühen: ein Fehler beim Entfernen wird bewusst ignoriert.
        let _ = harw_secrets::write_only::WriteOnlySecretSink::discard(writer.sink(), &id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    const FAKE_KEY: &str = "sk-test-FAKE-0000000000000000";
    const FAKE_OAUTH: &str = "sk-ant-oat01-FAKE-0000000000000000";

    fn port(auth: ProviderAuthKind, value: Option<&str>) -> LocalSecretPort {
        LocalSecretPort::new(
            PathBuf::from("/nonexistent/harw-test-home"),
            harw_config::ResolvedConfig::default(),
            auth,
            value.map(|raw| SecretString::from(raw.to_owned())),
        )
    }

    fn ensure(condition: bool, what: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(TestError::Unexpected(what.to_owned()))
        }
    }

    #[test]
    fn parse_piped_key_trims_newline() -> TestResult {
        let input = format!("{FAKE_KEY}\r\n");
        let parsed = parse_piped_key(input.as_bytes())
            .map_err(TestError::Unexpected)?;
        ensure(
            secrecy::ExposeSecret::expose_secret(&parsed) == FAKE_KEY,
            "Zeilenende sollte entfernt werden",
        )
    }

    #[test]
    fn parse_piped_key_rejects_empty_short_inner_space_non_ascii() -> TestResult {
        let too_long = "a".repeat(MAX_SECRET_LEN + 1);
        let cases: [&[u8]; 6] = [
            b"",
            b"   \n",
            b"short",
            b"sk-test FAKE-0000",
            "sk-test-FAKE-ü000000".as_bytes(),
            too_long.as_bytes(),
        ];
        for case in cases {
            match parse_piped_key(case) {
                Ok(_) => return Err(TestError::Unexpected("Eingabe haette abgelehnt werden sollen".to_owned())),
                Err(message) => ensure(
                    !message.contains("FAKE"),
                    "Fehlertext darf den Wert nicht enthalten",
                )?,
            }
        }
        ensure(parse_piped_key(&[0xFF, 0xFE, 0x41]).is_err(), "kein UTF-8 sollte scheitern")
    }

    #[test]
    fn local_port_describe_maps_shape_and_binds_name() -> TestResult {
        let port = port(ProviderAuthKind::Oauth, Some(FAKE_OAUTH));
        let info = port
            .describe(LOCAL_KEY_HANDLE, "claude-sub")
            .map_err(TestError::Unexpected)?;
        ensure(
            info.shape == StagedKeyShape::AnthropicOAuthToken,
            "Form sollte AnthropicOAuthToken sein",
        )?;
        ensure(info.for_provider == "claude-sub", "Provider-Name unveraendert")?;
        ensure(info.key_name == "claude-sub-oauth-token", "Schluesselname fuer OAuth")?;
        // describe verbraucht nicht.
        let again = port
            .describe(LOCAL_KEY_HANDLE, "claude-sub")
            .map_err(TestError::Unexpected)?;
        ensure(again == info, "describe darf den Wert nicht verbrauchen")?;

        let api = port_api_key_name();
        ensure(api == "anthropic-key-api-key", "Schluesselname fuer API-Key")
    }

    fn port_api_key_name() -> String {
        port(ProviderAuthKind::ApiKey, Some(FAKE_KEY)).key_name("anthropic-key")
    }

    #[test]
    fn local_port_rejects_foreign_handle() -> TestResult {
        let port = port(ProviderAuthKind::ApiKey, Some(FAKE_KEY));
        ensure(port.describe("fremd", "x").is_err(), "describe mit fremdem Griff")?;
        ensure(port.commit("fremd", "x").is_err(), "commit mit fremdem Griff")?;
        // Der Wert bleibt nach abgelehntem commit erhalten.
        ensure(
            port.describe(LOCAL_KEY_HANDLE, "x").is_ok(),
            "Wert darf durch fremden Griff nicht verbraucht werden",
        )
    }

    #[test]
    fn local_port_commit_without_value_fails() -> TestResult {
        let port = port(ProviderAuthKind::ApiKey, None);
        ensure(
            port.describe(LOCAL_KEY_HANDLE, "x").is_err(),
            "describe ohne Wert sollte scheitern",
        )?;
        let result = port.commit(LOCAL_KEY_HANDLE, "x");
        match result {
            Ok(_) => Err(TestError::Unexpected("commit ohne Wert sollte scheitern".to_owned())),
            Err(message) => ensure(message.contains("verbraucht"), "Fehler vor dem Store-Zugriff"),
        }
    }

    #[test]
    fn catalog_rows_have_no_secret_fields() -> TestResult {
        let providers = harw_ops::provider::known_providers();
        let json = serde_json::to_string(&serde_json::json!({ "providers": providers }))
            .map_err(crate::test_support::ctx("Katalog sollte serialisieren"))?;
        ensure(!json.contains("secrets:"), "Katalog-JSON ohne secrets:")?;
        for row in catalog_rows(&providers) {
            ensure(row.len() == 5, "fuenf Spalten je Zeile")?;
            ensure(
                row.iter().all(|cell| !cell.contains("secrets:")),
                "Katalogzeile ohne secrets:",
            )?;
        }
        Ok(())
    }
}
