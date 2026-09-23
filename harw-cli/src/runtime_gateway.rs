//! Runtime-Montage der Gateway-Einstiege (`harw gateway`: Telegram, Dream).
//!
//! # Beschreibung
//! Vertrag: `docs/remediation/CONTRACTS.md` §runtime-spec
//! (`EntryKind::GatewayTelegram`/`GatewayDream`, beide `{}`-Rechte,
//! `RegistryProfile::NoTools`, `AskResolution::Fail`, `SpawnerPolicy::None`,
//! `CeilingPolicy::Closed`). Dieses Modul baut aus einem Kanal-Ereignis genau
//! einen [`harw_runtime::RuntimeAssembly`]:
//!
//! 1. [`channel_principal`] leitet den vertrauenswürdigen [`Principal`] an
//!    der Eingangsgrenze ab (`Principal::trusted_ingress`, nie aus
//!    Modell-/Chat-Text).
//! 2. [`GatewayEntry::entry_kind`] wählt die passende [`EntryKind`]-Variante.
//! 3. [`gateway_assembly`] baut über `crate::runtime_entry::runtime_spec` die
//!    Eingangsbeschreibung und montiert danach **direkt** über
//!    [`harw_runtime::RuntimeAssembly::builder`] (statt über
//!    `crate::runtime_entry::build_assembly`) — mit `ModelSource::Configured`,
//!    ohne Job-/Freigabespeicher (Gateways kennen keine durablen Jobs/Freigaben),
//!    ohne Sitzungsereignis-Kanal (kein Spawner in `Closed`) und, wenn der
//!    Aufrufer einen übergibt, mit einem `secret_resolver` für versiegelte
//!    `secrets:`-Provider-Credentials.
//!
//! # Secret-Resolver für `ModelSource::Configured` (Regression aus B3)
//! `crate::runtime_entry::build_assembly` kennt keinen `secret_resolver`-Pfad:
//! eine über diese Hilfsfunktion montierte Gateway-Assembly konnte deshalb
//! `secrets:`-Referenzen (versiegelter Store) nicht auflösen, obwohl
//! `env:`/Klartext-Referenzen weiter funktionierten. [`gateway_assembly`]
//! umgeht `build_assembly` deshalb bewusst und ruft
//! `RuntimeAssembly::builder(spec).model(ModelSource::Configured).stores(stores)`
//! direkt auf, hängt bei `Some(resolver)` zusätzlich
//! [`harw_runtime::RuntimeAssemblyBuilder::secret_resolver`] an und baut dann.
//! Der Aufrufer (`crate::gateway::mount_gateway_assembly`) öffnet den Resolver
//! genau einmal vorab über eine vorläufig geladene Konfiguration
//! (`crate::gateway::open_gateway_secret_resolver` →
//! `crate::secret_store::open_configured_secret_resolver`) und teilt ihn per
//! `Arc::clone` zwischen der Dream-Montage und allen Telegram-Montagen.
//!
//! # Montagen je Gateway-Start (Befunde G1/G3)
//! `crate::gateway::mount_gateway_assembly` baut für jeden Daemon-Start genau
//! eine Montage für [`GatewayEntry::Dream`] und zusätzlich **je aktivierter
//! Telegram-Bindung** eine eigene Montage für [`GatewayEntry::Telegram`]
//! (null Bindungen → keine Telegram-Montage). Das Modell der Dream-Montage
//! speist den Dream-Scheduler, sodass Audit und Trace Telegram-Turns und
//! Dream-Läufe über ihre [`EntryKind`] unterscheiden; mehrere Telegram-Bots
//! unterscheiden sich zusätzlich über ihren bindungsbezogenen [`Principal`]
//! (siehe [`channel_principal`]).
//!
//! # Kein stiller Fallback (Befund G-048)
//! Die frühere Montage (vor W2d-1) bildete einen gescheiterten
//! Provider-Aufbau auf `()` ab und fiel lautlos auf
//! [`harw_core::EchoModelProvider`] zurück — Telegram-Antworten und
//! Dream-Berichte liefen dann unbemerkt mit Echo-Inhalt weiter
//! (`docs/remediation/ledger/W2b/W2B-01.md` beschreibt denselben
//! Provider-Fehlerpfad als `RuntimeError::Provider`, dessen `Display`-Text
//! [`gateway_assembly`] hier durchreicht). Dieses Modul tut das bewusst
//! **nicht**: jeder Fehler aus `RuntimeAssemblyBuilder::build` wird
//! unverändert mit dem Präfix `"gateway: "` als `Err` zurückgegeben; es gibt
//! keinen Echo-Zweig.
//!
//! **Gateway-eigenes Vorab-Gate, unabhängig von `harw-runtime`s
//! Kulanz-Fallback:** `harw_runtime::model::build_root_model_with_resolver`
//! (Aufbau-interner Schritt von `RuntimeAssemblyBuilder::build`) startet für
//! interaktive Einstiege (CLI/TUI) inzwischen auch **ohne** nutzbaren
//! Provider erfolgreich — der Fehler erscheint dort erst beim ersten
//! tatsächlichen Modell-Aufruf, nicht beim Bau (`DefaultModelResolution::
//! NoneUsable` → `UnusableModelProvider`, siehe `harw-runtime/src/model.rs`).
//! Für einen unbeaufsichtigten Hintergrund-Daemon ohne Bedienoberfläche wäre
//! das kein sichtbarer Fehler, sondern ein Daemon, der lautlos akzeptiert,
//! aber nie antwortet — das verletzt G-048 in der Sache, auch ohne Echo-Text.
//! [`gateway_assembly`] lädt deshalb die Konfiguration ein zweites Mal (nach
//! demselben, bereits an anderer Stelle dieses Moduls etablierten Muster
//! wiederholten, aber günstigen Lesens einer TOML-Datei) und bricht selbst
//! mit `Err` ab, wenn [`config_has_usable_provider`] keinen vorhandenen,
//! **aktivierten** Provider findet, bevor `RuntimeAssemblyBuilder::build`
//! überhaupt aufgerufen wird. Ein vom Scaffolding vorgesäter, aber
//! deaktivierter Katalog-Provider (`harw-model-catalog::seed_profile_
//! providers`, `enabled = false`) zählt dabei ausdrücklich **nicht** als
//! vorhandene Konfiguration.
//!
//! # Nebenläufigkeit
//! Rein synchron und seiteneffektfrei bis auf das, was der Bau selbst tut
//! (Konfiguration lesen, Projekt erkennen); kein globaler Zustand.

use std::path::Path;
use std::sync::Arc;

use harw_core::StateStore;
use harw_provider_http::SecretResolver;
use harw_runtime::{EntryKind, ModelSource, RuntimeAssembly, RuntimeStores};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

/// Welcher Gateway-Einstieg eine Montage anfordert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GatewayEntry {
    /// Telegram-Kanal (`harw-cli/src/gateway.rs` Telegram-Zweig).
    Telegram,
    /// Dream-Läufe des Gateways; eigene Montage in
    /// `crate::gateway::mount_gateway_assembly`, deren Modell an den
    /// Dream-Scheduler geht.
    Dream,
}

impl GatewayEntry {
    /// Bildet den Gateway-Einstieg auf seine [`EntryKind`] ab.
    ///
    /// # Beschreibung
    /// `Telegram` → [`EntryKind::GatewayTelegram`], `Dream` →
    /// [`EntryKind::GatewayDream`] (`docs/remediation/CONTRACTS.md`
    /// §runtime-spec). Beide reduzieren über [`EntryKind::profile`] auf
    /// identische Rechte; die Varianten bleiben getrennt, damit Diagnose und
    /// Audit den auslösenden Kanal unterscheiden können.
    #[must_use]
    pub(crate) fn entry_kind(self) -> EntryKind {
        match self {
            GatewayEntry::Telegram => EntryKind::GatewayTelegram,
            GatewayEntry::Dream => EntryKind::GatewayDream,
        }
    }
}

/// Leitet den vertrauenswürdigen [`Principal`] eines Gateway-Kanals ab.
///
/// # Arguments
/// - `entry` ([`GatewayEntry`]): welcher Kanal.
/// - `peer` (`&str`): für `Telegram` die Kennung der konfigurierten
///   Bot-Bindung ohne `telegram:`-Präfix; für `Dream` ungenutzt, weil
///   Dream-Läufe keinen externen Peer haben (der Gateway übergibt `""`).
///
/// # Bindungsbezogener Telegram-Principal (Befund G6)
/// Die Telegram-Montagen des Daemons (`crate::gateway::mount_gateway_assembly`)
/// rufen diese Funktion je aktivierter Bindung mit der Bindungs-ID ohne
/// führendes `telegram:` auf (`crate::gateway::telegram_principal_peer`); der
/// Principal heißt damit `telegram:<bindung>` (z. B. `telegram:ops` für die
/// Bindung `telegram:ops`). Er stammt aus der vertrauenswürdig
/// **konfigurierten** Bindung, nie aus Chat- oder Modelltext, und bezeichnet
/// den Bot-Kanal, nicht den einzelnen Absender — der handelnde Mensch wird
/// pro Ereignis über `SessionKey`/`SenderRef` an der Admission-Grenze
/// getragen. Die Rechte bleiben unverändert `Observer` mit `{}`-Rechten.
///
/// # Beschreibung
/// `Telegram` → `Principal::trusted_ingress(PrincipalKind::Channel,
/// "telegram:<peer>", IngressSurface::Telegram, PermissionTier::Observer)`.
/// `Dream` → `Principal::trusted_ingress(PrincipalKind::Operation,
/// "gateway-dream", IngressSurface::Gateway, PermissionTier::Observer)`.
/// Beide Stufen sind `Observer`: kein Gateway-Kanal darf zustandsverändernde
/// Operationen ohne weitere Freigabe auslösen (`EntryKind::profile` liefert
/// dazu ohnehin `{}`-Rechte und `RegistryProfile::NoTools`).
#[must_use]
pub(crate) fn channel_principal(entry: GatewayEntry, peer: &str) -> Principal {
    match entry {
        GatewayEntry::Telegram => Principal::trusted_ingress(
            PrincipalKind::Channel,
            format!("telegram:{peer}"),
            IngressSurface::Telegram,
            PermissionTier::Observer,
        ),
        GatewayEntry::Dream => Principal::trusted_ingress(
            PrincipalKind::Operation,
            "gateway-dream",
            IngressSurface::Gateway,
            PermissionTier::Observer,
        ),
    }
}

/// Baut die Runtime-Montage eines Gateway-Turns.
///
/// # Arguments
/// - `entry` ([`GatewayEntry`]): Telegram oder Dream; bestimmt über
///   [`GatewayEntry::entry_kind`] die [`EntryKind`].
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw` bzw. `HARW_HOME`).
/// - `cwd` (`&Path`): Arbeitsverzeichnis des Laufs.
/// - `principal` ([`Principal`]): aus [`channel_principal`] an der
///   Eingangsgrenze abgeleitet.
/// - `state_store` (`Arc<dyn StateStore>`): Verlaufsspeicher der Sitzung
///   (Pflichtfeld von [`RuntimeStores`]).
/// - `secret_resolver` (`Option<Arc<dyn SecretResolver + Send + Sync>>`):
///   Auflöser für versiegelte `secrets:`-Provider-Credentials, geöffnet vom
///   Aufrufer über `crate::secret_store::open_configured_secret_resolver`
///   (im Gateway einmal je Start, geteilt zwischen Telegram- und
///   Dream-Montage).
///   `None`, wenn kein aktivierter Provider eine `secrets:`-Referenz nutzt
///   (oder kein KEK konfiguriert ist) — `ModelSource::Configured` löst dann
///   nur `env:`/Klartext-Referenzen auf.
///
/// # Beschreibung
/// Reicht `entry`, `home`, `cwd`, `principal` unverändert an
/// `crate::runtime_entry::runtime_spec` weiter und montiert danach **direkt**
/// über [`RuntimeAssembly::builder`] — nicht über
/// `crate::runtime_entry::build_assembly`, das keinen `secret_resolver`-Pfad
/// kennt (Regression B3, siehe Moduldoku): `.model(ModelSource::Configured)`
/// (Gateways haben keinen abweichenden Modellpfad), `.stores(RuntimeStores {
/// state_store, job_store: None, approval_store: None })` — Gateways kennen
/// weder durable Jobs noch durable Freigaben — und bei `Some(resolver)`
/// zusätzlich `.secret_resolver(resolver)`. Der Sitzungsereignis-Kanal bleibt
/// ungesetzt: `GatewayTelegram`/`GatewayDream` reduzieren auf
/// `SpawnerPolicy::None`, ein Spawner-Kanal wäre also ungenutzt.
///
/// # Errors
/// Jeder Fehler aus `RuntimeAssemblyBuilder::build` (Konfiguration,
/// Vertrauen, Projekterkennung, Sandbox, Registry, **Provider**, Speicher)
/// wird unverändert mit dem Präfix `"gateway: "` zurückgegeben. Es gibt
/// **keinen** Echo-Fallback (Befund G-048): ein nicht baubarer Provider lässt
/// den Gateway-Turn fehlschlagen, statt unbemerkt mit Echo-Antworten
/// weiterzulaufen. Zusätzlich — noch bevor `RuntimeAssemblyBuilder::build`
/// aufgerufen wird — bricht diese Funktion mit `Err("gateway: no usable
/// model/provider is configured; …")` ab, wenn [`config_has_usable_provider`]
/// keinen vorhandenen, aktivierten Provider findet (siehe Moduldoku,
/// Abschnitt „Gateway-eigenes Vorab-Gate"): `harw-runtime` selbst lässt
/// `ModelSource::Configured` in diesem Fall inzwischen erfolgreich, aber mit
/// einem nur bei tatsächlicher Nutzung fehlschlagenden Platzhalter-Modell
/// bauen — für einen unbeaufsichtigten Daemon ohne Bedienoberfläche ist das
/// kein sichtbarer Start-Fehler.
pub(crate) fn gateway_assembly(
    entry: GatewayEntry,
    home: &Path,
    cwd: &Path,
    principal: Principal,
    state_store: Arc<dyn StateStore>,
    secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,
) -> Result<RuntimeAssembly, String> {
    let spec = crate::runtime_entry::runtime_spec(entry.entry_kind(), home, cwd, principal);

    // Vorab-Gate (siehe Moduldoku und diese Funktions-Doku, Abschnitt
    // „Gateway-eigenes Vorab-Gate"): dieselbe Konfiguration, die
    // `RuntimeAssemblyBuilder::build` gleich intern erneut lädt, wird hier
    // vorab gelesen, ausschließlich um zu prüfen, ob überhaupt ein nutzbarer
    // Provider referenziert wird — bevor irgendetwas sonst gebaut wird.
    let (preliminary_config, _preliminary_trust) =
        harw_runtime::load_config(&spec).map_err(|error| format!("gateway: {error}"))?;
    if !config_has_usable_provider(&preliminary_config) {
        return Err(
            "gateway: no usable model/provider is configured; refusing to mount a gateway \
             runtime instead of starting with a placeholder model (G-048)"
                .to_owned(),
        );
    }

    let stores = RuntimeStores {
        state_store,
        job_store: None,
        approval_store: None,
    };
    let mut builder = RuntimeAssembly::builder(spec)
        .model(ModelSource::Configured)
        .stores(stores);
    if let Some(resolver) = secret_resolver {
        builder = builder.secret_resolver(resolver);
    }
    builder.build().map_err(|error| format!("gateway: {error}"))
}

/// `true`, wenn `config` mindestens einen tatsächlich nutzbaren Provider
/// ausweist — entweder über `default_provider`/`default_model`, oder über ein
/// katalogisiertes Modell (`config.models`), dessen Provider vorhanden
/// **und** aktiviert ist.
///
/// # Description
/// Spiegelt bewusst dieselbe „nutzbar"-Definition wie
/// `harw_runtime::model::resolve_default_model`/`provider_is_usable`
/// (`harw-runtime/src/model.rs`, nicht Teil dieses Schreibbereichs): ein in
/// `config.providers` vorhandener, aber `enabled = false` gesetzter Provider
/// zählt **nicht** als konfiguriert — insbesondere ein vom Scaffolding
/// vorgesäter Katalog-Provider (`harw-model-catalog::seed_profile_
/// providers`), der bis zum Onboarding oder `harw settings provider enable
/// <id>` immer deaktiviert bleibt.
///
/// # Arguments
/// - `config` (`&harw_config::ResolvedConfig`): die vorab geladene
///   Konfiguration desselben Laufs (siehe [`gateway_assembly`]).
///
/// # Returns
/// `true`, sobald irgendein Pfad zu einem vorhandenen, aktivierten Provider
/// führt; sonst `false`.
fn config_has_usable_provider(config: &harw_config::ResolvedConfig) -> bool {
    let provider_is_usable = |provider_id: &str| -> bool {
        config
            .providers
            .get(provider_id)
            .is_some_and(|provider| provider.enabled)
    };
    let default_ok = config.harness.default_model.is_some()
        && config
            .harness
            .default_provider
            .as_deref()
            .is_some_and(provider_is_usable);
    if default_ok {
        return true;
    }
    config
        .models
        .values()
        .any(|model| provider_is_usable(&model.provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::InMemoryStateStore;

    #[test]
    fn test_gateway_entry_kind_maps_both_variants() {
        assert_eq!(
            GatewayEntry::Telegram.entry_kind(),
            EntryKind::GatewayTelegram
        );
        assert_eq!(GatewayEntry::Dream.entry_kind(), EntryKind::GatewayDream);
    }

    #[test]
    fn test_channel_principal_telegram_prefixes_peer_and_is_observer() {
        let principal = channel_principal(GatewayEntry::Telegram, "12345");
        assert_eq!(principal.kind(), PrincipalKind::Channel);
        assert_eq!(principal.id(), "telegram:12345");
        assert_eq!(principal.surface(), IngressSurface::Telegram);
        assert_eq!(principal.tier(), PermissionTier::Observer);
    }

    #[test]
    fn test_channel_principal_dream_is_operation_gateway() {
        let principal = channel_principal(GatewayEntry::Dream, "unused-peer");
        assert_eq!(principal.kind(), PrincipalKind::Operation);
        assert_eq!(principal.id(), "gateway-dream");
        assert_eq!(principal.surface(), IngressSurface::Gateway);
        assert_eq!(principal.tier(), PermissionTier::Observer);
    }

    /// Leeres Tempdir-Home ohne Provider-Konfiguration muss `Err` mit dem
    /// Präfix `"gateway: "` liefern — nie still auf Echo zurückfallen
    /// (Befund G-048).
    #[test]
    fn test_gateway_assembly_without_provider_config_returns_err_not_echo() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("tempdir cwd"))?;
        let principal = channel_principal(GatewayEntry::Dream, "unused-peer");
        let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

        let result = gateway_assembly(
            GatewayEntry::Dream,
            home.path(),
            cwd.path(),
            principal,
            state_store,
            None,
        );

        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a tempdir home without provider config must not build an assembly".into(),
            ));
        };
        assert!(
            error.starts_with("gateway: "),
            "error must carry the \"gateway: \" prefix, got: {error}"
        );
        Ok(())
    }

    /// Ein Home mit genau einem, aber **deaktivierten** Provider — dieselbe
    /// Form, die `harw-model-catalog::seed_profile_providers` seit dem
    /// Scaffolding-Bundle für jeden Katalog-Eintrag vorsät (`enabled =
    /// false`, kein Schlüssel) — muss ebenso `Err` liefern wie ein Home ganz
    /// ohne Provider-Datei: ein deaktivierter Provider darf nie als
    /// „vorhandene Konfiguration" zählen.
    #[test]
    fn test_gateway_assembly_with_only_a_disabled_provider_returns_err_not_echo() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("tempdir cwd"))?;
        std::fs::write(
            home.path().join("config.toml"),
            "default_provider = \"seeded\"\ndefault_model = \"seeded-model\"\n",
        )
        .map_err(ctx("write config.toml"))?;
        std::fs::create_dir_all(home.path().join("providers"))
            .map_err(ctx("create providers dir"))?;
        std::fs::write(
            home.path().join("providers").join("seeded.toml"),
            "name = \"seeded\"\napi = \"openai-chat\"\nbase_url = \"https://example.test/v1\"\nenabled = false\n",
        )
        .map_err(ctx("write disabled provider"))?;
        let principal = channel_principal(GatewayEntry::Dream, "unused-peer");
        let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

        let result = gateway_assembly(
            GatewayEntry::Dream,
            home.path(),
            cwd.path(),
            principal,
            state_store,
            None,
        );

        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a home with only a disabled provider must not build an assembly".into(),
            ));
        };
        assert!(
            error.starts_with("gateway: "),
            "error must carry the \"gateway: \" prefix, got: {error}"
        );
        assert!(
            !error.contains("Echo"),
            "mount error must not describe an echo fallback, got: {error}"
        );
        Ok(())
    }

    #[test]
    fn config_has_usable_provider_true_when_default_provider_is_enabled() {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("plain".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config
            .providers
            .insert("plain".to_owned(), enabled_provider_toml("plain", true));

        assert!(config_has_usable_provider(&config));
    }

    #[test]
    fn config_has_usable_provider_false_when_only_provider_is_disabled() {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("seeded".to_owned());
        config.harness.default_model = Some("seeded-model".to_owned());
        config
            .providers
            .insert("seeded".to_owned(), enabled_provider_toml("seeded", false));

        assert!(!config_has_usable_provider(&config));
    }

    #[test]
    fn config_has_usable_provider_true_via_catalog_model_when_default_is_dangling() {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("missing".to_owned());
        config.harness.default_model = Some("missing-model".to_owned());
        config
            .providers
            .insert("catalog".to_owned(), enabled_provider_toml("catalog", true));
        config.models.insert(
            "catalog-model".to_owned(),
            harw_config::ModelToml {
                stream: None,
                id: "catalog-model".to_owned(),
                name: None,
                provider: "catalog".to_owned(),
                aliases: Vec::new(),
                context_window: None,
                max_tokens: None,
                prompt_caching: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
                default_reasoning_effort: None,
            },
        );

        assert!(config_has_usable_provider(&config));
    }

    #[test]
    fn config_has_usable_provider_false_for_an_entirely_empty_config() {
        assert!(!config_has_usable_provider(
            &harw_config::ResolvedConfig::default()
        ));
    }

    /// Minimaler `ProviderToml`-Testfixture mit explizit gesetztem `enabled`.
    fn enabled_provider_toml(name: &str, enabled: bool) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://example.test/v1".to_owned(),
            auth: None,
            auth_header: Some("none".to_owned()),
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: Vec::new(),
            enabled,
            origin_allowlist: harw_config::OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
        }
    }
}
