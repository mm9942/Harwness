//! `WebAdapter` — exponiert eine [`Operation`] als HTTP-Route für `harw-web`.
//!
//! # Verantwortungsbereich
//! Dieser Adapter übersetzt eine `Surface::Web`-Deklaration einer Operation in
//! eine zur Laufzeit nutzbare Adapter-Instanz, die `harw-web` (Knoten UI-00)
//! über einen Unix-Socket erreichbar macht. Wie [`crate::adapter::CommandAdapter`]
//! und [`crate::adapter::ModelToolAdapter`] kennt dieser Adapter **keinen**
//! Transport, kein Rendering und keine Registry-Details — er filtert
//! [`Surface::Web`]-Einträge aus [`OperationMeta::surfaces`] und delegiert
//! `invoke` unverändert an [`Operation::run`].
//!
//! # Warum keine dritte Autorisierungslogik entsteht
//! `WebAdapter` erschöpft sich in genau denselben zwei Achsen, die
//! [`ModelToolAdapter`] bereits kennt: `readonly` (steuert in `harw-web` die
//! zulässige HTTP-Methode, `GET` vs. `POST`) und [`ApprovalPolicy`]
//! (steuert, ob `harw-web` eine Operation direkt ausführen darf oder an eine
//! Genehmigung weiterreichen muss). Es gibt bewusst **keinen** eigenen
//! `WebAdapter`-Genehmigungspfad — das ist die in `harw-web`s Moduldoku
//! geforderte Eigenschaft „kein zweiter Autoritätspfad", hier auf
//! Typ-Ebene erzwungen: `WebAdapter::approval()` liest exakt das Feld, das
//! `Surface::Web` von `Surface::ModelTool` übernimmt.
//!
//! # Kardinalität
//! Wie [`crate::adapter::CommandAdapter`] erzeugt [`WebAdapter::from_operation`]
//! **einen Adapter pro `Surface::Web`-Eintrag** — eine Operation kann also
//! mehrere HTTP-Routen deklarieren (z. B. eine kanonische und einen Alias-Pfad),
//! muss es aber nicht.
//!
//! # Warum keine Route ohne `OperationMeta` konstruierbar ist
//! [`WebAdapter`] hat keinen öffentlichen Konstruktor außer
//! [`WebAdapter::from_operation`], und dieser nimmt ausschließlich
//! `Arc<dyn Operation>` entgegen. Da [`Operation::meta`] Teil des Traits ist,
//! gibt es keinen Wert vom Typ `Arc<dyn Operation>`, der keine
//! [`OperationMeta`] liefert — eine Route ohne Metadaten ist deshalb nicht
//! nur ungeprüft, sondern **nicht ausdrückbar**: es gibt keinen Weg, ein
//! `WebAdapter` aus einem rohen Pfad-String zu bauen.
//!
//! # Nebenläufigkeit
//! `WebAdapter` ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
//! `Send + Sync`), `&'static str`, `bool` und `Copy`-Enums ohne inneren
//! Zustand — identisch zur Nebenläufigkeitsbegründung von
//! [`crate::adapter::CommandAdapter`].
//!
//! # Fehlertypen
//! [`crate::error::OpError`] — wird in [`WebAdapter::invoke`] 1:1 von
//! [`Operation::run`] weitergeleitet.
//!
//! # Beispiel
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_operations::adapter::WebAdapter;
//! use harw_operations::operation::{
//!     ApprovalPolicy, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
//!     OperationDomain, OperationMeta, PermissionTier, Surface,
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
//!             surfaces: vec![Surface::Web {
//!                 path: "/api/my",
//!                 readonly: true,
//!                 approval: ApprovalPolicy::None,
//!             }],
//!             aliases: &[],
//!             category: OperationCategory::Misc,
//!             args_schema: None,
//!         })
//!     }
//!     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
//!         Box::pin(async { Ok(OpOutput { text: "ok".to_owned() }) })
//!     }
//! }
//!
//! let adapters = WebAdapter::from_operation(Arc::new(MyOp));
//! assert_eq!(adapters.len(), 1);
//! assert_eq!(adapters[0].path(), "/api/my");
//! ```

use std::sync::Arc;

use crate::context::OpContext;
use crate::error::OpError;
use crate::operation::{
    ApprovalPolicy, OpInput, OpOutput, Operation, OperationMeta, PermissionTier, Surface,
};

/// Adapter, der eine [`Operation`] als HTTP-Route für `harw-web` exponiert.
///
/// # Beschreibung
/// Ein `WebAdapter` repräsentiert **eine** `Surface::Web`-Deklaration aus
/// [`OperationMeta::surfaces`]. Mehrere Adapter für dieselbe Operation sind
/// möglich, wenn die Operation mehrere `Surface::Web`-Einträge deklariert.
///
/// Der Adapter führt kein HTTP-Parsing, kein Rendering und keine
/// Peer-Identifikation durch — das bleibt `harw-web` vorbehalten. Er baut
/// ausschließlich `OpInput` und delegiert den Aufruf an [`Operation::run`].
///
/// # Nebenläufigkeit
/// `WebAdapter` ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
/// `Send + Sync`), `&'static str`, `bool` und `Copy`-Enums ohne inneren
/// Zustand.
///
/// # Fehlertypen
/// [`OpError`] — wird in [`Self::invoke`] 1:1 von [`Operation::run`]
/// weitergeleitet.
///
/// # Beispiel
/// ```rust,no_run
/// // Erzeugung über from_operation; direkter Konstruktor ist nicht öffentlich.
/// use std::sync::Arc;
/// use harw_operations::adapter::WebAdapter;
/// ```
pub struct WebAdapter {
    op: Arc<dyn Operation>,
    path: &'static str,
    readonly: bool,
    approval: ApprovalPolicy,
    permission: PermissionTier,
    operation_name: &'static str,
}

impl std::fmt::Debug for WebAdapter {
    /// Druckt nur die Routing-relevanten Felder — nie `self.op` (ein
    /// `Arc<dyn Operation>` hat kein aussagekräftiges `Debug`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebAdapter")
            .field("operation_name", &self.operation_name)
            .field("path", &self.path)
            .field("readonly", &self.readonly)
            .field("approval", &self.approval)
            .field("permission", &self.permission)
            .finish()
    }
}

impl WebAdapter {
    /// Erzeugt einen `WebAdapter` pro `Surface::Web`-Eintrag der übergebenen Operation.
    ///
    /// # Description
    /// Iteriert über [`OperationMeta::surfaces`] und erzeugt für jeden
    /// `Surface::Web { path, readonly, approval }`-Eintrag einen Adapter. Die
    /// Berechtigung wird einmalig aus [`OperationMeta::permission`]
    /// übernommen und gilt für alle erzeugten Adapter dieser Operation.
    /// Andere Surface-Varianten (`Command`, `ModelTool`, `AgentTool`) werden
    /// stillschweigend ignoriert.
    ///
    /// # Arguments
    /// - `op` (`Arc<dyn Operation>`): Die Operation, die als HTTP-Route
    ///   exponiert werden soll. Der `Arc` wird pro gefundener `Surface::Web`
    ///   mit `Arc::clone` dupliziert — die Operation selbst wird nicht
    ///   geklont.
    ///
    /// # Returns
    /// Ein `Vec<WebAdapter>` mit einem Eintrag pro `Surface::Web`-Deklaration.
    /// Operationen ohne `Surface::Web`-Eintrag ergeben einen leeren `Vec` —
    /// das ist der Mechanismus, über den `harw-web` Operationen, die keine
    /// Web-Fläche deklarieren, überhaupt nicht sieht.
    ///
    /// # Errors
    /// Keine — diese Funktion schlägt nicht fehl.
    ///
    /// # Concurrency
    /// Sicher in Multi-Thread-Umgebungen: nur lesender Zugriff auf `meta()`
    /// und atomares Inkrementieren des `Arc`-Referenzzählers per
    /// `Arc::clone`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_operations::adapter::WebAdapter;
    /// // Ops ohne Surface::Web liefern einen leeren Vec:
    /// // let adapters = WebAdapter::from_operation(Arc::new(NoSurfaceOp));
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
                if let Surface::Web {
                    path,
                    readonly,
                    approval,
                } = surface
                {
                    Some(Self {
                        op: Arc::clone(&op),
                        path,
                        readonly: *readonly,
                        approval: *approval,
                        permission,
                        operation_name,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Gibt den eingetragenen HTTP-Pfad zurück.
    ///
    /// # Description
    /// Liefert den statischen Pfad-String aus `Surface::Web { path, .. }`,
    /// z. B. `"/api/session/list"`.
    ///
    /// # Returns
    /// `&str` — der Pfad-String. Lebensdauer ist an `&self` gebunden
    /// (`'static` intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.path(), "/api/session/list");
    /// ```
    #[must_use]
    pub fn path(&self) -> &str {
        self.path
    }

    /// Gibt `true` zurück, wenn die Operation keine Nebeneffekte hat.
    ///
    /// # Description
    /// Spiegelt das `readonly`-Flag aus der `Surface::Web`-Deklaration
    /// wider. `harw-web` verwendet dieses Flag, um die zulässige
    /// HTTP-Methode zu bestimmen (`GET` für `readonly`, sonst `POST`) —
    /// dieselbe Bedeutung wie bei [`crate::adapter::ModelToolAdapter::readonly`].
    ///
    /// # Returns
    /// `bool` — `true` = ausschließlich lesend, `false` = Nebeneffekte
    /// möglich.
    ///
    /// # Concurrency
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::WebAdapter = todo!();
    /// let ro = adapter.readonly();
    /// ```
    #[must_use]
    pub fn readonly(&self) -> bool {
        self.readonly
    }

    /// Gibt die Approval-Politik dieser Route zurück.
    ///
    /// # Description
    /// Spiegelt das `approval`-Feld aus der `Surface::Web`-Deklaration
    /// wider — denselben [`ApprovalPolicy`]-Enum wie
    /// [`crate::adapter::ModelToolAdapter::approval`]. `harw-web` darf eine
    /// Route mit `approval != ApprovalPolicy::None` nicht direkt ausführen,
    /// sondern muss den Aufruf an eine Genehmigung weiterreichen.
    ///
    /// # Returns
    /// [`ApprovalPolicy`] — Copy-Typ, kein Borrow nötig.
    ///
    /// # Concurrency
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_operations::operation::ApprovalPolicy;
    /// # let adapter: harw_operations::adapter::WebAdapter = todo!();
    /// let policy = adapter.approval();
    /// assert_eq!(policy, ApprovalPolicy::None);
    /// ```
    #[must_use]
    pub fn approval(&self) -> ApprovalPolicy {
        self.approval
    }

    /// Gibt die aus `OperationMeta::permission` übernommene Mindest-Berechtigungsstufe zurück.
    ///
    /// # Description
    /// Bestimmt, welche Aufrufer-Stufe (aus der `SO_PEERCRED`-Identität von
    /// `harw-web` aufgelöst) zur Ausführung dieser Route mindestens nötig
    /// ist. Der Wert wird einmalig beim Erzeugen des Adapters aus
    /// `OperationMeta::permission` kopiert und ändert sich nicht.
    ///
    /// # Returns
    /// [`PermissionTier`] — `Copy`, keine Allokation.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::WebAdapter = todo!();
    /// let _ = adapter.permission();
    /// ```
    #[must_use]
    pub fn permission(&self) -> PermissionTier {
        self.permission
    }

    /// Gibt den maschinellen Namen der zugrundeliegenden Operation zurück.
    ///
    /// # Description
    /// Liefert [`OperationMeta::name`] (z. B. `"session.list"`).
    ///
    /// # Returns
    /// `&str` — der maschinelle Op-Name. Lebensdauer an `&self` (`'static`
    /// intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::WebAdapter = todo!();
    /// let _ = adapter.operation_name();
    /// ```
    #[must_use]
    pub fn operation_name(&self) -> &str {
        self.operation_name
    }

    /// Führt die Operation mit JSON-Argumenten aus (HTTP-Request-Rumpf).
    ///
    /// # Description
    /// Baut `OpInput` über [`OpInput::model_tool`] und delegiert vollständig
    /// an [`Operation::run`]. Ein HTTP-Aufruf erscheint der Operation damit
    /// als [`crate::operation::OpInvocation::ModelTool`] — es wird bewusst
    /// **keine** vierte `OpInvocation`-Variante eingeführt (das läge
    /// außerhalb des Schreibbereichs dieses Knotens und würde den Contract
    /// erweitern, ohne dass ein belegter Bedarf dafür bestünde). Für eine
    /// Operation ist ein automatisiert-initiierter JSON-Aufruf mit
    /// `readonly`/`approval`-Achse identisch, gleich ob er vom Modell oder
    /// über `harw-web` kommt — die Unterscheidung „wer hat aufgerufen"
    /// bleibt in der Adapter-Wahl (`WebAdapter` vs. `ModelToolAdapter`), nicht
    /// im `OpInvocation`-Typ.
    ///
    /// # Arguments
    /// - `ctx` (`&OpContext`): Ausführungs-Kontext (Session, Turn, Sandbox,
    ///   Services). `harw-web` muss ihn aus der `SO_PEERCRED`-Identität und
    ///   einer serverseitig vertrauten Autorisierung aufbauen — nie aus dem
    ///   HTTP-Request selbst.
    /// - `args` (`serde_json::Value`): JSON-Argumente aus dem
    ///   HTTP-Request-Rumpf (leer/`Null` bei `GET`).
    ///
    /// # Returns
    /// - `Ok(OpOutput)`: Die Operation wurde erfolgreich ausgeführt.
    /// - `Err(OpError)`: Fehler, der 1:1 von `Operation::run` stammt.
    ///
    /// # Errors
    /// - [`OpError::InvalidArguments`]: `args` sind syntaktisch oder
    ///   semantisch ungültig (gemeldet von der Operation).
    /// - [`OpError::Execution`]: Laufzeitfehler während der Ausführung.
    /// - [`OpError::NotAvailable`]: Operation im aktuellen Kontext nicht
    ///   verfügbar.
    ///
    /// # Concurrency
    /// `async fn`, `Send`-fähig. Der Adapter selbst hat keinen
    /// veränderlichen Zustand; mehrere gleichzeitige `invoke`-Aufrufe auf
    /// demselben Adapter sind sicher.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_operations::adapter::WebAdapter;
    /// # async fn run(adapter: &WebAdapter, ctx: &harw_operations::context::OpContext) {
    /// let result = adapter.invoke(ctx, serde_json::json!({ "limit": 10 })).await;
    /// match result {
    ///     Ok(out) => println!("{}", out.text),
    ///     Err(e) => eprintln!("Fehler: {e}"),
    /// }
    /// # }
    /// ```
    pub async fn invoke(
        &self,
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        use tracing::Instrument;
        let json_bytes = args.to_string().len();
        let span = tracing::info_span!(
            "operation.web",
            op = self.operation_name,
            path = self.path,
            readonly = self.readonly,
            json_bytes,
        );
        async move {
            let start = std::time::Instant::now();
            let input = OpInput::model_tool(args);
            let result = self.op.run(ctx, input).await;
            tracing::info!(
                duration_ms = start.elapsed().as_millis() as u64,
                status = if result.is_ok() { "ok" } else { "err" },
                "operation.web.done"
            );
            result
        }
        .instrument(span)
        .await
    }

    /// Gibt eine Referenz auf die zugrundeliegende Operation zurück.
    ///
    /// # Description
    /// Ermöglicht Introspektion oder Meta-Zugriff auf die Operation, ohne
    /// den Adapter zu konsumieren. Das `Arc` wird nicht geklont — die
    /// Referenz ist an `&self` gebunden.
    ///
    /// # Returns
    /// `&Arc<dyn Operation>` — Referenz auf den geteilten Op-Pointer.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::WebAdapter = todo!();
    /// let _ = adapter.operation().meta().name;
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

    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    use super::WebAdapter;
    use crate::context::{OpContext, ServiceMap};
    use crate::error::OpError;
    use crate::operation::{
        ApprovalPolicy, CommandVisibility, OpFuture, OpInput, OpOutput, Operation,
        OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
    };

    /// Erstellt einen minimalen [`OpContext`] für Tests.
    fn make_test_ctx() -> (OpContext, PathBuf) {
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-web-adapter-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws")).unwrap();
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .unwrap();
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
        (ctx, tmp)
    }

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
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "noop".to_owned(),
                })
            })
        }
    }

    struct SingleWebOp;
    impl Operation for SingleWebOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "single-web",
                summary: "Eine Web-Route.",
                domain: OperationDomain::Session,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::Web {
                    path: "/api/single",
                    readonly: true,
                    approval: ApprovalPolicy::None,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "single".to_owned(),
                })
            })
        }
    }

    struct TwoWebOp;
    impl Operation for TwoWebOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "two-web",
                summary: "Zwei Web-Routen.",
                domain: OperationDomain::Agents,
                permission: PermissionTier::Maintainer,
                surfaces: vec![
                    Surface::Web {
                        path: "/api/alpha",
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                    Surface::Web {
                        path: "/api/beta",
                        readonly: false,
                        approval: ApprovalPolicy::Always,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "two".to_owned(),
                })
            })
        }
    }

    struct MixedSurfaceOp;
    impl Operation for MixedSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "mixed-surface",
                summary: "Command + Web.",
                domain: OperationDomain::Execution,
                permission: PermissionTier::Owner,
                surfaces: vec![
                    Surface::Command {
                        path: "/mixed",
                        visibility: CommandVisibility::ChannelParity,
                    },
                    Surface::Web {
                        path: "/api/mixed",
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "mixed".to_owned(),
                })
            })
        }
    }

    struct EchoXOp;
    impl Operation for EchoXOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "echo-x",
                summary: "Gibt json_args['x'] als Text zurück.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Web {
                    path: "/api/echo-x",
                    readonly: false,
                    approval: ApprovalPolicy::None,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
            Box::pin(async move {
                let x = input
                    .invocation
                    .json_args()
                    .get("x")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(OpOutput { text: x.to_owned() })
            })
        }
    }

    struct AlwaysErrOp;
    impl Operation for AlwaysErrOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "always-err",
                summary: "Schlägt immer mit Execution-Fehler fehl.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Web {
                    path: "/api/always-err",
                    readonly: false,
                    approval: ApprovalPolicy::Always,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Err(OpError::Execution("simulierter Fehler".to_owned())) })
        }
    }

    #[test]
    fn test_from_operation_no_web_surface_returns_empty_vec() {
        let op: Arc<dyn Operation> = Arc::new(NoSurfaceOp);
        let adapters = WebAdapter::from_operation(op);
        assert!(
            adapters.is_empty(),
            "Op ohne Surface::Web muss leeren Vec liefern"
        );
    }

    #[test]
    fn test_from_operation_single_web_surface_returns_one_adapter_with_correct_path() {
        let op: Arc<dyn Operation> = Arc::new(SingleWebOp);
        let adapters = WebAdapter::from_operation(op);
        assert_eq!(adapters.len(), 1, "Genau ein Adapter erwartet");
        assert_eq!(adapters[0].path(), "/api/single");
        assert!(adapters[0].readonly());
        assert_eq!(adapters[0].approval(), ApprovalPolicy::None);
        assert_eq!(adapters[0].permission(), PermissionTier::Operator);
    }

    #[test]
    fn test_from_operation_two_web_surfaces_returns_two_adapters_in_order() {
        let op: Arc<dyn Operation> = Arc::new(TwoWebOp);
        let adapters = WebAdapter::from_operation(op);
        assert_eq!(adapters.len(), 2, "Zwei Adapter erwartet");
        assert_eq!(adapters[0].path(), "/api/alpha");
        assert!(adapters[0].readonly());
        assert_eq!(adapters[1].path(), "/api/beta");
        assert!(!adapters[1].readonly());
        assert_eq!(adapters[1].approval(), ApprovalPolicy::Always);
    }

    #[test]
    fn test_from_operation_mixed_surfaces_returns_only_web_adapters() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = WebAdapter::from_operation(op);
        assert_eq!(
            adapters.len(),
            1,
            "Nur der Web-Eintrag soll einen Adapter erzeugen; Command wird ignoriert"
        );
        assert_eq!(adapters[0].path(), "/api/mixed");
    }

    #[tokio::test]
    async fn test_invoke_passes_json_args_to_operation() {
        let op: Arc<dyn Operation> = Arc::new(EchoXOp);
        let adapters = WebAdapter::from_operation(op);
        let (ctx, tmp) = make_test_ctx();
        let args = serde_json::json!({ "x": "hallo-welt" });
        let result = adapters[0].invoke(&ctx, args).await;
        std::fs::remove_dir_all(tmp).ok();
        let out = result.expect("Erwartet Ok");
        assert_eq!(out.text, "hallo-welt");
    }

    #[tokio::test]
    async fn test_invoke_propagates_execution_error() {
        let op: Arc<dyn Operation> = Arc::new(AlwaysErrOp);
        let adapters = WebAdapter::from_operation(op);
        let (ctx, tmp) = make_test_ctx();
        let result = adapters[0].invoke(&ctx, serde_json::Value::Null).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::Execution(msg)) => assert_eq!(msg, "simulierter Fehler"),
            other => panic!("Erwartet OpError::Execution, war: {other:?}"),
        }
    }

    #[test]
    fn test_operation_name_returns_meta_name() {
        let op: Arc<dyn Operation> = Arc::new(SingleWebOp);
        let adapters = WebAdapter::from_operation(op);
        assert_eq!(adapters[0].operation_name(), "single-web");
    }

    #[test]
    fn test_operation_accessor_returns_arc_with_correct_meta() {
        let op: Arc<dyn Operation> = Arc::new(SingleWebOp);
        let adapters = WebAdapter::from_operation(Arc::clone(&op));
        assert_eq!(adapters[0].operation().meta().name, "single-web");
    }

    #[test]
    fn test_web_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WebAdapter>();
    }
}
