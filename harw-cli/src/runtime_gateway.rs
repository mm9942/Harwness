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
//! `Arc::clone` zwischen der Telegram- und der Dream-Montage.
//!
//! # Zwei Montagen je Gateway-Start (Befunde G1/G3)
//! `crate::gateway::mount_gateway_assembly` baut für jeden Daemon-Start je eine
//! Montage für [`GatewayEntry::Telegram`] und [`GatewayEntry::Dream`]; das
//! Modell der Dream-Montage speist den Dream-Scheduler, sodass Audit und Trace
//! Telegram-Turns und Dream-Läufe über ihre [`EntryKind`] unterscheiden.
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
/// - `peer` (`&str`): für `Telegram` die stabile, vom Bot-API authentifizierte
///   Peer-Kennung; für `Dream` ungenutzt, weil Dream-Läufe keinen externen
///   Peer haben (der Gateway übergibt `""`).
///
/// # Daemon-Platzhalter `telegram:gateway` (Befund G6)
/// Die Telegram-Montage des Daemons (`crate::gateway::mount_gateway_assembly`)
/// ruft diese Funktion bis P1.6 mit dem festen Peer `"gateway"` auf, weil es
/// beim Start noch keinen transport-gebundenen, authentifizierten Peer gibt.
/// `telegram:gateway` ist damit ein **Platzhalter**, kein echter
/// Telegram-Absender; die Rechte bleiben unverändert `Observer` mit
/// `{}`-Rechten. Mit P1.6 wird der Principal je admittiertem Ereignis aus dem
/// echten Peer abgeleitet.
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
/// weiterzulaufen.
pub(crate) fn gateway_assembly(
    entry: GatewayEntry,
    home: &Path,
    cwd: &Path,
    principal: Principal,
    state_store: Arc<dyn StateStore>,
    secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,
) -> Result<RuntimeAssembly, String> {
    let spec = crate::runtime_entry::runtime_spec(entry.entry_kind(), home, cwd, principal);
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_gateway_assembly_without_provider_config_returns_err_not_echo() {
        let home = tempfile::tempdir().expect("tempdir home");
        let cwd = tempfile::tempdir().expect("tempdir cwd");
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
            panic!("a tempdir home without provider config must not build an assembly");
        };
        assert!(
            error.starts_with("gateway: "),
            "error must carry the \"gateway: \" prefix, got: {error}"
        );
    }
}
