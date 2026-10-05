//! `CommandAdapter` — exponiert eine [`Operation`] als `/`-Slash-Command.
//!
//! # Warum eine Op mehrere `CommandAdapter` erzeugen kann
//!
//! Eine [`Operation`] darf in ihrer [`crate::operation::OperationMeta::surfaces`]-Liste
//! **mehrere** [`crate::operation::Surface::Command`]-Einträge deklarieren —
//! etwa einen mit [`crate::operation::CommandVisibility::TuiOnly`] unter Pfad `"/status"`
//! und einen zweiten mit [`crate::operation::CommandVisibility::ChannelReduced`] unter
//! Pfad `"/status-brief"`. [`CommandAdapter::from_operation`] erzeugt pro
//! `Surface::Command`-Eintrag genau **einen** Adapter, der Pfad und Sichtbarkeit aus
//! dem jeweiligen Surface-Eintrag bezieht. TUI-Dispatcher und Channel-Dispatcher
//! registrieren damit unabhängig voneinander, ohne die Op selbst zu kennen.
//!
//! # Verantwortungsbereich
//! - Filtert `Surface::Command`-Einträge aus `OperationMeta::surfaces`.
//! - Baut `OpInput { raw_args, json_args: Value::Null }` und delegiert an
//!   `Operation::run`.
//! - Kein Rendering, kein Output-Formatting — das bleibt höheren Crates
//!   (`harw-tui`, `harw-core`) vorbehalten.
//!
//! # Schlüsseltypen
//! - [`CommandAdapter`] — der Adapter selbst
//!
//! # Nebenläufigkeit
//! [`CommandAdapter`] ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
//! `Send + Sync`), `&'static str` und `Copy`-Enums ohne inneren Zustand.
//!
//! # Fehlertypen
//! [`crate::error::OpError`] — wird 1:1 von `Operation::run` weitergeleitet.
//!
//! # Beispiel
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_operations::adapter::CommandAdapter;
//! use harw_operations::operation::{
//!     BusyAvailability, CommandVisibility, OpFuture, OpInput, OpOutput, Operation,
//!     OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
//! };
//! use harw_operations::context::OpContext;
//!
//! struct MyOp;
//! impl Operation for MyOp {
//!     fn meta(&self) -> &OperationMeta {
//!         static M: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
//!         M.get_or_init(|| OperationMeta {
//!             name: "my.op",
//!             summary: "Beispiel-Operation.",
//!             domain: OperationDomain::Misc,
//!             permission: PermissionTier::Observer,
//!             surfaces: vec![Surface::Command {
//!                 path: "/my",
//!                 visibility: CommandVisibility::TuiOnly,
//!             }],
//!             aliases: &[],
//!             category: OperationCategory::Misc,
//!             args_schema: None,
//!             output_schema: None,
//!             busy: BusyAvailability::DeferredUntilTurnEnd,
//!         })
//!     }
//!     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
//!         Box::pin(async { Ok(OpOutput::from("ok".to_owned())) })
//!     }
//! }
//!
//! let adapters = CommandAdapter::from_operation(Arc::new(MyOp));
//! assert_eq!(adapters.len(), 1);
//! assert_eq!(adapters[0].path(), "/my");
//! ```

use std::sync::Arc;

use crate::context::OpContext;
use crate::error::OpError;
use crate::operation::{
    CommandVisibility, OpInput, OpOutput, Operation, OperationMeta, PermissionTier, Surface,
};

/// Adapter, der eine [`Operation`] als `/`-Slash-Command exponiert.
///
/// # Beschreibung
/// Ein `CommandAdapter` repräsentiert **eine** `Surface::Command`-Deklaration
/// aus [`OperationMeta::surfaces`]. Mehrere Adapter für dieselbe Op sind möglich,
/// wenn die Op mehrere `Surface::Command`-Einträge deklariert (z. B. TUI und Channel).
///
/// Der Adapter führt kein Rendering durch — er baut ausschließlich `OpInput` und
/// delegiert den Aufruf an [`Operation::run`].
///
/// # Nebenläufigkeit
/// `CommandAdapter` ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
/// `Send + Sync`), `&'static str` und `Copy`-Enums ohne inneren Zustand.
///
/// # Fehlertypen
/// [`OpError`] — wird in [`Self::dispatch`] 1:1 von `Operation::run` weitergeleitet.
///
/// # Beispiel
/// ```rust,no_run
/// // Erzeugung über from_operation; direkter Konstruktor ist nicht öffentlich.
/// use std::sync::Arc;
/// use harw_operations::adapter::CommandAdapter;
/// ```
pub struct CommandAdapter {
    op: Arc<dyn Operation>,
    path: &'static str,
    visibility: CommandVisibility,
    permission: PermissionTier,
    operation_name: &'static str,
}

impl CommandAdapter {
    /// Erzeugt einen `CommandAdapter` pro `Surface::Command`-Eintrag der übergebenen Operation.
    ///
    /// # Beschreibung
    /// Iteriert über [`OperationMeta::surfaces`] und erzeugt für jeden
    /// `Surface::Command { path, visibility }`-Eintrag einen Adapter.
    /// Die Berechtigung wird einmalig aus [`OperationMeta::permission`] übernommen
    /// und gilt für alle erzeugten Adapter dieser Operation.
    /// `Surface::ModelTool`-Einträge werden stillschweigend ignoriert.
    ///
    /// # Argumente
    /// - `op` (`Arc<dyn Operation>`): Die Operation, die als Command exponiert werden soll.
    ///   Der `Arc` wird pro gefundener `Surface::Command` mit `Arc::clone` dupliziert —
    ///   die Operation selbst wird nicht geklont.
    ///
    /// # Returns
    /// Ein `Vec<CommandAdapter>` mit einem Eintrag pro `Surface::Command`-Deklaration.
    /// Operationen ohne `Surface::Command`-Eintrag ergeben einen leeren `Vec`.
    ///
    /// # Errors
    /// Keine — diese Funktion schlägt nicht fehl.
    ///
    /// # Concurrency
    /// Sicher in Multi-Thread-Umgebungen: nur lesender Zugriff auf `meta()` und
    /// atomares Inkrementieren des `Arc`-Referenzzählers per `Arc::clone`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_operations::adapter::CommandAdapter;
    /// // Ops ohne Surface::Command liefern einen leeren Vec:
    /// // let adapters = CommandAdapter::from_operation(Arc::new(NoSurfaceOp));
    /// // assert!(adapters.is_empty());
    /// ```
    #[must_use]
    pub fn from_operation(op: Arc<dyn Operation>) -> Vec<Self> {
        let meta: &OperationMeta = op.meta();
        let permission = meta.permission;
        let operation_name = meta.name;
        meta.surfaces
            .iter()
            .filter_map(|surface| {
                if let Surface::Command { path, visibility } = surface {
                    Some(Self {
                        op: Arc::clone(&op),
                        path,
                        visibility: *visibility,
                        permission,
                        operation_name,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Gibt den eingetragenen Command-Pfad zurück.
    ///
    /// # Beschreibung
    /// Liefert den statischen Pfad-String aus `Surface::Command { path, .. }`,
    /// z. B. `"/status"` oder `"/session/list"`.
    ///
    /// # Returns
    /// `&str` — der Pfad-String. Lebensdauer ist an `&self` gebunden (`'static` intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.path(), "/session/list");
    /// ```
    #[must_use]
    pub fn path(&self) -> &str {
        self.path
    }

    /// Gibt die Sichtbarkeit dieses Commands zurück.
    ///
    /// # Beschreibung
    /// Liefert den [`CommandVisibility`]-Wert aus der `Surface::Command`-Deklaration,
    /// der steuert, ob der Command in der TUI, in Channels oder in beidem erscheint.
    ///
    /// # Returns
    /// [`CommandVisibility`] — `Copy`, keine Allokation.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.visibility(), CommandVisibility::TuiOnly);
    /// ```
    #[must_use]
    pub fn visibility(&self) -> CommandVisibility {
        self.visibility
    }

    /// Gibt die aus `OperationMeta::permission` übernommene Mindest-Berechtigungsstufe zurück.
    ///
    /// # Beschreibung
    /// Bestimmt, welche Caller-Stufe zur Ausführung des Commands mindestens nötig ist.
    /// Der Wert wird einmalig beim Erzeugen des Adapters aus `OperationMeta::permission`
    /// kopiert und ändert sich nicht.
    ///
    /// # Returns
    /// [`PermissionTier`] — `Copy`, keine Allokation.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.permission(), PermissionTier::Observer);
    /// ```
    #[must_use]
    pub fn permission(&self) -> PermissionTier {
        self.permission
    }

    /// Prüft, ob dieser konkrete Aufruf auf einer Channel-Fläche zulässig ist.
    ///
    /// `TuiOnly` ist immer gesperrt, `ChannelParity` übernimmt die gesamte
    /// Command-Grammatik. `ChannelReduced` ist fail-closed und erlaubt nur
    /// die von [`Operation::channel_subcommands`] deklarierten Formen.
    #[must_use]
    pub fn allows_channel_invocation(&self, raw_args: &[String]) -> bool {
        match self.visibility {
            CommandVisibility::TuiOnly => false,
            CommandVisibility::ChannelParity => true,
            CommandVisibility::ChannelReduced => {
                let allowed = self.op.channel_subcommands();
                if allowed.contains(&"*") {
                    return true;
                }
                let first = raw_args.first().map(String::as_str).unwrap_or("-");
                allowed.contains(&first)
            }
        }
    }

    /// Gibt den maschinellen Namen der zugrundeliegenden Operation zurück.
    ///
    /// # Beschreibung
    /// Liefert [`OperationMeta::name`] (z. B. `"session.list"`). Der Wert ist
    /// identisch mit `self.operation().meta().name`, aber ohne zusätzlichen
    /// Trait-Dispatch.
    ///
    /// # Returns
    /// `&str` — der maschinelle Op-Name. Lebensdauer an `&self` (`'static` intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.operation_name(), "session.list");
    /// ```
    #[must_use]
    pub fn operation_name(&self) -> &str {
        self.operation_name
    }

    /// Führt die Operation mit rohen `/command`-Argumenten aus.
    ///
    /// # Beschreibung
    /// Baut `OpInput { raw_args, json_args: serde_json::Value::Null }` und
    /// delegiert den Aufruf vollständig an `Operation::run`. Es findet kein
    /// Rendering und keine Argument-Transformation statt — der zurückgegebene
    /// [`OpOutput`] ist das direkte Ergebnis der Operation.
    ///
    /// # Arguments
    /// - `ctx` (`&OpContext`): Ausführungs-Kontext (Session, Turn, Sandbox, Services).
    ///   Wird unverändert an `Operation::run` weitergegeben.
    /// - `raw_args` (`Vec<String>`): Rohe Argumenttoken aus der Command-Zeile,
    ///   z. B. `["--limit", "10"]`. Darf leer sein.
    ///
    /// # Returns
    /// - `Ok(OpOutput)`: Die Operation wurde erfolgreich ausgeführt.
    /// - `Err(OpError)`: Fehler, der 1:1 von `Operation::run` stammt.
    ///
    /// # Errors
    /// - [`OpError::InvalidArguments`]: `raw_args` sind syntaktisch oder semantisch ungültig
    ///   (gemeldet von der Op).
    /// - [`OpError::Execution`]: Laufzeitfehler während der Operationsausführung.
    /// - [`OpError::NotAvailable`]: Operation im aktuellen Kontext nicht verfügbar.
    ///
    /// # Concurrency
    /// Async, `Send`-fähig. Der Adapter selbst hat keinen veränderlichen Zustand;
    /// mehrere gleichzeitige `dispatch`-Aufrufe auf demselben Adapter sind sicher.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // let out = adapter.dispatch(&ctx, vec!["--limit".to_owned(), "5".to_owned()]).await?;
    /// // println!("{}", out.text);
    /// ```
    pub async fn dispatch(
        &self,
        ctx: &OpContext,
        raw_args: Vec<String>,
    ) -> Result<OpOutput, OpError> {
        use tracing::Instrument;
        let span = tracing::info_span!(
            "operation.command",
            op = self.operation_name,
            path = self.path,
            arg_count = raw_args.len()
        );
        async move {
            let start = std::time::Instant::now();
            let input = OpInput::command(self.path, raw_args);
            let result = self.op.run(ctx, input).await;
            tracing::info!(
                duration_ms = start.elapsed().as_millis() as u64,
                status = if result.is_ok() { "ok" } else { "err" },
                "operation.command.done"
            );
            result
        }
        .instrument(span)
        .await
    }

    /// Gibt eine Referenz auf die zugrundeliegende Operation zurück.
    ///
    /// # Beschreibung
    /// Ermöglicht Introspektion oder Meta-Zugriff auf die Op, ohne den Adapter zu
    /// konsumieren. Das `Arc` wird nicht geklont — die Referenz ist an `&self` gebunden.
    ///
    /// # Returns
    /// `&Arc<dyn Operation>` — Referenz auf den geteilten Op-Pointer.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // let name = adapter.operation().meta().name;
    /// ```
    #[must_use]
    pub fn operation(&self) -> &Arc<dyn Operation> {
        &self.op
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    use super::CommandAdapter;
    use crate::context::{OpContext, ServiceMap};
    use crate::error::OpError;
    use crate::operation::{
        ApprovalPolicy, BusyAvailability, CommandVisibility, OpFuture, OpInput, OpOutput,
        Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    // ── test helpers ─────────────────────────────────────────────────────────

    /// Erstellt einen minimalen [`OpContext`] für Tests.
    ///
    /// Verwendet einen atomaren Zähler für Thread-sichere, eindeutige
    /// Verzeichnisnamen, sodass parallele Tests nicht kollidieren.
    fn make_test_ctx() -> TestResult<(OpContext, PathBuf)> {
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-cmd-adapter-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws"))
            .map_err(ctx("Test-Workspace-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
        Ok((ctx, tmp))
    }

    // ── test-op fixtures ──────────────────────────────────────────────────────

    /// Op ohne jegliche Surface-Deklaration.
    struct NoSurfaceOp;

    impl Operation for NoSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "no-surface",
                summary: "Keine Flächen.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("noop".to_owned())) })
        }
    }

    /// Op mit genau einer `Surface::Command`-Fläche.
    struct SingleCommandOp;

    impl Operation for SingleCommandOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "single-command",
                summary: "Ein Command.",
                domain: OperationDomain::Session,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::Command {
                    path: "/single",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("single".to_owned())) })
        }
    }

    /// Op mit zwei `Surface::Command`-Flächen (verschiedene Pfade und Visibilities).
    struct TwoCommandOp;

    impl Operation for TwoCommandOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "two-command",
                summary: "Zwei Commands.",
                domain: OperationDomain::Agents,
                permission: PermissionTier::Maintainer,
                surfaces: vec![
                    Surface::Command {
                        path: "/alpha",
                        visibility: CommandVisibility::TuiOnly,
                    },
                    Surface::Command {
                        path: "/beta",
                        visibility: CommandVisibility::ChannelReduced,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("two".to_owned())) })
        }

        fn channel_subcommands(&self) -> &'static [&'static str] {
            &["-", "show"]
        }
    }

    /// Op mit gemischten Surfaces: ein Command und ein ModelTool.
    struct MixedSurfaceOp;

    impl Operation for MixedSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "mixed-surface",
                summary: "Command + ModelTool.",
                domain: OperationDomain::Execution,
                permission: PermissionTier::Owner,
                surfaces: vec![
                    Surface::Command {
                        path: "/mixed",
                        visibility: CommandVisibility::ChannelParity,
                    },
                    Surface::ModelTool {
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("mixed".to_owned())) })
        }
    }

    /// Op, die `input.raw_args.len()` als Textantwort zurückgibt.
    struct ArgCountOp;

    impl Operation for ArgCountOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "arg-count",
                summary: "Gibt die Anzahl der raw_args zurück.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Command {
                    path: "/argcount",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
            Box::pin(async move {
                Ok(OpOutput::from(format!(
                    "{}",
                    input.invocation.raw_args().len()
                )))
            })
        }
    }

    /// Op, die stets `OpError::InvalidArguments` zurückgibt.
    struct InvalidArgsOp;

    impl Operation for InvalidArgsOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "invalid-args",
                summary: "Schlägt immer mit InvalidArguments fehl.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Command {
                    path: "/invalid",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Err(OpError::InvalidArguments(
                    "kein Argument erlaubt".to_owned(),
                ))
            })
        }
    }

    // ── from_operation tests ──────────────────────────────────────────────────

    #[test]
    fn test_from_operation_no_command_surface_returns_empty_vec() {
        let op: Arc<dyn Operation> = Arc::new(NoSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert!(
            adapters.is_empty(),
            "Op ohne Surface::Command muss leeren Vec liefern"
        );
    }

    #[test]
    fn test_from_operation_single_command_surface_returns_one_adapter_with_correct_path() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters.len(), 1, "Genau ein Adapter erwartet");
        assert_eq!(adapters[0].path(), "/single");
    }

    #[test]
    fn test_from_operation_two_command_surfaces_returns_two_adapters_in_order() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters.len(), 2, "Zwei Adapter erwartet");
        assert_eq!(
            adapters[0].path(),
            "/alpha",
            "Erster Adapter muss /alpha sein"
        );
        assert_eq!(
            adapters[1].path(),
            "/beta",
            "Zweiter Adapter muss /beta sein"
        );
    }

    #[test]
    fn test_from_operation_two_command_surfaces_visibilities_match_declaration_order() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].visibility(), CommandVisibility::TuiOnly);
        assert_eq!(adapters[1].visibility(), CommandVisibility::ChannelReduced);
    }

    #[test]
    fn test_from_operation_mixed_surfaces_returns_only_command_adapters() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters.len(),
            1,
            "Nur der Command-Eintrag soll einen Adapter erzeugen; ModelTool wird ignoriert"
        );
        assert_eq!(adapters[0].path(), "/mixed");
    }

    // ── channel policy tests ─────────────────────────────────────────────────

    #[test]
    fn test_channel_policy_tui_only_is_always_rejected() {
        let adapters = CommandAdapter::from_operation(Arc::new(TwoCommandOp));
        assert!(!adapters[0].allows_channel_invocation(&[]));
        assert!(!adapters[0].allows_channel_invocation(&["show".to_owned()]));
    }

    #[test]
    fn test_channel_policy_parity_accepts_arbitrary_args() {
        let adapters = CommandAdapter::from_operation(Arc::new(MixedSurfaceOp));
        assert!(adapters[0].allows_channel_invocation(&[]));
        assert!(adapters[0].allows_channel_invocation(&["anything".to_owned()]));
    }

    #[test]
    fn test_channel_policy_reduced_is_fail_closed_to_declared_subcommands() {
        let adapters = CommandAdapter::from_operation(Arc::new(TwoCommandOp));
        let reduced = &adapters[1];
        assert!(reduced.allows_channel_invocation(&[]));
        assert!(reduced.allows_channel_invocation(&["show".to_owned()]));
        assert!(!reduced.allows_channel_invocation(&["run".to_owned()]));
    }

    // ── dispatch tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dispatch_passes_raw_args_to_operation() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ArgCountOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let args = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let result = adapters[0].dispatch(&ctx, args).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Ok(out) => assert_eq!(out.text, "3", "raw_args.len() soll 3 sein"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_dispatch_passes_empty_raw_args() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ArgCountOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapters[0].dispatch(&ctx, vec![]).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Ok(out) => assert_eq!(out.text, "0"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_dispatch_propagates_invalid_arguments_error() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(InvalidArgsOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapters[0].dispatch(&ctx, vec![]).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet OpError::InvalidArguments, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── accessor tests ────────────────────────────────────────────────────────

    #[test]
    fn test_permission_returns_operation_meta_permission() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].permission(),
            PermissionTier::Operator,
            "permission() muss OperationMeta::permission widerspiegeln"
        );
    }

    #[test]
    fn test_permission_owner_tier_is_propagated() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].permission(), PermissionTier::Owner);
    }

    #[test]
    fn test_visibility_returns_surface_command_visibility() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].visibility(),
            CommandVisibility::TuiOnly,
            "visibility() muss den Wert aus Surface::Command liefern"
        );
    }

    #[test]
    fn test_visibility_channel_parity_is_propagated() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].visibility(), CommandVisibility::ChannelParity);
    }

    #[test]
    fn test_operation_name_returns_meta_name() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].operation_name(),
            "single-command",
            "operation_name() muss OperationMeta::name liefern"
        );
    }

    #[test]
    fn test_operation_name_consistent_across_multiple_adapters_of_same_op() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].operation_name(), "two-command");
        assert_eq!(adapters[1].operation_name(), "two-command");
    }

    #[test]
    fn test_operation_returns_arc_with_correct_meta_name() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        assert_eq!(adapters[0].operation().meta().name, "single-command");
    }

    // ── Send + Sync compile-time check ────────────────────────────────────────

    #[test]
    fn test_command_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CommandAdapter>();
    }
}
