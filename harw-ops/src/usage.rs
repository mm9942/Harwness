//! `/usage` — zeigt die aufgezeichnete Token-Nutzung und Wächter-Ereignisse
//! der aktuellen Sitzung (Addendum F+G, Agent F-FIX).
//!
//! # Verantwortungsbereich
//! Liest den zuletzt gespeicherten Sitzungszustand
//! ([`harw_core::state_store::SessionStateSnapshot`]) über den im
//! [`OpContext`] registrierten [`harw_core::state_store::StateStore`]
//! ([`harw_core_bridge::OpContextCoreExt::state_store`]) und formatiert
//! dessen `total_usage`-Feld deutschsprachig. Fehlt der `StateStore`, oder
//! wurde für die aktuelle Session noch nie ein Zustand gespeichert, liefert
//! die Operation eine erklärende deutsche Meldung statt erfundener Zahlen.
//!
//! Zusätzlich liest sie — bestbemüht, wenn im [`OpContext`] registriert —
//! den Sitzungs-Metadaten-Sidecar ([`harw_session_store::meta::SessionMeta`])
//! über [`SessionsMetaRoot`] und zeigt daraus `usage_rounds` (die echte
//! Rundenzahl, siehe [`harw_session_store::meta::add_usage_round`]) sowie
//! `drift_events` (Wächter-Ereignisse je Art, siehe
//! [`harw_session_store::meta::add_drift_event`]).
//!
//! # Datenquelle (wichtig für Aufrufer)
//! `harw_operations::SessionControlSnapshot` — der andere Snapshot-Typ, den
//! diese Crate kennt — trägt **keine** Nutzungsfelder (nur
//! `reasoning_effort`/`active_model`/`active_provider`/`interaction_mode`,
//! siehe `harw-operations/src/session_control.rs`). Die einzige in dieser
//! Laufzeit erreichbare Nutzungsquelle ist deshalb
//! `SessionStateSnapshot::total_usage`, gelesen über
//! [`harw_core_bridge::OpContextCoreExt::state_store`]. `SessionStateSnapshot`
//! führt außerdem **keinen Rundenzähler** — ohne registrierten
//! [`SessionsMetaRoot`] weist die Ausgabe das für die Zeile „Runden" explizit
//! aus, statt eine erfundene Zahl zu zeigen.
//!
//! # Fehlertypen
//! Kein eigener Fehlerfall im Normalbetrieb: ein fehlender `StateStore` oder
//! ein fehlender Snapshot werden als erklärender Text in [`OpOutput`]
//! zurückgegeben, nicht als [`OpError`]. Nur ein tatsächlicher Lesefehler des
//! `StateStore` selbst liefert [`OpError::Execution`]. Ein fehlender oder
//! nicht lesbarer Sidecar ist dagegen best-effort: die Runden-/Wächter-Zeilen
//! fehlen dann, aber die Token-Nutzung wird trotzdem gezeigt (siehe
//! [`SessionsMetaRoot`]).
//!
//! # Nebenläufigkeit
//! Zustandslos; liest höchstens einmal asynchron aus dem `StateStore` und
//! höchstens einmal synchron (auf dem aufrufenden Executor) aus dem
//! Metadaten-Sidecar.

use std::path::PathBuf;

use harw_core::state_store::SessionStateSnapshot;
use harw_core_bridge::OpContextCoreExt;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::meta::SessionMeta;

/// Argumente der `/usage`-Operation (keine).
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct UsageArgs {}

/// Wurzelverzeichnis des Sitzungs-Metadaten-Sidecars (Addendum F+G).
///
/// # Description
/// Ein optionaler [`harw_operations::context::ServiceMap`]-Dienst: das
/// Wurzelverzeichnis, unter dem `harw_session_store::meta` seine
/// `<session-id>.meta.json`-Sidecars ablegt — dasselbe Verzeichnis, das der
/// registrierte `TranscriptStateStore` intern für `add_usage_round`/
/// `add_drift_event` verwendet (siehe `harw-core/src/state_store.rs`).
///
/// Ein Composition Root registriert diesen Dienst, damit `/usage` die echte
/// Rundenzahl und die Wächter-Ereignisse zeigen kann; ist er nicht
/// registriert, zeigt `/usage` weiterhin die Token-Nutzung, aber ohne
/// Runden-/Wächter-Zeilen (siehe Moduldoku „Fehlertypen").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsMetaRoot(pub PathBuf);

/// Deutschsprachige Meldung, wenn in diesem Ausführungskontext kein
/// `StateStore` registriert ist.
const NO_STATE_STORE_MESSAGE: &str = "Für diese Sitzung liegen keine Token-Nutzungsdaten vor: in diesem Ausführungskontext ist kein Zustandsspeicher (StateStore) registriert.";

/// Deutschsprachige Meldung, wenn ein `StateStore` registriert ist, aber
/// noch nie ein Sitzungszustand für diese Session gespeichert wurde.
const NO_SNAPSHOT_MESSAGE: &str = "Für diese Sitzung wurde noch kein Sitzungszustand gespeichert — es liegen noch keine Token-Nutzungsdaten vor.";

/// Formatiert eine [`SessionStateSnapshot`] (und optional den
/// Metadaten-Sidecar) als deutschsprachigen Nutzungsbericht (Runden, Input,
/// Output, Cache-Treffer, Cache-Schreibvorgänge, Cache-Trefferquote,
/// Wächter-Ereignisse).
///
/// # Description
/// `SessionStateSnapshot` selbst führt keinen Rundenzähler (siehe
/// Moduldoku) — ohne `meta` weist die Zeile „Runden" das explizit aus, statt
/// eine falsche Zahl anzuzeigen. Mit `meta` zeigt „Runden" die echte
/// `usage_rounds`-Zahl, und eine zusätzliche Zeile listet
/// `meta.drift_events` je Art (sortiert nach Art, da `BTreeMap`), oder
/// „keine", wenn die Session bisher kein Wächter-Ereignis hatte. Die
/// Cache-Trefferquote ist `Cache-Treffer / Input` in Prozent, gerundet auf
/// eine Nachkommastelle; bei `Input == 0` wird `n/v` ausgegeben.
#[must_use]
fn format_usage(snapshot: &SessionStateSnapshot, meta: Option<&SessionMeta>) -> String {
    let usage = &snapshot.total_usage;
    let cache_hits = usage.cached_tokens.unwrap_or(0);
    let cache_writes = usage.cache_write_tokens.unwrap_or(0);
    let hit_rate = if usage.input_tokens > 0 {
        format!(
            "{:.1}%",
            (cache_hits as f64 / usage.input_tokens as f64) * 100.0
        )
    } else {
        "n/v".to_owned()
    };
    let rounds_line = match meta {
        Some(meta) => format!("Runden: {}", meta.usage_rounds),
        None => {
            "Runden: nicht erfasst (Sitzungszustand führt keinen Rundenzähler)".to_owned()
        }
    };
    let drift_line = match meta {
        Some(meta) if meta.drift_events.is_empty() => "\nWächter-Ereignisse: keine".to_owned(),
        Some(meta) => {
            let mut entries: Vec<String> = meta
                .drift_events
                .iter()
                .map(|(kind, count)| format!("  {kind}: {count}"))
                .collect();
            entries.sort();
            format!("\nWächter-Ereignisse:\n{}", entries.join("\n"))
        }
        None => String::new(),
    };
    format!(
        "Token-Nutzung dieser Sitzung:\n\
         {rounds_line}\n\
         Input: {input}\n\
         Output: {output}\n\
         Cache-Treffer: {cache_hits}\n\
         Cache-Schreibvorgänge: {cache_writes}\n\
         Cache-Trefferquote: {hit_rate}{drift_line}",
        input = usage.input_tokens,
        output = usage.output_tokens,
    )
}

/// Zeigt die aufgezeichnete Token-Nutzung und die Wächter-Ereignisse der
/// aktuellen Sitzung (Addendum F+G).
///
/// # Beschreibung
/// Liest den zuletzt gespeicherten [`SessionStateSnapshot`] über den im
/// [`OpContext`] registrierten `StateStore`
/// ([`harw_core_bridge::OpContextCoreExt::state_store`]) und formatiert
/// dessen `total_usage` (siehe [`format_usage`]). Ist zusätzlich ein
/// [`SessionsMetaRoot`] registriert, lädt sie best-effort den
/// Metadaten-Sidecar ([`harw_session_store::meta::load_or_derive`]) und
/// nimmt dessen `usage_rounds` und `drift_events` in den Bericht auf; ein
/// Lesefehler des Sidecars wird nur geloggt (`tracing::warn!`) und führt
/// nicht dazu, dass die Operation fehlschlägt — die Token-Nutzung bleibt in
/// jedem Fall sichtbar. Siehe Moduldoku „Datenquelle" für die genaue
/// Herkunft und ihre Grenzen.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert `state_store()`, `session_id()` und
///   optional den Dienst [`SessionsMetaRoot`].
/// - `_args` (`UsageArgs`): keine Argumente.
///
/// # Rückgabe
/// [`OpOutput`] mit dem formatierten Nutzungsbericht, oder einer
/// erklärenden deutschen Meldung, wenn kein `StateStore` registriert ist
/// oder noch kein Sitzungszustand existiert.
///
/// # Fehler
/// - [`OpError::Execution`]: der registrierte `StateStore` meldet einen
///   Lesefehler.
///
/// # Nebenläufigkeit
/// Zustandslos; liest höchstens einmal asynchron aus dem `StateStore` und
/// höchstens einmal synchron aus dem Metadaten-Sidecar.
#[operation(
    name = "usage",
    summary = "Zeigt die aufgezeichnete Token-Nutzung der aktuellen Sitzung.",
    domain = "session",
    permission = "observer",
    command(path = "/usage", visibility = "channel_parity")
)]
async fn usage(ctx: &OpContext, _args: UsageArgs) -> Result<OpOutput, OpError> {
    let Some(store) = ctx.state_store() else {
        return Ok(OpOutput::from(NO_STATE_STORE_MESSAGE.to_owned()));
    };
    let snapshot = store
        .load_session_state(ctx.session_id())
        .await
        .map_err(|error| OpError::Execution(error.to_string()))?;
    let Some(snapshot) = snapshot else {
        return Ok(OpOutput::from(NO_SNAPSHOT_MESSAGE.to_owned()));
    };
    let meta = ctx.service::<SessionsMetaRoot>().and_then(|root| {
        match harw_session_store::meta::load_or_derive(&root.0, ctx.session_id()) {
            Ok(meta) => Some(meta),
            Err(error) => {
                tracing::warn!(
                    session = %ctx.session_id(),
                    error = %error,
                    "usage.session_meta_unavailable"
                );
                None
            }
        }
    });
    Ok(OpOutput::from(format_usage(&snapshot, meta.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::toks;
    use harw_operations::FromRawArgs;
    use harw_operations::context::ServiceMap;
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TokenUsage, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-usage-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("ws")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_usage_args_from_raw_args_ignores_tokens() {
        let args = UsageArgs::from_raw_args(&toks(&["ignored"]));
        assert!(args.is_ok());
    }

    #[tokio::test]
    async fn usage_without_state_store_returns_clear_german_message() {
        let (ctx, root) = test_context();

        let result = super::usage(&ctx, UsageArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Ok(output) => assert_eq!(output.text, NO_STATE_STORE_MESSAGE),
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    fn empty_activation() -> harw_core::state_store::ActivationSnapshot {
        harw_core::state_store::ActivationSnapshot {
            profile: harw_core::state_store::PersistedToolProfile::Minimal,
            enabled_tools: Default::default(),
            disabled_tools: Default::default(),
        }
    }

    #[test]
    fn format_usage_reports_all_six_fields_with_a_computed_hit_rate() {
        let snapshot = SessionStateSnapshot {
            version: 1,
            mode: harw_core::InteractionMode::default(),
            total_usage: TokenUsage {
                input_tokens: 200,
                output_tokens: 50,
                reasoning_tokens: None,
                cached_tokens: Some(50),
                cache_write_tokens: Some(10),
            },
            executable_snapshot_id: None,
            base_activation: empty_activation(),
            activation: empty_activation(),
        };

        let report = format_usage(&snapshot, None);

        assert!(report.contains("Runden:"));
        assert!(report.contains("Input: 200"));
        assert!(report.contains("Output: 50"));
        assert!(report.contains("Cache-Treffer: 50"));
        assert!(report.contains("Cache-Schreibvorgänge: 10"));
        assert!(report.contains("Cache-Trefferquote: 25.0%"));
    }

    #[test]
    fn format_usage_reports_unknown_hit_rate_without_input_tokens() {
        let snapshot = SessionStateSnapshot {
            version: 1,
            mode: harw_core::InteractionMode::default(),
            total_usage: TokenUsage::default(),
            executable_snapshot_id: None,
            base_activation: empty_activation(),
            activation: empty_activation(),
        };

        let report = format_usage(&snapshot, None);

        assert!(report.contains("Cache-Trefferquote: n/v"));
    }

    // Leitet ein frisches `SessionMeta` über den echten Produktionspfad ab
    // (`load_or_derive` über ein leeres, temporäres Wurzelverzeichnis ohne
    // Transcript), statt die vielen `SessionMeta`-Felder von Hand zu
    // literalisieren.
    fn fresh_meta_for_test() -> (harw_session_store::meta::SessionMeta, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-usage-meta-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create test meta root");
        let session = SessionId::new();
        let meta = harw_session_store::meta::load_or_derive(&root, &session)
            .expect("derive fresh meta over an empty root");
        (meta, root)
    }

    #[test]
    fn format_usage_with_meta_shows_real_rounds_and_drift_events() {
        let snapshot = SessionStateSnapshot {
            version: 1,
            mode: harw_core::InteractionMode::default(),
            total_usage: TokenUsage::default(),
            executable_snapshot_id: None,
            base_activation: empty_activation(),
            activation: empty_activation(),
        };
        let (mut meta, root) = fresh_meta_for_test();
        meta.usage_rounds = 7;
        meta.drift_events
            .insert("repeated_failing_call".to_owned(), 3);

        let report = format_usage(&snapshot, Some(&meta));
        std::fs::remove_dir_all(root).ok();

        assert!(report.contains("Runden: 7"));
        assert!(report.contains("Wächter-Ereignisse:"));
        assert!(report.contains("repeated_failing_call: 3"));
    }

    #[test]
    fn format_usage_with_meta_and_no_drift_events_says_so() {
        let snapshot = SessionStateSnapshot {
            version: 1,
            mode: harw_core::InteractionMode::default(),
            total_usage: TokenUsage::default(),
            executable_snapshot_id: None,
            base_activation: empty_activation(),
            activation: empty_activation(),
        };
        let (meta, root) = fresh_meta_for_test();

        let report = format_usage(&snapshot, Some(&meta));
        std::fs::remove_dir_all(root).ok();

        assert!(report.contains("Wächter-Ereignisse: keine"));
    }
}
